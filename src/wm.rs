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

/// New windows open centred, slightly above the middle (golden ratio).
pub fn place(area: Rect, size: Size<i32, Logical>) -> Point<i32, Logical> {
    let x = area.loc.x + (area.size.w - size.w) / 2;
    let y = area.loc.y + ((area.size.h - size.h) as f64 * 0.382) as i32;
    (x.max(area.loc.x), y.max(area.loc.y)).into()
}

/// `rect` moved from work area `from` to `to`: the same place relative to it, its size kept where it fits, else
/// shrunk to `to`, and wholly inside it.
pub fn fit_in(rect: Rect, from: Rect, to: Rect) -> Rect {
    let size = Size::from((rect.size.w.min(to.size.w), rect.size.h.min(to.size.h)));
    let loc = to.loc + (rect.loc - from.loc);
    let x = loc.x.min(to.loc.x + to.size.w - size.w).max(to.loc.x);
    let y = loc.y.min(to.loc.y + to.size.h - size.h).max(to.loc.y);
    Rect::new((x, y).into(), size)
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

/// Where a moved window lands: magnetic to the screen and other windows' edges, never lost off screen. Another
/// window's side counts only beside it: their spans on the other axis overlap, or will once that axis sticks.
pub fn move_to(rect: Rect, area: Rect, others: &[Rect], magnetic: bool) -> Point<i32, Logical> {
    let (mut x, mut y) = (rect.loc.x, rect.loc.y);
    if magnetic {
        let near = |a: i32, a_len: i32, b: i32, b_len: i32| a <= b + b_len + SNAP_DISTANCE && b <= a + a_len + SNAP_DISTANCE;
        let beside = others.iter().filter(|o| near(rect.loc.y, rect.size.h, o.loc.y, o.size.h));
        let above = others.iter().filter(|o| near(rect.loc.x, rect.size.w, o.loc.x, o.size.w));
        let xs: Vec<i32> = [area.loc.x, area.loc.x + area.size.w].into_iter().chain(beside.flat_map(|o| [o.loc.x, o.loc.x + o.size.w])).collect();
        let ys: Vec<i32> = [area.loc.y, area.loc.y + area.size.h].into_iter().chain(above.flat_map(|o| [o.loc.y, o.loc.y + o.size.h])).collect();
        x = stick(x, rect.size.w, &xs);
        y = stick(y, rect.size.h, &ys);
    }
    // The title bar never goes above the top of the work area, and MIN_VISIBLE px stay reachable.
    // max/min, not clamp: clamp panics when a tiny window or output makes the bounds cross.
    let x = x.max(area.loc.x - rect.size.w + MIN_VISIBLE).min(area.loc.x + area.size.w - MIN_VISIBLE);
    let y = y.max(area.loc.y).min(area.loc.y + area.size.h - MIN_VISIBLE);
    (x, y).into()
}

/// Where a window dragged against a screen edge tiles: its half at a side, its quarter at a side's top or
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

/// The tile for a window dragged with the pointer at `p`, if any. A side with another of `screens` just beyond it is
/// for crossing over, not tiling.
pub fn tile_at(p: Point<f64, Logical>, area: Rect, screens: &[Rect]) -> Option<Tile> {
    let (x, y, w, h) = (f64::from(area.loc.x), f64::from(area.loc.y), f64::from(area.size.w), f64::from(area.size.h));
    let (left, right) = (p.x <= x + TILE_EDGE, p.x >= x + w - 1.0 - TILE_EDGE);
    if !left && !right {
        return (p.y <= y + TILE_EDGE).then_some(Tile::Zoom);
    }
    let beyond = Point::from((p.x + if left { -1.0 } else { 1.0 } * (TILE_EDGE + 1.0), p.y));
    if screens.iter().any(|s| !s.to_f64().contains(p) && s.to_f64().contains(beyond)) {
        return None;
    }
    let (top, bottom) = (p.y < y + h / 4.0, p.y > y + h * 3.0 / 4.0);
    Some(Tile::Part(tile_part(area, Edges { left, right: !left, top, bottom })))
}

/// The part of the work area against `edges`: one side is a half, two a quarter; none (or opposite ones) the whole.
pub fn tile_part(area: Rect, edges: Edges) -> Rect {
    let span = |start: i32, len: i32, first: bool, second: bool| match (first, second) {
        (true, false) => (start, len / 2),
        (false, true) => (start + len / 2, len - len / 2),
        _ => (start, len),
    };
    let (x, w) = span(area.loc.x, area.size.w, edges.left, edges.right);
    let (y, h) = span(area.loc.y, area.size.h, edges.top, edges.bottom);
    Rect::new((x, y).into(), (w, h).into())
}

/// Window ▸ Center: the middle of the work area, never above or left of it.
pub fn center(area: Rect, size: Size<i32, Logical>) -> Point<i32, Logical> {
    let x = area.loc.x + (area.size.w - size.w) / 2;
    let y = area.loc.y + (area.size.h - size.h) / 2;
    (x.max(area.loc.x), y.max(area.loc.y)).into()
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

/// A keyboard nudge or resize step (E1.2), in px.
pub const NUDGE: i32 = 16;

/// `rect` moved `dx`, `dy` keyboard steps, kept reachable as a dragged window is (no magnetic edges).
pub fn nudge(rect: Rect, area: Rect, dx: i32, dy: i32) -> Point<i32, Logical> {
    move_to(Rect::new(rect.loc + Point::from((dx * NUDGE, dy * NUDGE)), rect.size), area, &[], false)
}

/// `rect`'s size with its right and bottom edges moved `dx`, `dy` keyboard steps (out when positive), within the app's
/// `min` / `max`; growing stops at the work area's right and bottom edges.
pub fn nudge_size(rect: Rect, area: Rect, dx: i32, dy: i32, min: Size<i32, Logical>, max: Size<i32, Logical>) -> Size<i32, Logical> {
    let edges = Edges { right: true, bottom: true, ..Default::default() };
    let delta = Point::from((f64::from(dx * NUDGE), f64::from(dy * NUDGE)));
    let mut size = resize_size(rect, edges, delta, ResizeMods { symmetric: false, keep_aspect: false }, min, max);
    // A window already past the edge isn't pulled in.
    if dx > 0 {
        size.w = size.w.min((area.loc.x + area.size.w - rect.loc.x).max(rect.size.w));
    }
    if dy > 0 {
        size.h = size.h.min((area.loc.y + area.size.h - rect.loc.y).max(rect.size.h));
    }
    size
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
/// A sheet (a modal dialog, E8.5) hangs centred just under its parent's title bar: `parent` is the parent's frame,
/// `bar` its title-bar height, `size` the sheet's frame size.
pub fn sheet_loc(parent: Rect, bar: i32, size: Size<i32, Logical>) -> Point<i32, Logical> {
    (parent.loc.x + (parent.size.w - size.w) / 2, parent.loc.y + bar).into()
}

/// The stacking order (bottom to top) with every sheet directly above its parent; `parent` gives a sheet's parent.
pub fn sheets_above<T: Clone + PartialEq>(order: &[T], parent: impl Fn(&T) -> Option<T>) -> Vec<T> {
    let mut out = Vec::with_capacity(order.len());
    for root in order.iter().filter(|w| parent(w).is_none_or(|p| !order.contains(&p))) {
        let mut stack = vec![root.clone()];
        while let Some(w) = stack.pop() {
            if out.contains(&w) {
                continue;
            }
            stack.extend(order.iter().rev().filter(|s| parent(s).as_ref() == Some(&w)).cloned());
            out.push(w);
        }
    }
    out
}

/// Where an app's remembered frame (E8.5) opens on a display with work area `area`: as it was, kept reachable by the
/// move rules; `None` when it no longer fits (then the window is placed as new).
pub fn recall(saved: Rect, area: Rect) -> Option<Rect> {
    (saved.size.w <= area.size.w && saved.size.h <= area.size.h && saved.size.w > 0 && saved.size.h > 0)
        .then(|| Rect::new(move_to(saved, area, &[], false), saved.size))
}

/// The remembered-frames file: one `app-id<TAB>display<TAB>x y w h` per line.
pub fn parse_frames(text: &str) -> Vec<(String, String, Rect)> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let (app, display, nums) = (parts.next()?, parts.next()?, parts.next()?);
            let n: Vec<i32> = nums.split(' ').map(str::parse).collect::<Result<_, _>>().ok()?;
            let [x, y, w, h] = n[..] else { return None };
            Some((app.to_owned(), display.to_owned(), Rect::new((x, y).into(), (w, h).into())))
        })
        .collect()
}

pub fn format_frames(frames: &[(String, String, Rect)]) -> String {
    frames.iter().map(|(app, display, r)| format!("{app}\t{display}\t{} {} {} {}\n", r.loc.x, r.loc.y, r.size.w, r.size.h)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_in_keeps_place_and_size_where_it_fits() {
        let a = Rect::new((0, 30).into(), (1920, 1050).into());
        let b = Rect::new((1920, 0).into(), (1280, 800).into());
        let r = |x, y, w, h| Rect::new((x, y).into(), (w, h).into());
        assert_eq!(fit_in(r(100, 130, 400, 300), a, b), r(2020, 100, 400, 300), "same place, same size");
        assert_eq!(fit_in(r(1500, 900, 400, 300), a, b), r(2800, 500, 400, 300), "pushed back inside");
        assert_eq!(fit_in(r(10, 40, 1800, 1000), a, b), b, "too big: shrunk to the area");
    }

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
        let at = |x: f64, y: f64| tile_at((x, y).into(), area, &[]);
        assert_eq!(at(500.0, 400.0), None);
        assert_eq!(at(500.0, 10.0), Some(Tile::Zoom));
        assert_eq!(at(0.0, 400.0), Some(Tile::Part(Rect::new((0, 24).into(), (500, 800).into()))));
        assert_eq!(at(999.0, 400.0), Some(Tile::Part(Rect::new((500, 24).into(), (500, 800).into()))));
        assert_eq!(at(0.0, 30.0), Some(Tile::Part(Rect::new((0, 24).into(), (500, 400).into()))));
        assert_eq!(at(999.0, 820.0), Some(Tile::Part(Rect::new((500, 424).into(), (500, 400).into()))));
        // Another display to the right: that side is for crossing over; the left one still tiles.
        let screens = [Rect::new((0, 0).into(), (1000, 824).into()), Rect::new((1000, 0).into(), (800, 600).into())];
        assert_eq!(tile_at((999.0, 400.0).into(), area, &screens), None);
        assert_eq!(tile_at((999.0, 700.0).into(), area, &screens), Some(Tile::Part(Rect::new((500, 424).into(), (500, 400).into()))), "below it");
        assert_eq!(tile_at((0.0, 400.0).into(), area, &screens), at(0.0, 400.0));
    }

    #[test]
    fn keyboard_tiles_and_centre() {
        let area = rect(0, 24, 1001, 800);
        let e = |left, right, top, bottom| tile_part(area, Edges { left, right, top, bottom });
        assert_eq!(e(true, false, false, false), rect(0, 24, 500, 800));
        assert_eq!(e(false, true, false, false), rect(500, 24, 501, 800), "the odd pixel goes right");
        assert_eq!(e(false, false, true, false), rect(0, 24, 1001, 400));
        assert_eq!(e(false, true, false, true), rect(500, 424, 501, 400));
        assert_eq!(e(false, false, false, false), area);
        assert_eq!(center(area, (401, 200).into()), (300, 324).into());
        assert_eq!(center(area, (2000, 2000).into()), (0, 24).into(), "too big: the corner");
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
        assert_eq!(move_to(rect(500, 300, 200, 100), area, &[rect(704, 250, 100, 100)], true), (504, 300).into(), "sticks to a neighbour");
        assert_eq!(move_to(rect(500, 300, 200, 100), area, &[rect(704, 0, 100, 100)], true), (500, 300).into(), "not beside it: free");
        assert_eq!(move_to(rect(500, 300, 200, 100), area, &[rect(704, 195, 100, 100)], true), (504, 295).into(), "corners 5 px apart: both stick");
        assert_eq!(move_to(rect(500, 300, 200, 100), area, &[rect(100, 404, 200, 100)], true), (500, 300).into(), "below but off to the side: free");
        assert_eq!(move_to(rect(-500, -50, 200, 100), area, &[], true), (-160, 0).into(), "40 px stay on screen, top never above");
        assert_eq!(move_to(rect(2000, 2000, 200, 100), area, &[], true), (960, 760).into());
    }

    #[test]
    fn a_move_clamps_to_the_display_under_the_pointer() {
        let (main, right, left) = (rect(0, 24, 1920, 1056), rect(1920, 24, 1280, 776), rect(-1920, 24, 1920, 1056));
        let window = |x, y| rect(x, y, 400, 300);
        assert_eq!(move_to(window(2500, 300), main, &[], false), (1880, 300).into(), "the first display alone stops it");
        assert_eq!(move_to(window(2500, 300), right, &[], false), (2500, 300).into(), "on the right one it goes on");
        assert_eq!(move_to(window(3500, 900), right, &[], false), (3160, 760).into(), "40 px stay on the right one");
        assert_eq!(move_to(window(-1500, 300), left, &[], false), (-1500, 300).into(), "a negative origin");
        assert_eq!(move_to(window(-2500, 0), left, &[], false), (-2280, 24).into(), "title bar kept below its menu bar");
        assert_eq!(move_to(window(1915, 300), right, &[], true), (1920, 300).into(), "its edge is magnetic");
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
    fn keyboard_steps_move_and_resize_16_px() {
        let (area, w, free) = (rect(0, 24, 1000, 800), rect(100, 100, 400, 300), Size::from(NO_HINT));
        assert_eq!(nudge(w, area, 1, 0), (116, 100).into());
        assert_eq!(nudge(w, area, 0, -1), (100, 84).into());
        assert_eq!(nudge(rect(100, 30, 400, 300), area, 0, -1), (100, 24).into(), "title bar stays below the menu bar");
        assert_eq!(nudge(rect(955, 100, 400, 300), area, 1, 0), (960, 100).into(), "40 px stay on screen");
        assert_eq!(nudge_size(w, area, 1, 0, free, free), (416, 300).into());
        assert_eq!(nudge_size(w, area, 0, -1, free, free), (400, 284).into());
        assert_eq!(nudge_size(rect(100, 100, 200, 300), area, -1, 0, free, free), (200, 300).into(), "no hints: floor");
        assert_eq!(nudge_size(w, area, 1, 0, free, (410, 0).into()), (410, 300).into(), "app max");
        assert_eq!(nudge_size(w, area, 0, -1, (0, 290).into(), free), (400, 290).into(), "app min");
        assert_eq!(nudge_size(rect(100, 100, 890, 300), area, 1, 0, free, free), (900, 300).into(), "stops at the work area");
        assert_eq!(nudge_size(rect(700, 100, 400, 300), area, 1, 0, free, free), (400, 300).into(), "already past: kept");
        assert_eq!(nudge_size(rect(700, 100, 400, 300), area, -1, 0, free, free), (384, 300).into(), "and can still shrink");
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

    #[test]
    fn sheet_hangs_centred_under_the_title_bar() {
        let p = sheet_loc(rect(100, 50, 800, 600), 28, (400, 200).into());
        assert_eq!((p.x, p.y), (300, 78));
    }

    #[test]
    fn sheets_stack_directly_above_their_parent() {
        // 1 is a sheet of 3, 4 of 1, 5 of a window that is gone.
        let parent = |w: &i32| match w {
            1 => Some(3),
            4 => Some(1),
            5 => Some(9),
            _ => None,
        };
        assert_eq!(sheets_above(&[1, 2, 3, 4, 5, 6], parent), vec![2, 3, 1, 4, 5, 6]);
    }

    #[test]
    fn recalled_frame_is_clamped_or_dropped() {
        let area = rect(0, 24, 1000, 800);
        assert_eq!(recall(rect(100, 100, 400, 300), area), Some(rect(100, 100, 400, 300)));
        // Off the right edge and above the menu bar: pulled back so it can be grabbed.
        assert_eq!(recall(rect(2000, 0, 400, 300), area), Some(rect(960, 24, 400, 300)));
        assert_eq!(recall(rect(0, 24, 1200, 300), area), None);
    }

    #[test]
    fn frames_file_round_trips() {
        let frames = vec![("org.app.Foo".to_owned(), "DP-1".to_owned(), rect(-10, 40, 640, 480)), ("bar".to_owned(), String::new(), rect(0, 0, 1, 2))];
        assert_eq!(parse_frames(&format_frames(&frames)), frames);
        assert!(parse_frames("junk\nx\ty\t1 2 3\n").is_empty());
    }
}
