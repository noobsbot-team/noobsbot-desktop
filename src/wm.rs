//! Window management rules (E1.2): placement, magnetic edges, on-screen limits and resize geometry.
//! Pure functions on rectangles, so every rule is unit-tested; grabs.rs applies them to live windows.

use smithay::utils::{Logical, Point, Rectangle, Size};

pub type Rect = Rectangle<i32, Logical>;

/// A move starts only after this many pixels, so a plain click never nudges a window.
pub const DRAG_THRESHOLD: f64 = 4.0;
/// Edges closer than this stick together.
pub const SNAP_DISTANCE: i32 = 8;
/// This much of a window must stay on screen so it can always be grabbed again.
pub const MIN_VISIBLE: i32 = 40;
/// Floor for apps that give no minimum size, so a window can never vanish.
pub const MIN_SIZE: (i32, i32) = (200, 120);
/// An activation token older than this no longer brings a window forward.
pub const ACTIVATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Two clicks closer than this are a double-click.
pub const DOUBLE_CLICK: std::time::Duration = std::time::Duration::from_millis(400);

/// New windows open centred, slightly above the middle (golden ratio).
pub fn place(area: Rect, size: Size<i32, Logical>) -> Point<i32, Logical> {
    let x = area.loc.x + (area.size.w - size.w) / 2;
    let y = area.loc.y + ((area.size.h - size.h) as f64 * 0.382) as i32;
    (x.max(area.loc.x), y.max(area.loc.y)).into()
}

/// Moves the span `start..start + len` so whichever end is nearest an edge (within SNAP_DISTANCE) sits on it.
pub fn stick(start: i32, len: i32, edges: &[i32]) -> i32 {
    let mut best: Option<i32> = None;
    for &edge in edges {
        for offset in [edge - start, edge - (start + len)] {
            if offset.abs() <= SNAP_DISTANCE && best.is_none_or(|b| offset.abs() < b.abs()) {
                best = Some(offset);
            }
        }
    }
    start + best.unwrap_or(0)
}

/// Where a moved window lands: magnetic to the screen and other windows' edges, never lost off screen.
/// ponytail: other windows' edges count even where the windows don't overlap on the other axis.
pub fn move_to(rect: Rect, area: Rect, others: &[Rect], magnetic: bool) -> Point<i32, Logical> {
    let (mut x, mut y) = (rect.loc.x, rect.loc.y);
    if magnetic {
        let xs: Vec<i32> = [area.loc.x, area.loc.x + area.size.w].into_iter().chain(others.iter().flat_map(|o| [o.loc.x, o.loc.x + o.size.w])).collect();
        let ys: Vec<i32> = [area.loc.y, area.loc.y + area.size.h].into_iter().chain(others.iter().flat_map(|o| [o.loc.y, o.loc.y + o.size.h])).collect();
        x = stick(x, rect.size.w, &xs);
        y = stick(y, rect.size.h, &ys);
    }
    // The title bar never goes above the top of the work area, and MIN_VISIBLE px stay reachable.
    // max/min, not clamp: clamp panics when a tiny window or output makes the bounds cross.
    let x = x.max(area.loc.x - rect.size.w + MIN_VISIBLE).min(area.loc.x + area.size.w - MIN_VISIBLE);
    let y = y.max(area.loc.y).min(area.loc.y + area.size.h - MIN_VISIBLE);
    (x, y).into()
}

/// Where a window dragged against a screen edge tiles (macOS 15): its half at a side, its quarter at a side's top or
/// bottom quarter, the whole work area (zoom) at the top edge or on the menu bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tile {
    Zoom,
    Part(Rect),
}

impl Tile {
    /// The rectangle the window will fill, for the preview.
    pub fn rect(self, area: Rect) -> Rect {
        match self {
            Tile::Zoom => area,
            Tile::Part(rect) => rect,
        }
    }
}

/// The pointer is this close to a side for a tile.
pub const TILE_EDGE: f64 = 3.0;

/// The tile for a window dragged with the pointer at `p`, if any.
pub fn tile_at(p: Point<f64, Logical>, area: Rect) -> Option<Tile> {
    let (x, y, w, h) = (f64::from(area.loc.x), f64::from(area.loc.y), f64::from(area.size.w), f64::from(area.size.h));
    let (left, right) = (p.x <= x + TILE_EDGE, p.x >= x + w - 1.0 - TILE_EDGE);
    if !left && !right {
        return (p.y <= y + TILE_EDGE).then_some(Tile::Zoom);
    }
    let (half_w, half_h) = (area.size.w / 2, area.size.h / 2);
    let col = if left { area.loc.x } else { area.loc.x + half_w };
    let width = if left { half_w } else { area.size.w - half_w };
    let rect = if p.y < y + h / 4.0 {
        Rect::new((col, area.loc.y).into(), (width, half_h).into())
    } else if p.y > y + h * 3.0 / 4.0 {
        Rect::new((col, area.loc.y + half_h).into(), (width, area.size.h - half_h).into())
    } else {
        Rect::new((col, area.loc.y).into(), (width, area.size.h).into())
    };
    Some(Tile::Part(rect))
}

/// Which edges a resize drags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Edges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct ResizeMods {
    /// Alt: grow from the centre.
    pub symmetric: bool,
    /// Shift: keep the aspect ratio.
    pub keep_aspect: bool,
}

/// The size a resize asks for, given the window at grab start and how far the pointer moved.
pub fn resize_size(
    start: Rect,
    edges: Edges,
    delta: Point<f64, Logical>,
    mods: ResizeMods,
    min: Size<i32, Logical>,
    max: Size<i32, Logical>,
) -> Size<i32, Logical> {
    let (dx, dy) = (delta.x.round() as i32, delta.y.round() as i32);
    let mut dw = if edges.left {
        -dx
    } else if edges.right {
        dx
    } else {
        0
    };
    let mut dh = if edges.top {
        -dy
    } else if edges.bottom {
        dy
    } else {
        0
    };
    if mods.symmetric {
        dw *= 2;
        dh *= 2;
    }
    let (mut w, mut h) = (start.size.w + dw, start.size.h + dh);

    if mods.keep_aspect && start.size.w > 0 && start.size.h > 0 {
        let ratio = start.size.w as f64 / start.size.h as f64;
        let horizontal = edges.left || edges.right;
        let vertical = edges.top || edges.bottom;
        let width_leads = horizontal && (!vertical || w as f64 / start.size.w as f64 >= h as f64 / start.size.h as f64);
        if width_leads {
            h = (w as f64 / ratio).round() as i32;
        } else {
            w = (h as f64 * ratio).round() as i32;
        }
    }

    let (min_w, min_h) = if min.w <= 0 && min.h <= 0 { MIN_SIZE } else { (min.w.max(1), min.h.max(1)) };
    let w = w.max(min_w).min(if max.w > 0 { max.w } else { i32::MAX });
    let h = h.max(min_h).min(if max.h > 0 { max.h } else { i32::MAX });
    (w, h).into()
}

/// Where a window of `size` goes during a resize: pinned to the edge opposite the dragged one (or to the
/// centre for Alt), so content never jumps while a slow app catches up.
pub fn resize_anchor(start: Rect, edges: Edges, symmetric: bool, size: Size<i32, Logical>) -> Point<i32, Logical> {
    let x = if symmetric && (edges.left || edges.right) {
        start.loc.x + (start.size.w - size.w) / 2
    } else if edges.left {
        start.loc.x + start.size.w - size.w
    } else {
        start.loc.x
    };
    let y = if symmetric && (edges.top || edges.bottom) {
        start.loc.y + (start.size.h - size.h) / 2
    } else if edges.top {
        start.loc.y + start.size.h - size.h
    } else {
        start.loc.y
    };
    (x, y).into()
}

/// Invisible resize zones reach this far outside every window edge...
pub const ZONE_OUTSIDE: f64 = 8.0;
/// ...and this far into the window.
pub const ZONE_INSIDE: f64 = 4.0;
/// Corner zones are this big (centred on the corner) and resize diagonally.
pub const ZONE_CORNER: f64 = 16.0;

/// The edges the resize zone at `p` drags, or None outside every zone of the window at `rect`.
pub fn zone_edges(rect: Rect, p: Point<f64, Logical>) -> Option<Edges> {
    let (l, t) = (rect.loc.x as f64, rect.loc.y as f64);
    let (r, b) = (l + rect.size.w as f64, t + rect.size.h as f64);
    let beyond = p.x < l - ZONE_OUTSIDE || p.x >= r + ZONE_OUTSIDE || p.y < t - ZONE_OUTSIDE || p.y >= b + ZONE_OUTSIDE;
    let inside = p.x >= l + ZONE_INSIDE && p.x < r - ZONE_INSIDE && p.y >= t + ZONE_INSIDE && p.y < b - ZONE_INSIDE;
    if beyond || inside {
        return None;
    }
    let reach = ZONE_CORNER / 2.0;
    let (in_left, in_right, in_top, in_bottom) = (p.x < l + ZONE_INSIDE, p.x >= r - ZONE_INSIDE, p.y < t + ZONE_INSIDE, p.y >= b - ZONE_INSIDE);
    let (horizontal, vertical) = (in_left || in_right, in_top || in_bottom);
    // A tiny window can be in both opposite bands; left and top win.
    let left = in_left || (vertical && p.x < l + reach);
    let right = !left && (in_right || (vertical && p.x >= r - reach));
    let top = in_top || (horizontal && p.y < t + reach);
    let bottom = !top && (in_bottom || (horizontal && p.y >= b - reach));
    Some(Edges { left, right, top, bottom })
}

/// Double-clicking an edge zone: the window grows to the work-area edge in that direction only (never shrinks).
pub fn grow_to(rect: Rect, area: Rect, edges: Edges) -> Rect {
    let (mut l, mut t, mut r, mut b) = (rect.loc.x, rect.loc.y, rect.loc.x + rect.size.w, rect.loc.y + rect.size.h);
    if edges.left {
        l = l.min(area.loc.x);
    }
    if edges.right {
        r = r.max(area.loc.x + area.size.w);
    }
    if edges.top {
        t = t.min(area.loc.y);
    }
    if edges.bottom {
        b = b.max(area.loc.y + area.size.h);
    }
    Rectangle::new((l, t).into(), (r - l, b - t).into())
}

/// The corner of `rect` nearest `p`, which a modifier + right-drag resizes from.
pub fn nearest_corner(rect: Rect, p: Point<f64, Logical>) -> Edges {
    let left = p.x < f64::from(rect.loc.x) + f64::from(rect.size.w) / 2.0;
    let top = p.y < f64::from(rect.loc.y) + f64::from(rect.size.h) / 2.0;
    Edges { left, right: !left, top, bottom: !top }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_corner_by_quadrant() {
        let rect = Rect::new((100, 100).into(), (200, 100).into());
        assert_eq!(nearest_corner(rect, (110.0, 110.0).into()), Edges { left: true, top: true, ..Default::default() });
        assert_eq!(nearest_corner(rect, (290.0, 190.0).into()), Edges { right: true, bottom: true, ..Default::default() });
        assert_eq!(nearest_corner(rect, (250.0, 120.0).into()), Edges { right: true, top: true, ..Default::default() });
    }

    #[test]
    fn tiles_follow_the_pointer_to_an_edge() {
        let area = Rect::new((0, 24).into(), (1000, 800).into());
        let at = |x: f64, y: f64| tile_at((x, y).into(), area);
        assert_eq!(at(500.0, 400.0), None);
        assert_eq!(at(500.0, 10.0), Some(Tile::Zoom));
        assert_eq!(at(0.0, 400.0), Some(Tile::Part(Rect::new((0, 24).into(), (500, 800).into()))));
        assert_eq!(at(999.0, 400.0), Some(Tile::Part(Rect::new((500, 24).into(), (500, 800).into()))));
        assert_eq!(at(0.0, 30.0), Some(Tile::Part(Rect::new((0, 24).into(), (500, 400).into()))));
        assert_eq!(at(999.0, 820.0), Some(Tile::Part(Rect::new((500, 424).into(), (500, 400).into()))));
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rectangle::new((x, y).into(), (w, h).into())
    }
    const NONE: ResizeMods = ResizeMods { symmetric: false, keep_aspect: false };
    const NO_HINT: (i32, i32) = (0, 0);

    #[test]
    fn move_on_tiny_output_does_not_panic() {
        move_to(rect(5, 5, 1, 1), rect(0, 0, 30, 20), &[], true);
    }

    #[test]
    fn places_centred_above_middle() {
        let p = place(rect(0, 0, 1000, 1000), (400, 200).into());
        assert_eq!((p.x, p.y), (300, 305));
        let big = place(rect(0, 0, 100, 100), (400, 400).into());
        assert_eq!((big.x, big.y), (0, 0), "oversized windows start at the corner");
    }

    #[test]
    fn sticks_nearest_edge_within_distance() {
        assert_eq!(stick(5, 100, &[0, 500]), 0, "start end sticks");
        assert_eq!(stick(395, 100, &[0, 500]), 400, "far end sticks");
        assert_eq!(stick(20, 100, &[0, 500]), 20, "too far: free");
        assert_eq!(stick(9, 100, &[0, 101]), 1, "nearest of two wins");
    }

    #[test]
    fn move_sticks_and_stays_reachable() {
        let area = rect(0, 0, 1000, 800);
        assert_eq!(move_to(rect(6, 300, 200, 100), area, &[], true), (0, 300).into());
        assert_eq!(move_to(rect(6, 300, 200, 100), area, &[], false), (6, 300).into(), "Ctrl: no sticking");
        assert_eq!(move_to(rect(500, 300, 200, 100), area, &[rect(704, 0, 100, 100)], true), (504, 300).into(), "sticks to a neighbour");
        assert_eq!(move_to(rect(-500, -50, 200, 100), area, &[], true), (-160, 0).into(), "40 px stay on screen, top never above");
        assert_eq!(move_to(rect(2000, 2000, 200, 100), area, &[], true), (960, 760).into());
    }

    #[test]
    fn left_edge_resize_keeps_right_edge_pinned() {
        let start = rect(100, 100, 400, 300);
        let edges = Edges { left: true, ..Default::default() };
        let size = resize_size(start, edges, (-50.0, 0.0).into(), NONE, NO_HINT.into(), NO_HINT.into());
        assert_eq!(size, (450, 300).into());
        // The app is slow and still shows its old 400 px buffer: it stays pinned to the right edge.
        assert_eq!(resize_anchor(start, edges, false, (400, 300).into()).x, 100);
        assert_eq!(resize_anchor(start, edges, false, size).x, 50);
    }

    #[test]
    fn alt_grows_from_centre_and_shift_keeps_ratio() {
        let start = rect(100, 100, 400, 200);
        let right = Edges { right: true, ..Default::default() };
        let size = resize_size(start, right, (50.0, 0.0).into(), ResizeMods { symmetric: true, keep_aspect: false }, NO_HINT.into(), NO_HINT.into());
        assert_eq!(size, (500, 200).into());
        assert_eq!(resize_anchor(start, right, true, size), (50, 100).into());

        let corner = Edges { right: true, bottom: true, ..Default::default() };
        let size = resize_size(start, corner, (200.0, 10.0).into(), ResizeMods { symmetric: false, keep_aspect: true }, NO_HINT.into(), NO_HINT.into());
        assert_eq!(size, (600, 300).into());
    }

    #[test]
    fn respects_hints_and_floor() {
        let start = rect(0, 0, 400, 300);
        let br = Edges { right: true, bottom: true, ..Default::default() };
        let tiny = resize_size(start, br, (-1000.0, -1000.0).into(), NONE, NO_HINT.into(), NO_HINT.into());
        assert_eq!(tiny, MIN_SIZE.into(), "no hints: floor");
        let hinted = resize_size(start, br, (-1000.0, 1000.0).into(), NONE, (300, 100).into(), (0, 500).into());
        assert_eq!(hinted, (300, 500).into(), "app min and max win");
    }

    #[test]
    fn zones_hug_every_edge_and_corner() {
        let win = rect(100, 100, 400, 300);
        let at = |x: f64, y: f64| zone_edges(win, (x, y).into());
        let e = |left, right, top, bottom| Some(Edges { left, right, top, bottom });
        assert_eq!(at(93.0, 250.0), e(true, false, false, false), "8 px outside the left edge");
        assert_eq!(at(103.0, 250.0), e(true, false, false, false), "outer 4 px inside");
        assert_eq!(at(91.0, 250.0), None, "too far out");
        assert_eq!(at(105.0, 250.0), None, "the window's own content");
        assert_eq!(at(300.0, 405.0), e(false, false, false, true));
        assert_eq!(at(505.0, 250.0), e(false, true, false, false));
        assert_eq!(at(95.0, 105.0), e(true, false, true, false), "top-left corner");
        assert_eq!(at(104.0, 95.0), e(true, false, true, false), "16 px corner reaches along the top");
        assert_eq!(at(110.0, 95.0), e(false, false, true, false), "past the corner: top only");
        assert_eq!(at(505.0, 395.0), e(false, true, false, true), "bottom-right corner");
    }

    #[test]
    fn double_click_grows_one_direction_only() {
        let area = rect(0, 0, 1000, 800);
        let left = Edges { left: true, ..Default::default() };
        assert_eq!(grow_to(rect(100, 100, 400, 300), area, left), rect(0, 100, 500, 300));
        let br = Edges { right: true, bottom: true, ..Default::default() };
        assert_eq!(grow_to(rect(100, 100, 400, 300), area, br), rect(100, 100, 900, 700));
        assert_eq!(grow_to(rect(-50, 100, 400, 300), area, left), rect(-50, 100, 400, 300), "never shrinks");
    }
}
