//! Display arrangement (E6.2): displays as rectangles that snap edge to edge onto the others. Shared by both crates
//! (`#[path]` in settings, like `display_info.rs`): the Displays page drags with it, `displays::plan` re-snaps with
//! it after a scale, mode or rotation changed a display's size.

use crate::display_info::Display;

/// A display's place in logical px.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Area {
    pub fn of(d: &Display) -> Area {
        Area { x: d.x, y: d.y, w: d.width.max(1), h: d.height.max(1) }
    }

    pub fn overlaps(self, o: Area) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }
}

/// Where `moved` lands when dropped: touching one of `others` edge to edge (never apart, never overlapping when it
/// can help it), at the place nearest the drop; along the shared edge, its ends snap to the other's ends or centre
/// within `pull` px.
pub fn snap(moved: Area, others: &[Area], pull: i32) -> (i32, i32) {
    let along = |pos: i32, len: i32, o_pos: i32, o_len: i32| {
        // At least a little of the edge is shared.
        let pos = pos.clamp(o_pos - len + 1, o_pos + o_len - 1);
        [o_pos, o_pos + o_len - len, o_pos + (o_len - len) / 2].into_iter().find(|p| (p - pos).abs() <= pull).unwrap_or(pos)
    };
    let mut spots = Vec::new();
    for o in others {
        let y = along(moved.y, moved.h, o.y, o.h);
        let x = along(moved.x, moved.w, o.x, o.w);
        spots.extend([(o.x + o.w, y), (o.x - moved.w, y), (x, o.y + o.h), (x, o.y - moved.h)]);
    }
    let distance = |&(x, y): &(i32, i32)| i64::from(x - moved.x).pow(2) + i64::from(y - moved.y).pow(2);
    let free = |&(x, y): &(i32, i32)| !others.iter().any(|o| Area { x, y, ..moved }.overlaps(*o));
    spots.iter().copied().filter(free).min_by_key(distance).or_else(|| spots.iter().copied().min_by_key(distance)).unwrap_or((moved.x, moved.y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(x: i32, y: i32, w: i32, h: i32) -> Area {
        Area { x, y, w, h }
    }

    #[test]
    fn dropped_displays_meet_an_edge() {
        let laptop = area(0, 0, 1536, 960);
        let monitor = |x, y| area(x, y, 2560, 1440);
        assert_eq!(snap(monitor(1700, 30), &[laptop], 40), (1536, 0), "a gap closes; tops line up within reach");
        assert_eq!(snap(monitor(1400, 300), &[laptop], 40), (1536, 300), "an overlap is pushed out to the edge");
        assert_eq!(snap(monitor(-2700, -500), &[laptop], 40), (-2560, -480), "bottoms line up within reach");
        assert_eq!(snap(monitor(-2500, -2000), &[laptop], 40), (-2500, -1440), "above, sharing part of the edge");
        assert_eq!(snap(monitor(5000, 5000), &[laptop], 40), (1535, 960), "far away: the nearest spot, still touching");
        assert_eq!(snap(area(10, 1000, 1536, 960), &[area(0, 0, 1536, 960)], 40), (0, 960), "left edges line up below");
        assert_eq!(snap(area(500, 980, 500, 300), &[laptop], 40), (518, 960), "centred under it within reach");
        assert_eq!(snap(monitor(1536, 0), &[laptop], 0), (1536, 0), "already touching: stays");
    }

    #[test]
    fn snapping_avoids_a_third_display() {
        let a = area(0, 0, 1000, 1000);
        let b = area(1000, 0, 1000, 1000);
        // Right of `a` is taken by `b`; the next best spot that overlaps neither.
        let (x, y) = snap(area(900, 50, 1000, 1000), &[a, b], 40);
        assert!(![a, b].iter().any(|o| area(x, y, 1000, 1000).overlaps(*o)), "{x},{y}");
    }
}
