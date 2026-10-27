//! Touchpad system gestures (E5.3) and hot corners (E5.4). The recogniser and the corner logic are pure; the `State`
//! glue at the bottom feeds them libinput's events and the pointer.
//!
//! The Space slide follows a sideways swipe 1:1, Mission Control a swipe up or down, App Exposé one down, Launchpad a
//! 4-finger pinch and Show Desktop a spread; each springs on from where the fingers let go (`Live`). Launchpad pages
//! follow too. Reduce motion: the action happens on release instead.

use std::time::{Duration, Instant};

use smithay::{
    reexports::calloop::{
        RegistrationToken,
        timer::{TimeoutAction, Timer},
    },
    utils::{Logical, Point, Size},
};

use crate::{state::State, wm::Rect};

/// What a system gesture does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    /// The Space to the right (+1, fingers moved left) or left (-1).
    Space(i32),
    MissionControl,
    AppExpose,
    /// 4-finger pinch in.
    Launchpad,
    /// 4-finger spread.
    ShowDesktop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Swipe,
    Pinch,
}

/// Swipe travel (libinput touchpad units, about 1 px) of a whole swipe at sensitivity 1: Mission Control fully open.
pub const FULL: f64 = 300.0;
/// This much of a whole swipe commits on release.
const COMMIT: f64 = 0.3;
/// Shorter than this never triggers, however fast (a twitch).
const MIN_TRAVEL: f64 = 20.0;
/// A release at least this fast along the swipe (units per ms) commits: a flick.
const FLICK: f64 = 0.4;
/// No update for this long before the release: the fingers had stopped.
const STILL_MS: u32 = 50;
/// Scale change of a whole pinch; half of it commits.
const PINCH: f64 = 0.5;
/// Past the first or last Space the screen gives at most this much of its width.
const RUBBER: f64 = 0.1;

/// Does a swipe let go `d` along its axis, of a whole swipe `full`, moving at `v` (units per ms) commit? Past
/// `COMMIT` of the way or on a flick, unless heading back.
pub fn commits(d: f64, full: f64, v: f64) -> bool {
    let back = v * d < 0.0 && v.abs() > FLICK / 4.0;
    !back && d.abs() >= MIN_TRAVEL && (v.abs() >= FLICK || d.abs() >= COMMIT * full)
}

/// Does a pinch let go `p` of a whole pinch along (in −, out +), moving at `v` of one per ms, commit? Half way, or
/// on a flick, as a swipe does (`commits`, a whole pinch counted as a whole swipe).
pub fn pinch_commits(p: f64, v: f64) -> bool {
    commits(p * FULL, FULL / (2.0 * COMMIT), v * FULL)
}

/// `x` past an end, given back as a rubber band: a third as far at first, never more than `RUBBER`.
pub fn rubber(x: f64) -> f64 {
    RUBBER * x / (x.abs() + 3.0 * RUBBER)
}

/// What follows the fingers of a swipe.
#[derive(Debug, Clone, PartialEq)]
pub enum Live {
    /// The Spaces under this key, slid over a screen this wide.
    Space(String, f64),
    /// Mission Control opening, or (`true`) closing.
    Mission(bool),
    /// App Exposé of this app opening.
    Expose(String),
    /// Launchpad opening (a pinch in), or (`true`) closing (out).
    Launchpad(bool),
    /// Show Desktop's windows moving aside, or back (a spread).
    Desktop(Vec<crate::anim::DeskMove>),
}

impl Live {
    /// The gesture whose switch gates this: down over an open overview is the App Exposé swipe, as on release.
    pub fn gesture(&self) -> Gesture {
        match self {
            Live::Space(..) => Gesture::Space(0),
            Live::Mission(false) => Gesture::MissionControl,
            Live::Mission(true) | Live::Expose(_) => Gesture::AppExpose,
            Live::Launchpad(false) => Gesture::Launchpad,
            // Spreading over Launchpad closes it, as on release.
            Live::Launchpad(true) | Live::Desktop(_) => Gesture::ShowDesktop,
        }
    }
}

/// One system gesture in progress: begin → update* → end.
#[derive(Debug, Clone)]
pub struct Tracker {
    pub kind: Kind,
    pub live: Option<Live>,
    /// Sideways (`true`) or up and down, once the fingers moved `MIN_TRAVEL`.
    pub horizontal: Option<bool>,
    sensitivity: f64,
    dx: f64,
    dy: f64,
    scale: f64,
    /// Smoothed velocity, units per ms (a pinch's in whole pinches per ms), and the time of the last update.
    vx: f64,
    vy: f64,
    vs: f64,
    last: u32,
}

impl Tracker {
    /// A tracker if this gesture is ours: swipes with `swipe_fingers` (3, or 4 to leave 3 to apps), pinches with 4+.
    /// Everything else (2-finger pinch) goes to the app.
    pub fn begin(kind: Kind, fingers: u32, swipe_fingers: u8, sensitivity: f64, time: u32) -> Option<Self> {
        let swipe = if swipe_fingers == 4 { 4 } else { 3 };
        let ours = match kind {
            Kind::Swipe => fingers == swipe,
            Kind::Pinch => fingers >= 4,
        };
        ours.then_some(Self {
            kind,
            horizontal: None,
            live: None,
            sensitivity: sensitivity.clamp(0.25, 4.0),
            dx: 0.0,
            dy: 0.0,
            scale: 1.0,
            vx: 0.0,
            vy: 0.0,
            vs: 0.0,
            last: time,
        })
    }

    /// The fingers moved by `(dx, dy)`; `scale` is the pinch's absolute scale (1 for a swipe).
    pub fn update(&mut self, time: u32, dx: f64, dy: f64, scale: f64) {
        let (dx, dy) = (dx * self.sensitivity, dy * self.sensitivity);
        let dt = f64::from(time.wrapping_sub(self.last).max(1));
        self.vx = (self.vx + dx / dt) / 2.0;
        self.vy = (self.vy + dy / dt) / 2.0;
        self.vs = (self.vs + (scale - self.scale) * self.sensitivity / PINCH / dt) / 2.0;
        (self.dx, self.dy, self.scale, self.last) = (self.dx + dx, self.dy + dy, scale, time);
        if self.horizontal.is_none() && self.dx.hypot(self.dy) >= MIN_TRAVEL {
            self.horizontal = Some(self.dx.abs() >= self.dy.abs());
        }
    }

    /// The fingers' speed (units per ms) when let go at `time`: 0 if they had stopped.
    pub fn speed(&self, time: u32) -> (f64, f64) {
        if time.wrapping_sub(self.last) > STILL_MS { (0.0, 0.0) } else { (self.vx, self.vy) }
    }

    /// A pinch so far, in whole pinches (in −, out +, scaled by sensitivity), and its speed per ms when let go at
    /// `time`: 0 if the fingers had stopped.
    pub fn pinch(&self, time: u32) -> (f64, f64) {
        let v = if time.wrapping_sub(self.last) > STILL_MS { 0.0 } else { self.vs };
        ((self.scale - 1.0) * self.sensitivity / PINCH, v)
    }

    /// Swipe travel so far, scaled by sensitivity.
    pub fn delta(&self) -> (f64, f64) {
        (self.dx, self.dy)
    }

    /// The fingers lifted: the action, if the gesture went far or fast enough (and wasn't cancelled or reversed).
    pub fn end(&self, time: u32, cancelled: bool) -> Option<Gesture> {
        if cancelled {
            return None;
        }
        if self.kind == Kind::Pinch {
            let (p, v) = self.pinch(time);
            return pinch_commits(p, v).then_some(if p < 0.0 { Gesture::Launchpad } else { Gesture::ShowDesktop });
        }
        let horizontal = self.horizontal.unwrap_or(self.dx.abs() >= self.dy.abs());
        let (vx, vy) = self.speed(time);
        let (d, v) = if horizontal { (self.dx, vx) } else { (self.dy, vy) };
        commits(d, FULL, v).then_some(match (horizontal, d < 0.0) {
            (true, true) => Gesture::Space(1),
            (true, false) => Gesture::Space(-1),
            (false, true) => Gesture::MissionControl,
            (false, false) => Gesture::AppExpose,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// How close (px) to the very corner the pointer must be.
const HIT: f64 = 2.0;
/// The pointer rests in a corner this long before it triggers (no accidents).
pub const DELAY: Duration = Duration::from_millis(250);
/// The ripple's length.
const RIPPLE: f64 = 0.4;
/// The ripple's largest radius.
const RIPPLE_R: f64 = 44.0;

/// The corner `p` (output-relative) is in, if any.
pub fn corner_at(p: Point<f64, Logical>, size: Size<i32, Logical>) -> Option<Corner> {
    let (w, h) = (f64::from(size.w), f64::from(size.h));
    let (left, right) = (p.x <= HIT, p.x >= w - 1.0 - HIT);
    let (top, bottom) = (p.y <= HIT, p.y >= h - 1.0 - HIT);
    match (left, right, top, bottom) {
        (true, _, true, _) => Some(Corner::TopLeft),
        (_, true, true, _) => Some(Corner::TopRight),
        (true, _, _, true) => Some(Corner::BottomLeft),
        (_, true, _, true) => Some(Corner::BottomRight),
        _ => None,
    }
}

/// A corner of the display at `geo` no other display touches: where one does, the pointer just crosses over and
/// no hot corner fires.
pub fn open_corner(corner: Corner, geo: Rect, others: &[Rect]) -> bool {
    let (right, bottom) = (geo.loc.x + geo.size.w - 1, geo.loc.y + geo.size.h - 1);
    // The corner pixel and the way out of it.
    let (x, y, dx, dy) = match corner {
        Corner::TopLeft => (geo.loc.x, geo.loc.y, -1, -1),
        Corner::TopRight => (right, geo.loc.y, 1, -1),
        Corner::BottomLeft => (geo.loc.x, bottom, -1, 1),
        Corner::BottomRight => (right, bottom, 1, 1),
    };
    ![(x + dx, y), (x, y + dy)].into_iter().any(|p| others.iter().any(|o| o.contains(Point::from(p))))
}

/// The open corner (`open_corner`) of the display in `screens` that holds `at` (global), with that display's index.
pub fn hot_corner(screens: &[Rect], at: Point<f64, Logical>) -> Option<(usize, Corner)> {
    let i = screens.iter().position(|s| s.to_f64().contains(at))?;
    let geo = screens[i];
    let corner = corner_at(at - geo.loc.to_f64(), geo.size)?;
    let others: Vec<Rect> = screens.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, r)| *r).collect();
    open_corner(corner, geo, &others).then_some((i, corner))
}

/// Hot corner state: the corner the pointer rests in and since when.
#[derive(Default)]
pub struct Corners {
    at: Option<(Corner, Instant)>,
    fired: bool,
    timer: Option<RegistrationToken>,
    /// The ripple of the last trigger and when it started.
    pub ripple: Option<(Corner, Instant)>,
    /// The display of the pointer's corner, which the ripple shows on (`None`: the first).
    pub output: Option<smithay::output::Output>,
}

impl Corners {
    /// The pointer moved to `corner` (`None`: not in an active one). `true`: it just entered one, start the delay.
    /// A drag never enters a corner.
    pub fn motion(&mut self, corner: Option<Corner>, dragging: bool, now: Instant) -> bool {
        let corner = corner.filter(|_| !dragging);
        if self.at.map(|(c, _)| c) == corner {
            return false;
        }
        (self.at, self.fired) = (corner.map(|c| (c, now)), false);
        corner.is_some()
    }

    /// The delay ran out: the corner to trigger, once per visit, and not if a drag started meanwhile.
    pub fn due(&mut self, dragging: bool, now: Instant) -> Option<Corner> {
        let (corner, since) = self.at.filter(|_| !self.fired && !dragging)?;
        (now.saturating_duration_since(since) >= DELAY).then(|| {
            self.fired = true;
            corner
        })
    }
}

/// The ripple at `t` (0 → 1): a glass circle growing out of the corner, and its opacity.
pub fn ripple(corner: Corner, size: Size<i32, Logical>, t: f64) -> (Rect, f32) {
    let r = (RIPPLE_R * (0.3 + 0.7 * (1.0 - (1.0 - t).powi(3)))).round() as i32;
    let x = if matches!(corner, Corner::TopLeft | Corner::BottomLeft) { 0 } else { size.w };
    let y = if matches!(corner, Corner::TopLeft | Corner::TopRight) { 0 } else { size.h };
    (Rect::new((x - r, y - r).into(), (2 * r, 2 * r).into()), (1.0 - t) as f32)
}

impl State {
    /// libinput's swipe/pinch began: ours (see `Tracker::begin`) or the app's. `true`: ours.
    pub fn gesture_begin(&mut self, kind: Kind, fingers: u32, time: u32) -> bool {
        // Locked (E6.4): no system gesture, and none reaches an app either.
        if self.locked() {
            self.gesture = None;
            return true;
        }
        let touchpad = &self.config.touchpad;
        self.gesture = Tracker::begin(kind, fingers, touchpad.effective_swipe_fingers(), touchpad.gesture_sensitivity, time);
        self.gesture.is_some()
    }

    /// An update of our gesture; `false`: not ours, forward it. While Launchpad is open its pages follow the fingers.
    pub fn gesture_update(&mut self, time: u32, delta: Point<f64, Logical>, scale: f64) -> bool {
        if self.locked() {
            return true;
        }
        let Some(tracker) = self.gesture.as_mut() else { return false };
        let fresh = tracker.horizontal.is_none();
        tracker.update(time, delta.x, delta.y, scale);
        let (kind, horizontal, (dx, dy)) = (tracker.kind, tracker.horizontal, tracker.delta());
        let (pinch, undecided) = (tracker.pinch(time).0, tracker.live.is_none());
        let decide = match kind {
            // A pinch follows once it has gone as far as a swipe must, the way it went then.
            Kind::Pinch => undecided && pinch.abs() * FULL >= MIN_TRAVEL,
            // Fingers left = the next page, as with natural scrolling.
            Kind::Swipe if self.launchpad_scroll(-delta.x, -delta.y, 0.0, false) => return true,
            Kind::Swipe => fresh && horizontal.is_some(),
        };
        if decide {
            let live = self.live(kind, horizontal.unwrap_or(false), if kind == Kind::Pinch { pinch } else { dy });
            if let Some(tracker) = self.gesture.as_mut() {
                tracker.live = live;
            }
        }
        match self.gesture.as_ref().and_then(|t| t.live.clone()) {
            Some(Live::Space(key, width)) => self.swipe_space(&key, -dx / width),
            Some(Live::Mission(closing)) => self.mission_hold(f64::from(u8::from(closing)) - dy / FULL, None),
            Some(Live::Expose(app)) => self.mission_hold(dy / FULL, Some(app)),
            Some(Live::Launchpad(closing)) => self.launchpad_hold(if closing { 1.0 - pinch } else { -pinch }),
            Some(Live::Desktop(moves)) => self.desktop_hold(&moves, pinch),
            None => {}
        }
        true
    }

    /// What follows a swipe that just turned out sideways (or up and down, `d` so far), or a pinch `d` of a whole one
    /// in (−) or out: nothing under reduce motion or with that gesture switched off.
    fn live(&self, kind: Kind, horizontal: bool, d: f64) -> Option<Live> {
        if self.config.appearance.reduce_motion {
            return None;
        }
        let live = if kind == Kind::Pinch { self.live_pinch(d) } else { self.live_any(horizontal, d) };
        live.filter(|l| self.config.touchpad.allows(l.gesture()))
    }

    /// In opens Launchpad; out closes it, else shows the desktop (or brings the windows back).
    fn live_pinch(&self, p: f64) -> Option<Live> {
        match (p < 0.0, self.launchpad_open()) {
            (true, false) => Some(Live::Launchpad(false)),
            (false, true) => Some(Live::Launchpad(true)),
            (false, false) => self.config.touchpad.allows(Gesture::ShowDesktop).then(|| Live::Desktop(self.desktop_moves())),
            (true, true) => None,
        }
    }

    fn live_any(&self, horizontal: bool, dy: f64) -> Option<Live> {
        // Under Mission Control the Space changes at once, on release.
        if horizontal && self.mission.is_none() {
            let output = self.pointer_output()?;
            let width = self.space.output_geometry(&output)?.size.w;
            return Some(Live::Space(self.output_key(&output), f64::from(width.max(1))));
        }
        // Up opens Mission Control, down App Exposé of the focused app, or closes either; the rest acts on release.
        match self.mission.as_ref().map(|m| m.progress.target() == 1.0) {
            None if !horizontal && dy < 0.0 => Some(Live::Mission(false)),
            None if !horizontal && dy > 0.0 => self.focused_window().and_then(|w| self.app_key(&w)).map(Live::Expose),
            Some(true) if !horizontal && dy > 0.0 => Some(Live::Mission(true)),
            _ => None,
        }
    }

    /// The end of our gesture: its action; `false`: not ours, forward it.
    pub fn gesture_end(&mut self, time: u32, cancelled: bool) -> bool {
        let locked = self.locked();
        let Some(tracker) = self.gesture.take() else { return locked };
        // Whatever followed the fingers springs on or back; locked meanwhile, back.
        let cancelled = cancelled || locked;
        let (dx, dy) = tracker.delta();
        let (vx, vy) = tracker.speed(time);
        let (pinch, vp) = tracker.pinch(time);
        match tracker.live {
            Some(Live::Space(key, width)) => {
                self.swipe_space_end(&key, -dx / width, -vx * 1000.0 / width, !cancelled && commits(dx, width, vx));
                return true;
            }
            Some(Live::Mission(closing)) => {
                // Only a swipe the way it started commits.
                let commit = !cancelled && (dy < 0.0) != closing && commits(dy, FULL, vy);
                self.mission_let_go(commit != closing, -vy * 1000.0 / FULL);
                return true;
            }
            Some(Live::Expose(_)) => {
                self.mission_let_go(!cancelled && dy > 0.0 && commits(dy, FULL, vy), vy * 1000.0 / FULL);
                return true;
            }
            Some(Live::Launchpad(closing)) => {
                let commit = !cancelled && (pinch < 0.0) != closing && pinch_commits(pinch, vp);
                self.launchpad_let_go(commit != closing, -vp * 1000.0);
                return true;
            }
            Some(Live::Desktop(moves)) => {
                self.desktop_let_go(moves, !cancelled && pinch > 0.0 && pinch_commits(pinch, vp));
                return true;
            }
            None if locked => return true,
            None => {}
        }
        if tracker.kind == Kind::Swipe && self.launchpad_scroll(0.0, 0.0, 0.0, true) {
            return true;
        }
        let gesture = tracker.end(time, cancelled).filter(|g| self.config.touchpad.allows(*g));
        tracing::debug!(?gesture, delta = ?tracker.delta(), "gesture");
        let mission = self.mission.as_ref().filter(|m| m.progress.target() == 1.0).map(|m| m.app.is_some());
        match gesture {
            Some(Gesture::Space(delta)) => self.switch_space(delta),
            // Up again over Mission Control does nothing; over App Exposé it widens to all windows.
            Some(Gesture::MissionControl) if mission != Some(false) => self.toggle_mission_control(),
            // Down closes an open overview.
            Some(Gesture::AppExpose) if mission.is_some() => self.close_mission(),
            Some(Gesture::AppExpose) => self.app_expose(),
            Some(Gesture::Launchpad) if !self.launchpad_open() => self.open_launchpad(),
            // Spreading closes Launchpad, else shows the desktop (or brings the windows back).
            Some(Gesture::ShowDesktop) if self.launchpad_open() => {
                self.close_launchpad();
            }
            Some(Gesture::ShowDesktop) => self.toggle_desktop(),
            _ => {}
        }
        true
    }

    /// The configured action of a corner, if any.
    fn corner_action(&self, corner: Corner) -> Option<crate::shortcuts::Action> {
        let c = &self.config.hot_corners;
        let text = match corner {
            Corner::TopLeft => &c.top_left,
            Corner::TopRight => &c.top_right,
            Corner::BottomLeft => &c.bottom_left,
            Corner::BottomRight => &c.bottom_right,
        };
        crate::shortcuts::action(&self.config, text)
    }

    fn dragging(&self) -> bool {
        self.seat.get_pointer().is_some_and(|p| p.is_grabbed())
    }

    /// The pointer moved to `at` (global): resting in an active corner of any display for `DELAY` triggers it. Never
    /// while locked (E6.4).
    pub fn hot_corner_motion(&mut self, at: Point<f64, Logical>) {
        if self.locked() {
            return;
        }
        let outputs: Vec<smithay::output::Output> = self.space.outputs().cloned().collect();
        let screens: Vec<Rect> = outputs.iter().filter_map(|o| self.space.output_geometry(o)).collect();
        let hit = hot_corner(&screens, at).filter(|(_, c)| self.corner_action(*c).is_some());
        let corner = hit.map(|(_, c)| c);
        if let Some((i, _)) = hit {
            self.corners.output = outputs.get(i).cloned();
        }
        let dragging = self.dragging();
        let entered = self.corners.motion(corner, dragging, Instant::now());
        // Left the corner (or entered another): its pending trigger goes.
        if (entered || self.corners.at.is_none())
            && let Some(token) = self.corners.timer.take()
        {
            self.loop_handle.remove(token);
        }
        if !entered {
            return;
        }
        let timer = self.loop_handle.insert_source(Timer::from_duration(DELAY), |_, _, state| {
            state.corners.timer = None;
            let dragging = state.dragging();
            if !state.locked()
                && let Some(corner) = state.corners.due(dragging, Instant::now())
                && let Some(action) = state.corner_action(corner)
            {
                state.corners.ripple = Some((corner, Instant::now()));
                state.run_action(action);
                state.schedule_redraw();
            }
            TimeoutAction::Drop
        });
        self.corners.timer = timer.inspect_err(|err| tracing::warn!("hot corner timer: {err}")).ok();
    }

    /// This frame's ripple on `output` (rect, opacity), clearing a finished one. A ripple started in the future
    /// (`--show`) holds half way.
    pub fn ripple_frame(&mut self, output: &smithay::output::Output) -> Option<(Rect, f32)> {
        let shown = self.corners.output.as_ref().filter(|o| self.space.outputs().any(|x| x == *o)).or(self.space.outputs().next());
        let size = self.space.output_geometry(output).filter(|_| shown == Some(output))?.size;
        let (corner, start) = self.corners.ripple?;
        let now = Instant::now();
        let t = if start > now { 0.5 } else { now.duration_since(start).as_secs_f64() / RIPPLE };
        if t >= 1.0 {
            self.corners.ripple = None;
            return None;
        }
        Some(ripple(corner, size, t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A swipe of `steps` updates of `(dx, dy)` every `ms`, released `release_after` ms after the last.
    fn swipe(fingers: u32, setting: u8, steps: u32, ms: u32, dx: f64, dy: f64, release_after: u32) -> Option<Gesture> {
        let mut t = Tracker::begin(Kind::Swipe, fingers, setting, 1.0, 0)?;
        for i in 1..=steps {
            t.update(i * ms, dx, dy, 1.0);
        }
        t.end(steps * ms + release_after, false)
    }

    #[test]
    fn live_follows_only_switched_on_gestures() {
        let mut touchpad = crate::config::Touchpad::default();
        let (space, open, close) = (Live::Space("k".into(), 1.0), Live::Mission(false), Live::Mission(true));
        assert!([&space, &open, &close].iter().all(|l| touchpad.allows(l.gesture())));
        touchpad.gesture_spaces = false;
        touchpad.gesture_mission_control = false;
        assert!(!touchpad.allows(space.gesture()) && !touchpad.allows(open.gesture()));
        assert!(touchpad.allows(close.gesture()));
        let (pad_open, pad_close) = (Live::Launchpad(false), Live::Launchpad(true));
        assert!(touchpad.allows(pad_open.gesture()) && touchpad.allows(pad_close.gesture()));
        touchpad.gesture_show_desktop = false;
        assert!(touchpad.allows(pad_open.gesture()) && !touchpad.allows(pad_close.gesture()), "a spread closes Launchpad");
        let expose = Live::Expose("app".into());
        assert!(touchpad.allows(expose.gesture()));
        touchpad.gesture_app_expose = false;
        assert!(!touchpad.allows(close.gesture()) && !touchpad.allows(expose.gesture()));
    }

    #[test]
    fn flick_commits_slow_drag_cancels() {
        // A quick flick: 60 units in 30 ms (2 units/ms) left.
        assert_eq!(swipe(3, 3, 3, 10, -20.0, 0.0, 5), Some(Gesture::Space(1)));
        // The same 60 units dragged slowly and let go: not 30% of the way, cancelled.
        assert_eq!(swipe(3, 3, 30, 50, -2.0, 0.0, 5), None);
        // Fast, then the fingers stop before lifting: no flick.
        assert_eq!(swipe(3, 3, 3, 10, -20.0, 0.0, 200), None);
        // Slow but past 30% of the way commits.
        assert_eq!(swipe(3, 3, 20, 50, 5.0, 0.0, 200), Some(Gesture::Space(-1)));
        // A twitch never triggers.
        assert_eq!(swipe(3, 3, 1, 5, -5.0, 0.0, 1), None);
    }

    #[test]
    fn release_commits_past_30_percent_or_on_a_flick() {
        let width = 1000.0;
        assert!(commits(-310.0, width, 0.0), "past 30%, let go still");
        assert!(!commits(-290.0, width, 0.0), "short of 30%: back");
        assert!(commits(-50.0, width, -0.5), "a flick from anywhere");
        assert!(!commits(-500.0, width, 0.2), "past 30%, but heading back");
        assert!(!commits(-10.0, width, -2.0), "a twitch");
    }

    #[test]
    fn the_axis_locks_once_moved() {
        let mut t = Tracker::begin(Kind::Swipe, 3, 3, 1.0, 0).unwrap();
        t.update(10, -10.0, -5.0, 1.0);
        assert_eq!(t.horizontal, None, "not far enough yet");
        t.update(20, -15.0, 2.0, 1.0);
        assert_eq!(t.horizontal, Some(true));
        // Drifting up a lot later still swipes Spaces.
        for i in 3..10 {
            t.update(i * 10, 0.0, -20.0, 1.0);
        }
        assert_eq!(t.end(95, false), None, "sideways only 25 units: back, and never Mission Control");
    }

    #[test]
    fn pinch_commits_past_half_or_on_a_flick() {
        assert!(pinch_commits(-0.55, 0.0) && pinch_commits(0.55, 0.0), "past half, let go still");
        assert!(!pinch_commits(-0.45, 0.0), "short of half: back");
        assert!(pinch_commits(0.2, 0.01), "a flick out");
        assert!(!pinch_commits(0.7, -0.002), "past half, but heading back");
        assert!(!pinch_commits(0.03, 0.05), "a twitch");
    }

    #[test]
    fn rubber_band_gives_a_little_and_never_much() {
        assert_eq!(rubber(0.0), 0.0);
        assert!((rubber(0.03) - 0.01).abs() < 0.002, "a third as far at first: {}", rubber(0.03));
        assert!(rubber(0.5) < RUBBER && rubber(5.0) < RUBBER && rubber(5.0) > rubber(0.5));
        assert_eq!(rubber(-0.4), -rubber(0.4));
    }

    #[test]
    fn directions() {
        assert_eq!(swipe(3, 3, 3, 10, 0.0, -30.0, 5), Some(Gesture::MissionControl));
        assert_eq!(swipe(3, 3, 3, 10, 2.0, 30.0, 5), Some(Gesture::AppExpose));
    }

    #[test]
    fn reversing_cancels() {
        let mut t = Tracker::begin(Kind::Swipe, 3, 3, 1.0, 0).unwrap();
        for i in 1..=10 {
            t.update(i * 10, -20.0, 0.0, 1.0);
        }
        for i in 11..=14 {
            t.update(i * 10, 10.0, 0.0, 1.0);
        }
        assert_eq!(t.end(145, false), None, "past half way, but heading back when let go");
    }

    #[test]
    fn finger_count_option() {
        assert!(Tracker::begin(Kind::Swipe, 3, 3, 1.0, 0).is_some());
        assert!(Tracker::begin(Kind::Swipe, 4, 3, 1.0, 0).is_none(), "4-finger swipes go to apps by default");
        assert!(Tracker::begin(Kind::Swipe, 3, 4, 1.0, 0).is_none(), "set to 4, 3-finger swipes go to apps");
        assert_eq!(swipe(4, 4, 3, 10, -20.0, 0.0, 5), Some(Gesture::Space(1)));
        assert!(Tracker::begin(Kind::Pinch, 2, 3, 1.0, 0).is_none(), "2-finger pinch zooms in the app");
    }

    #[test]
    fn pinches() {
        // Pinched to `scale` in one step, let go once the fingers stopped.
        let pinch = |scale: f64| {
            let mut t = Tracker::begin(Kind::Pinch, 4, 3, 1.0, 0).unwrap();
            t.update(10, 0.0, 0.0, scale);
            t.end(200, false)
        };
        assert_eq!(pinch(0.6), Some(Gesture::Launchpad));
        assert_eq!(pinch(1.4), Some(Gesture::ShowDesktop));
        assert_eq!(pinch(0.9), None);
        // The same short pinch, let go while moving fast: a flick.
        let mut t = Tracker::begin(Kind::Pinch, 4, 3, 1.0, 0).unwrap();
        for (i, scale) in [0.97, 0.94, 0.9].into_iter().enumerate() {
            t.update(10 * (i as u32 + 1), 0.0, 0.0, scale);
        }
        assert!((t.pinch(35).0 + 0.2).abs() < 1e-9, "a fifth of a whole pinch in");
        assert_eq!(t.end(35, false), Some(Gesture::Launchpad));
        let mut t = Tracker::begin(Kind::Pinch, 4, 3, 1.0, 0).unwrap();
        t.update(10, 0.0, 0.0, 0.5);
        assert_eq!(t.end(20, true), None, "cancelled by libinput");
    }

    #[test]
    fn sensitivity_scales_travel() {
        let mut t = Tracker::begin(Kind::Swipe, 3, 3, 2.0, 0).unwrap();
        for i in 1..=40 {
            t.update(i * 50, 2.0, 0.0, 1.0);
        }
        // 80 units, doubled: past half way.
        assert_eq!(t.end(2200, false), Some(Gesture::Space(-1)));
    }

    #[test]
    fn corners_hit_only_the_very_corner() {
        let size = Size::from((1920, 1080));
        assert_eq!(corner_at((0.0, 0.0).into(), size), Some(Corner::TopLeft));
        assert_eq!(corner_at((1919.0, 0.0).into(), size), Some(Corner::TopRight));
        assert_eq!(corner_at((1.0, 1079.0).into(), size), Some(Corner::BottomLeft));
        assert_eq!(corner_at((1919.0, 1078.0).into(), size), Some(Corner::BottomRight));
        assert_eq!(corner_at((0.0, 500.0).into(), size), None);
        assert_eq!(corner_at((10.0, 0.0).into(), size), None);
    }

    #[test]
    fn corners_meeting_another_display_are_closed() {
        let laptop = Rect::new((0, 0).into(), (1536, 960).into());
        // A taller monitor on the right, tops aligned: both right corners lead onto it.
        let monitor = Rect::new((1536, 0).into(), (2560, 1440).into());
        assert!(open_corner(Corner::TopLeft, laptop, &[monitor]));
        assert!(open_corner(Corner::BottomLeft, laptop, &[monitor]));
        assert!(!open_corner(Corner::TopRight, laptop, &[monitor]));
        assert!(!open_corner(Corner::BottomRight, laptop, &[monitor]));
        // A shorter one, bottoms aligned: the top right corner is open again.
        let short = Rect::new((1536, 460).into(), (800, 500).into());
        assert!(open_corner(Corner::TopRight, laptop, &[short]));
        assert!(!open_corner(Corner::BottomRight, laptop, &[short]));
        // One above: the top corners are closed.
        let above = Rect::new((0, -1000).into(), (1536, 1000).into());
        assert!(!open_corner(Corner::TopLeft, laptop, &[above]) && !open_corner(Corner::TopRight, laptop, &[above]));
        assert!(open_corner(Corner::TopRight, laptop, &[]));
    }

    #[test]
    fn each_display_has_its_own_corners_but_not_on_shared_edges() {
        // Side by side: the outer corners of each, never where they meet.
        let side = [Rect::new((0, 0).into(), (1920, 1080).into()), Rect::new((1920, 0).into(), (1920, 1080).into())];
        assert_eq!(hot_corner(&side, (0.0, 0.0).into()), Some((0, Corner::TopLeft)));
        assert_eq!(hot_corner(&side, (3839.0, 1079.0).into()), Some((1, Corner::BottomRight)));
        assert_eq!(hot_corner(&side, (1920.0, 0.0).into()), None, "the second's top left meets the first");
        assert_eq!(hot_corner(&side, (1919.0, 1079.0).into()), None, "the first's bottom right meets the second");
        assert_eq!(hot_corner(&side, (5000.0, 0.0).into()), None, "on no display");
        // Stacked: the top one keeps its top corners, the bottom one its bottom corners.
        let stacked = [Rect::new((0, 0).into(), (1920, 1080).into()), Rect::new((0, 1080).into(), (1920, 1080).into())];
        assert_eq!(hot_corner(&stacked, (1919.0, 0.0).into()), Some((0, Corner::TopRight)));
        assert_eq!(hot_corner(&stacked, (0.0, 2159.0).into()), Some((1, Corner::BottomLeft)));
        assert_eq!(hot_corner(&stacked, (0.0, 1079.0).into()), None);
        assert_eq!(hot_corner(&stacked, (1919.0, 1080.0).into()), None);
    }

    #[test]
    fn corner_waits_fires_once_never_while_dragging() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut c = Corners::default();
        assert!(c.motion(Some(Corner::TopLeft), false, t0));
        assert!(!c.motion(Some(Corner::TopLeft), false, ms(50)), "still there: no new delay");
        assert_eq!(c.due(false, ms(100)), None, "too soon");
        assert_eq!(c.due(false, ms(260)), Some(Corner::TopLeft));
        assert_eq!(c.due(false, ms(400)), None, "once per visit");
        // Leave and come back: armed again.
        assert!(!c.motion(None, false, ms(500)));
        assert!(c.motion(Some(Corner::TopLeft), false, ms(600)));
        assert_eq!(c.due(true, ms(900)), None, "a drag started meanwhile");
        // A drag into the corner never arms it.
        let mut c = Corners::default();
        assert!(!c.motion(Some(Corner::BottomRight), true, t0));
        assert_eq!(c.due(false, ms(300)), None);
    }

    #[test]
    fn ripple_grows_and_fades_from_its_corner() {
        let size = Size::from((1920, 1080));
        let (start, a0) = ripple(Corner::BottomRight, size, 0.0);
        let (end, a1) = ripple(Corner::BottomRight, size, 0.99);
        assert_eq!(start.loc + Point::from((start.size.w / 2, start.size.h / 2)), Point::from((1920, 1080)));
        assert!(end.size.w > start.size.w && a1 < a0);
    }
}
