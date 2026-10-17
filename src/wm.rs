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
    let x = x.clamp(area.loc.x - rect.size.w + MIN_VISIBLE, area.loc.x + area.size.w - MIN_VISIBLE);
    let y = y.clamp(area.loc.y, area.loc.y + area.size.h - MIN_VISIBLE);
    (x, y).into()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rectangle::new((x, y).into(), (w, h).into())
    }
    const NONE: ResizeMods = ResizeMods { symmetric: false, keep_aspect: false };
    const NO_HINT: (i32, i32) = (0, 0);

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
}
