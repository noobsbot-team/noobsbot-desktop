//! Pointer accessibility (E8.6): shake to find the pointer, dwell click, hold for secondary click.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use smithay::{
    backend::input::ButtonState,
    reexports::calloop::{
        RegistrationToken,
        timer::{TimeoutAction, Timer},
    },
    utils::{Logical, Point, Rectangle},
};

use crate::{
    input::{BTN_LEFT, BTN_RIGHT},
    state::State,
};

/// How far the pointer may drift and still be resting (logical px).
const STILL: f64 = 4.0;
/// A shake: this many direction changes along one axis, its strokes covering this distance, within this time.
const SHAKE_TURNS: usize = 4;
const SHAKE_DISTANCE: f64 = 600.0;
const SHAKE_WINDOW: Duration = Duration::from_millis(500);
/// A stroke (a run of steps one way along an axis) shorter than this is wobble, not a swing.
const STROKE: f64 = 40.0;
/// The shaken pointer grows this much, eases in and out, and stays big this long after the last shake.
pub const SHAKE_GROW: f64 = 4.0;
const GROW_IN: Duration = Duration::from_millis(150);
const GROW_HOLD: Duration = Duration::from_millis(1000);
const GROW_OUT: Duration = Duration::from_millis(300);

fn near(a: Point<f64, Logical>, b: Point<f64, Logical>) -> bool {
    (a.x - b.x).hypot(a.y - b.y) <= STILL
}

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
        // Back and forth along one axis, in strokes: a fast straight fling wobbles a pixel or two across its path at
        // every step, which is no shake.
        let shaken = |axis: fn(&Point<f64, Logical>) -> f64| {
            let mut strokes: Vec<f64> = Vec::new();
            for v in self.steps.iter().map(|(_, d)| axis(d)).filter(|v| *v != 0.0) {
                match strokes.last_mut() {
                    Some(stroke) if (*stroke > 0.0) == (v > 0.0) => *stroke += v,
                    _ => strokes.push(v),
                }
            }
            strokes.retain(|s| s.abs() >= STROKE);
            let turns = strokes.windows(2).filter(|w| (w[0] > 0.0) != (w[1] > 0.0)).count();
            turns >= SHAKE_TURNS && strokes.iter().map(|s| s.abs()).sum::<f64>() >= SHAKE_DISTANCE
        };
        let shaking = shaken(|d| d.x) || shaken(|d| d.y);
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

/// Counts down while the pointer rests; one click per rest.
#[derive(Default)]
pub struct Dwell {
    /// Where and since when the pointer rests, while a click is still to come.
    rest: Option<(Instant, Point<f64, Logical>)>,
    anchor: Option<Point<f64, Logical>>,
}

impl Dwell {
    /// The pointer is at `pos`; true when it left its resting place (a new countdown starts).
    pub fn motion(&mut self, now: Instant, pos: Point<f64, Logical>) -> bool {
        if self.anchor.is_some_and(|a| near(a, pos)) {
            return false;
        }
        (self.anchor, self.rest) = (Some(pos), Some((now, pos)));
        true
    }

    /// No click for this rest (a button is down).
    pub fn cancel(&mut self) {
        self.rest = None;
    }

    /// Time left until the click, while one is coming.
    pub fn remaining(&self, now: Instant, wait: Duration) -> Option<Duration> {
        self.rest.map(|(since, _)| wait.saturating_sub(now.duration_since(since)))
    }

    /// 0 → 1 over the countdown, while one runs.
    pub fn progress(&self, now: Instant, wait: Duration) -> Option<f64> {
        let left = self.remaining(now, wait)?;
        (!wait.is_zero()).then(|| 1.0 - left.as_secs_f64() / wait.as_secs_f64())
    }
}

/// A primary press held back until it is clear whether it stays still long enough to become a secondary click.
#[derive(Default)]
pub struct Hold {
    /// Where and when (event time) the held press was.
    pending: Option<(Point<f64, Logical>, u32)>,
    /// The press became a secondary click: its release goes nowhere.
    swallow: bool,
}

#[derive(Debug, PartialEq)]
pub enum Release {
    /// Send the held press (at this time), then the release.
    Press(u32),
    Swallow,
    Pass,
}

impl Hold {
    pub fn press(&mut self, at: Point<f64, Logical>, time: u32) {
        (self.pending, self.swallow) = (Some((at, time)), false);
    }

    /// The pointer moved to `pos`: the held press's time once it strays (send it as it was).
    pub fn moved(&mut self, pos: Point<f64, Logical>) -> Option<u32> {
        self.pending.take_if(|(at, _)| !near(*at, pos)).map(|(_, time)| time)
    }

    pub fn release(&mut self) -> Release {
        match (self.pending.take(), std::mem::take(&mut self.swallow)) {
            (Some((_, time)), _) => Release::Press(time),
            (None, true) => Release::Swallow,
            (None, false) => Release::Pass,
        }
    }

    /// The hold time is up: true when a held press becomes a secondary click.
    pub fn fire(&mut self) -> bool {
        self.swallow = self.pending.take().is_some();
        self.swallow
    }

    /// The held press, to send now (another button came).
    pub fn take(&mut self) -> Option<u32> {
        self.pending.take().map(|(_, time)| time)
    }
}

#[derive(Default)]
pub struct Assist {
    pub shake: Shake,
    pub dwell: Dwell,
    pub hold: Hold,
    dwell_timer: bool,
    hold_timer: Option<RegistrationToken>,
}

impl Assist {
    /// The pointer is growing or shrinking, or a dwell ring is counting down.
    pub fn animating(&self, now: Instant, dwell: Duration) -> bool {
        self.shake.scale(now) > 1.0 || self.dwell.progress(now, dwell).is_some_and(|p| p < 1.0)
    }
}

fn ms(ms: u32) -> Duration {
    Duration::from_millis(ms.into())
}

impl State {
    pub fn dwell_wait(&self) -> Duration {
        ms(self.config.accessibility.dwell_click_ms)
    }

    pub fn now_msec(&self) -> u32 {
        Duration::from(self.protocols.clock.now()).as_millis() as u32
    }

    /// The pointer moves from where it is to `to` by `delta` (pointer_motion, before anything sees it).
    pub fn assist_motion(&mut self, to: Point<f64, Logical>, delta: Point<f64, Logical>) {
        let now = Instant::now();
        if self.config.accessibility.shake_to_find && self.pointer_assist.shake.motion(now, delta) {
            self.schedule_redraw();
        }
        if let Some(time) = self.pointer_assist.hold.moved(to) {
            self.drop_hold_timer();
            self.pointer_button(BTN_LEFT, ButtonState::Pressed, time);
        }
        let wait = self.dwell_wait();
        if wait.is_zero() {
            return;
        }
        let moved = self.pointer_assist.dwell.motion(now, to);
        if self.seat.get_pointer().is_some_and(|p| p.is_grabbed()) {
            self.pointer_assist.dwell.cancel();
        } else if moved && !std::mem::replace(&mut self.pointer_assist.dwell_timer, true) {
            // One timer per rest: it waits again whenever the pointer moved on meanwhile.
            let armed = self.loop_handle.insert_source(Timer::from_duration(wait), |_, _, state| {
                let left = state.pointer_assist.dwell.remaining(Instant::now(), state.dwell_wait()).filter(|_| !state.dwell_wait().is_zero());
                match left {
                    Some(left) if !left.is_zero() => TimeoutAction::ToDuration(left),
                    Some(_) => {
                        state.pointer_assist.dwell.cancel();
                        state.pointer_assist.dwell_timer = false;
                        let time = state.now_msec();
                        state.pointer_button(BTN_LEFT, ButtonState::Pressed, time);
                        state.pointer_button(BTN_LEFT, ButtonState::Released, time);
                        state.schedule_redraw();
                        TimeoutAction::Drop
                    }
                    None => {
                        state.pointer_assist.dwell_timer = false;
                        TimeoutAction::Drop
                    }
                }
            });
            self.pointer_assist.dwell_timer = armed.is_ok();
        }
    }

    fn drop_hold_timer(&mut self) {
        if let Some(token) = self.pointer_assist.hold_timer.take() {
            self.loop_handle.remove(token);
        }
    }

    /// A button from a device: a primary press is held back for hold-for-secondary-click, the rest goes on.
    pub fn assisted_button(&mut self, button: u32, state: ButtonState, time: u32) {
        let wait = ms(self.config.accessibility.hold_secondary_ms);
        self.pointer_assist.dwell.cancel();
        if button == BTN_LEFT && state == ButtonState::Pressed && !wait.is_zero() {
            self.pointer_assist.hold.press(self.pointer_at, time);
            self.drop_hold_timer();
            let timer = self.loop_handle.insert_source(Timer::from_duration(wait), |_, _, state| {
                state.pointer_assist.hold_timer = None;
                if state.pointer_assist.hold.fire() {
                    let time = state.now_msec();
                    state.pointer_button(BTN_RIGHT, ButtonState::Pressed, time);
                    state.pointer_button(BTN_RIGHT, ButtonState::Released, time);
                }
                TimeoutAction::Drop
            });
            match timer {
                Ok(token) => self.pointer_assist.hold_timer = Some(token),
                Err(_) => self.pointer_button(button, state, time),
            }
            return;
        }
        let held = if button == BTN_LEFT && state == ButtonState::Released {
            match self.pointer_assist.hold.release() {
                Release::Swallow => return,
                Release::Press(time) => Some(time),
                Release::Pass => None,
            }
        } else {
            self.pointer_assist.hold.take()
        };
        if let Some(time) = held {
            self.drop_hold_timer();
            self.pointer_button(BTN_LEFT, ButtonState::Pressed, time);
        }
        self.pointer_button(button, state, time);
    }

    /// The dwell countdown ring around the pointer, output-relative (logical).
    pub fn dwell_ring(&self, output_origin: Point<i32, Logical>) -> Option<Rectangle<i32, Logical>> {
        let progress = self.pointer_assist.dwell.progress(self.frame_time, self.dwell_wait())?;
        let at = self.pointer_at.to_i32_round() - output_origin;
        // Shrinks from 24 px to 6 px across.
        let r = (12.0 - 9.0 * progress).round() as i32;
        Some(Rectangle::new(at - Point::from((r, r)), (2 * r, 2 * r).into()))
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
        // Neither is a quick fling across the screen whose other axis wobbles a pixel or two each step.
        let mut fling = Shake::default();
        assert!(!(0..50).any(|i| fling.motion(t0 + Duration::from_millis(i * 5), p(20.0, if i % 2 == 0 { 1.5 } else { -1.5 }))));
    }

    #[test]
    fn dwell_counts_down_once_per_rest() {
        let (t0, wait) = (Instant::now(), Duration::from_millis(1000));
        let mut dwell = Dwell::default();
        assert_eq!(dwell.remaining(t0, wait), None);
        assert!(dwell.motion(t0, p(100.0, 100.0)));
        assert!(!dwell.motion(t0 + Duration::from_millis(300), p(102.0, 101.0)), "a few px of drift still rests");
        assert_eq!(dwell.progress(t0 + Duration::from_millis(250), wait), Some(0.25));
        assert_eq!(dwell.remaining(t0 + Duration::from_millis(1200), wait), Some(Duration::ZERO));
        dwell.cancel();
        assert_eq!(dwell.remaining(t0 + Duration::from_millis(2000), wait), None, "clicked: no more until it moves");
        assert!(!dwell.motion(t0 + Duration::from_millis(2100), p(101.0, 100.0)));
        assert!(dwell.motion(t0 + Duration::from_millis(2200), p(120.0, 100.0)));
        assert_eq!(dwell.remaining(t0 + Duration::from_millis(2700), wait), Some(Duration::from_millis(500)));
    }

    #[test]
    fn a_still_hold_turns_into_a_secondary_click() {
        let mut hold = Hold::default();
        assert_eq!(hold.release(), Release::Pass, "nothing held: releases pass");
        // Released early: the primary click, late.
        hold.press(p(10.0, 10.0), 7);
        assert_eq!(hold.release(), Release::Press(7));
        // Held still past the wait: a secondary click, and the primary release goes nowhere.
        hold.press(p(10.0, 10.0), 8);
        assert_eq!(hold.moved(p(12.0, 11.0)), None);
        assert!(hold.fire());
        assert_eq!(hold.release(), Release::Swallow);
        assert_eq!(hold.release(), Release::Pass);
        // Dragged away: the press goes out as it was, then the release passes.
        hold.press(p(10.0, 10.0), 9);
        assert_eq!(hold.moved(p(30.0, 10.0)), Some(9));
        assert!(!hold.fire());
        assert_eq!(hold.release(), Release::Pass);
    }
}
