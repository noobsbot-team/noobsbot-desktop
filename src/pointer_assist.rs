//! Pointer accessibility (E8.6): shake to find the pointer.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use smithay::utils::{Logical, Point};

use crate::state::State;

/// A shake: this many direction changes, covering this distance, within this time.
const SHAKE_TURNS: usize = 4;
const SHAKE_DISTANCE: f64 = 600.0;
const SHAKE_WINDOW: Duration = Duration::from_millis(500);
/// The shaken pointer grows this much, eases in and out, and stays big this long after the last shake.
pub const SHAKE_GROW: f64 = 4.0;
const GROW_IN: Duration = Duration::from_millis(150);
const GROW_HOLD: Duration = Duration::from_millis(1000);
const GROW_OUT: Duration = Duration::from_millis(300);

fn smoothstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Spots a quick back-and-forth and grows the pointer for a moment.
#[derive(Default)]
pub struct Shake {
    steps: VecDeque<(Instant, Point<f64, Logical>)>,
    /// When the pointer started growing and when it was last shaken.
    grown: Option<(Instant, Instant)>,
}

impl Shake {
    /// A pointer step of `delta`; true when it completes a shake.
    pub fn motion(&mut self, now: Instant, delta: Point<f64, Logical>) -> bool {
        self.steps.push_back((now, delta));
        while self.steps.front().is_some_and(|(t, _)| now.duration_since(*t) > SHAKE_WINDOW) {
            self.steps.pop_front();
        }
        // Direction changes along one axis, ignoring sub-pixel jitter.
        let turns = |axis: fn(&Point<f64, Logical>) -> f64| {
            let signs: Vec<bool> = self.steps.iter().map(|(_, d)| axis(d)).filter(|v| v.abs() >= 1.0).map(|v| v > 0.0).collect();
            signs.windows(2).filter(|w| w[0] != w[1]).count()
        };
        let distance: f64 = self.steps.iter().map(|(_, d)| d.x.hypot(d.y)).sum();
        let shaking = turns(|d| d.x).max(turns(|d| d.y)) >= SHAKE_TURNS && distance >= SHAKE_DISTANCE;
        if shaking {
            let start = self.grown.filter(|_| self.scale(now) > 1.0).map_or(now, |(start, _)| start);
            self.grown = Some((start, now));
        }
        shaking
    }

    /// The pointer's size factor at `now`: 1, or up to `SHAKE_GROW` around a shake.
    pub fn scale(&self, now: Instant) -> f64 {
        let Some((start, last)) = self.grown else { return 1.0 };
        let grow = smoothstep(now.saturating_duration_since(start).as_secs_f64() / GROW_IN.as_secs_f64());
        let shrink = smoothstep(now.saturating_duration_since(last + GROW_HOLD).as_secs_f64() / GROW_OUT.as_secs_f64());
        1.0 + (SHAKE_GROW - 1.0) * grow * (1.0 - shrink)
    }
}

#[derive(Default)]
pub struct Assist {
    pub shake: Shake,
}

impl Assist {
    /// The pointer is growing or shrinking.
    pub fn animating(&self, now: Instant) -> bool {
        self.shake.scale(now) > 1.0
    }
}

impl State {
    /// The pointer moves from where it is to `to` by `delta` (pointer_motion, before anything sees it).
    pub fn assist_motion(&mut self, _to: Point<f64, Logical>, delta: Point<f64, Logical>) {
        if self.config.accessibility.shake_to_find && self.pointer_assist.shake.motion(Instant::now(), delta) {
            self.schedule_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f64, y: f64) -> Point<f64, Logical> {
        Point::from((x, y))
    }

    #[test]
    fn shaking_grows_the_pointer_and_it_settles_back() {
        let t0 = Instant::now();
        let mut shake = Shake::default();
        // A slow sweep is no shake.
        assert!(!(0..20).any(|i| shake.motion(t0 + Duration::from_millis(i * 50), p(40.0, 0.0))));
        // Five quick 160 px swings, 60 ms each.
        let t1 = t0 + Duration::from_secs(2);
        let shaken = (0..5).map(|i| shake.motion(t1 + Duration::from_millis(i * 60), p(if i % 2 == 0 { 160.0 } else { -160.0 }, 3.0))).collect::<Vec<_>>();
        assert_eq!(shaken, [false, false, false, false, true]);
        let last = t1 + Duration::from_millis(240);
        assert_eq!(shake.scale(last), 1.0, "starts growing");
        assert_eq!(shake.scale(last + GROW_IN), SHAKE_GROW);
        assert_eq!(shake.scale(last + GROW_HOLD), SHAKE_GROW);
        let s = shake.scale(last + GROW_HOLD + GROW_OUT / 2);
        assert!(s > 1.0 && s < SHAKE_GROW, "{s}");
        assert_eq!(shake.scale(last + GROW_HOLD + GROW_OUT), 1.0);
        // Jitter back and forth by a pixel is no shake either.
        let mut jitter = Shake::default();
        assert!(!(0..40).any(|i| jitter.motion(t0 + Duration::from_millis(i * 5), p(if i % 2 == 0 { 0.5 } else { -0.5 }, 0.0))));
    }
}
