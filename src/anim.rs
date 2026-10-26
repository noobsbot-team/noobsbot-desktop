//! Motion (D10): spring presets, fades, and the frame-loop hook that keeps redrawing only while something moves.

use std::time::{Duration, Instant};

use smithay::{
    desktop::Window,
    utils::{Logical, Point, Rectangle},
};

use crate::{render::motion, state::State};

/// A damped spring from 0 to 1: `response` is the period of the undamped spring, `damping` 1.0 never overshoots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spring {
    pub response: f64,
    pub damping: f64,
}

/// Menus, popovers, toggles, hover.
pub const SNAPPY: Spring = Spring { response: 0.25, damping: 0.90 };
/// Windows, panels sliding, Spaces.
pub const SMOOTH: Spring = Spring { response: 0.40, damping: 1.00 };
/// Small UI bounces.
#[cfg_attr(not(test), expect(dead_code, reason = "D10 preset, for the first small UI bounce"))]
pub const BOUNCY: Spring = Spring { response: 0.35, damping: 0.70 };

/// Closer than this to the target (a fraction of the distance) counts as settled: 0.5 px over 500 px.
const EPS: f64 = 0.001;

impl Spring {
    fn omega(self) -> f64 {
        std::f64::consts::TAU / self.response
    }

    /// Position at `t` seconds.
    pub fn value(self, t: f64) -> f64 {
        let (w, z) = (self.omega(), self.damping);
        if z >= 1.0 {
            return 1.0 - (1.0 + w * t) * (-w * t).exp();
        }
        let wd = w * (1.0 - z * z).sqrt();
        1.0 - (-z * w * t).exp() * ((wd * t).cos() + z * w / wd * (wd * t).sin())
    }

    /// When it stays within `EPS` of the target: where the decay envelope drops below it.
    pub fn settle(self) -> Duration {
        let (w, z) = (self.omega(), self.damping);
        let envelope = |t: f64| if z >= 1.0 { (1.0 + w * t) * (-w * t).exp() } else { (-z * w * t).exp() / (1.0 - z * z).sqrt() };
        let ms = (0u32..10_000).find(|ms| envelope(f64::from(*ms) / 1000.0) < EPS).unwrap_or(10_000);
        Duration::from_millis(ms.into())
    }
}

/// How a value travels from 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Curve {
    Spring(Spring),
    /// Cubic ease-out over a fixed time (D10 fades: 120 ms in, 150 ms out).
    Ease(Duration),
}

pub const FADE_IN: Duration = Duration::from_millis(120);
pub const FADE_OUT: Duration = Duration::from_millis(150);
/// Reduce motion turns every movement into a crossfade this long.
pub const CROSSFADE: Duration = Duration::from_millis(150);

/// `NOOBSBOT_SLOW_MOTION=10` plays every animation 10× slower, to watch or screenshot one.
fn slow() -> f64 {
    static SLOW: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *SLOW.get_or_init(|| std::env::var("NOOBSBOT_SLOW_MOTION").ok().and_then(|s| s.parse().ok()).filter(|f: &f64| *f > 0.0).unwrap_or(1.0))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anim {
    pub start: Instant,
    pub curve: Curve,
}

impl Anim {
    pub fn new(curve: Curve) -> Self {
        Self { start: Instant::now(), curve }
    }

    pub fn duration(&self) -> Duration {
        let d = match self.curve {
            Curve::Spring(s) => s.settle(),
            Curve::Ease(d) => d,
        };
        d.mul_f64(slow())
    }

    pub fn end(&self) -> Instant {
        self.start + self.duration()
    }

    pub fn done(&self, now: Instant) -> bool {
        now >= self.end()
    }

    /// 0 at the start, exactly 1 once done.
    pub fn progress(&self, now: Instant) -> f64 {
        if self.done(now) {
            return 1.0;
        }
        let t = now.saturating_duration_since(self.start).as_secs_f64() / slow();
        match self.curve {
            Curve::Spring(s) => s.value(t),
            Curve::Ease(d) => 1.0 - (1.0 - t / d.as_secs_f64()).powi(3),
        }
    }
}

/// Where a window frame is drawn and how opaque.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub rect: Rectangle<f64, Logical>,
    pub alpha: f64,
}

impl Look {
    pub fn at(rect: Rectangle<f64, Logical>) -> Self {
        Self { rect, alpha: 1.0 }
    }

    /// `rect` scaled by `s` about its centre, at `alpha`.
    pub fn scaled(rect: Rectangle<f64, Logical>, s: f64, alpha: f64) -> Self {
        let (w, h) = (rect.size.w * s, rect.size.h * s);
        let loc = (rect.loc.x + (rect.size.w - w) / 2.0, rect.loc.y + (rect.size.h - h) / 2.0);
        Self { rect: Rectangle::new(loc.into(), (w, h).into()), alpha }
    }

    pub fn lerp(self, to: Look, t: f64) -> Look {
        let l = |a: f64, b: f64| a + (b - a) * t;
        let (a, b) = (self.rect, to.rect);
        Look {
            rect: Rectangle::new((l(a.loc.x, b.loc.x), l(a.loc.y, b.loc.y)).into(), (l(a.size.w, b.size.w), l(a.size.h, b.size.h)).into()),
            alpha: l(self.alpha, to.alpha).clamp(0.0, 1.0),
        }
    }

    /// Where `area` (drawn around `frame`, e.g. with its shadow) goes when `frame` is drawn at this look.
    pub fn map(self, frame: Rectangle<f64, Logical>, area: Rectangle<f64, Logical>) -> Rectangle<f64, Logical> {
        let (sx, sy) = (self.rect.size.w / frame.size.w.max(1.0), self.rect.size.h / frame.size.h.max(1.0));
        let loc = (self.rect.loc.x + (area.loc.x - frame.loc.x) * sx, self.rect.loc.y + (area.loc.y - frame.loc.y) * sy);
        Rectangle::new(loc.into(), (area.size.w * sx, area.size.h * sy).into())
    }
}

/// How a shell piece appears (D10): popovers fade in over 120 ms while growing 0.98 → 1 (`snappy`); banners
/// (`slide`) come in from past the right edge (`smooth`). Reduce motion: a 150 ms fade only.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Entrance {
    pub start: Instant,
    pub reduce: bool,
    pub slide: bool,
}

impl Entrance {
    fn moving(&self) -> Anim {
        let curve = match (self.reduce, self.slide) {
            (true, _) => Curve::Ease(CROSSFADE),
            (false, true) => Curve::Spring(SMOOTH),
            (false, false) => Curve::Spring(SNAPPY),
        };
        Anim { start: self.start, curve }
    }

    fn fade(&self) -> Anim {
        Anim { start: self.start, curve: Curve::Ease(if self.reduce { CROSSFADE } else { FADE_IN }) }
    }

    pub fn end(&self) -> Instant {
        self.moving().end().max(self.fade().end())
    }

    /// Where the piece whose settled bounds are `rect` is drawn at `now`.
    pub fn look(&self, rect: Rectangle<f64, Logical>, now: Instant) -> Look {
        let alpha = self.fade().progress(now);
        let from = match (self.reduce, self.slide) {
            (true, _) => Look::at(rect),
            (false, true) => Look::at(Rectangle::new((rect.loc.x + rect.size.w + 16.0, rect.loc.y).into(), rect.size)),
            (false, false) => Look::scaled(rect, 0.98, 1.0),
        };
        Look { alpha, ..from.lerp(Look::at(rect), self.moving().progress(now)) }
    }
}

/// Keeps frames coming until `end` (see `State::animate`).
pub fn extend(until: &mut Option<Instant>, end: Instant) {
    *until = Some(until.map_or(end, |t| t.max(end)));
}

/// A live window on its way from `from` to wherever its frame is now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Motion {
    pub from: Look,
    pub anim: Anim,
}

impl Motion {
    /// A spring moves it, but the fade in takes 120 ms (D10).
    pub fn look(&self, frame: Rectangle<f64, Logical>, now: Instant) -> Look {
        let p = self.anim.progress(now);
        let fade = match self.anim.curve {
            Curve::Spring(_) => Anim { start: self.anim.start, curve: Curve::Ease(FADE_IN) }.progress(now),
            Curve::Ease(_) => p,
        };
        let alpha = self.from.alpha + (1.0 - self.from.alpha) * fade;
        Look { alpha, ..self.from.lerp(Look::at(frame), p) }
    }
}

impl State {
    /// `curve`, or the reduce-motion crossfade.
    pub fn curve(&self, curve: Curve) -> Curve {
        if self.config.appearance.reduce_motion { Curve::Ease(CROSSFADE) } else { curve }
    }

    /// The frame-loop hook: keeps drawing frames until `anim` has settled, then stops (idle is 0% CPU).
    pub fn animate(&mut self, anim: &Anim) {
        extend(&mut self.anim_until, anim.end());
        self.schedule_redraw();
    }

    /// After a frame: is another one needed for an animation? Judged at the time the frame is seen, like the
    /// animations themselves.
    pub fn animating(&mut self) -> bool {
        self.anim_until = self.anim_until.filter(|t| *t > self.frame_time);
        self.anim_until.is_some()
    }

    fn output_loc(&self) -> Point<i32, Logical> {
        self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map_or_else(Default::default, |g| g.loc)
    }

    fn window_frame(&self, window: &Window) -> Option<Rectangle<f64, Logical>> {
        motion::window_frame(&self.space, window, self.output_loc())
    }

    /// Where `window` is drawn right now: mid-motion, as a ghost, or at its frame.
    fn current_look(&self, window: &Window) -> Option<Look> {
        let now = Instant::now();
        let ghost = self.backend.as_ref().and_then(|b| b.ghosts.iter().find(|g| g.window == *window)).map(|g| g.look(now));
        let frame = self.window_frame(window);
        ghost.or_else(|| frame.map(|f| motion::current(window, now).map_or(Look::at(f), |m| m.look(f, now))))
    }

    /// `window` springs from `from` to its frame (E5.5).
    fn move_from(&mut self, window: &Window, from: Look, curve: Curve) {
        let anim = Anim::new(self.curve(curve));
        motion::start(window, Motion { from, anim });
        self.animate(&anim);
    }

    /// A new window was placed: scale 0.92 → 1 and fade in.
    pub fn window_opened(&mut self, window: &Window) {
        let Some(frame) = self.window_frame(window) else { return };
        let scale = if self.config.appearance.reduce_motion { 1.0 } else { 0.92 };
        self.move_from(window, Look::scaled(frame, scale, 0.0), Curve::Spring(SMOOTH));
    }

    /// About to zoom or un-zoom: the frame springs from here to its new size. Reduce motion: it just jumps.
    // ponytail: the snapshot is stretched until the app draws at the new size; a crossfade of old and new frames
    // looks cleaner if the stretch shows.
    pub fn window_reframing(&mut self, window: &Window) {
        if let Some(look) = self.current_look(window).filter(|_| !self.config.appearance.reduce_motion) {
            self.move_from(window, look, Curve::Spring(SMOOTH));
        }
    }

    /// Where `window` minimises to: its tile's thumbnail (the next tile while it is still shown), else its app's dock
    /// icon, or the bottom centre without a shown dock.
    // ponytail: un-minimising grows out of the last tile's place, not the one it had; the tile is gone by then.
    fn dock_target(&self, window: &Window) -> Rectangle<f64, Logical> {
        let mut items = self.dock_items();
        let tile = crate::shell::dock::is_minimised(window) && !self.config.dock.minimise_into_icon;
        if tile && !items.iter().any(|i| i.is_tile() && i.windows.contains(window)) {
            items.push(crate::shell::dock::Item { key: "minimised:next".into(), app: None, windows: vec![window.clone()] });
        }
        let frame = self.window_frame(window).map(|f| f.size);
        let icon = self.dock_layout(&items).filter(|_| self.dock_shown()).and_then(|l| {
            let i = items.iter().position(|i| i.is_tile() == tile && i.windows.contains(window))?;
            let icon = l.icons.get(i).copied()?;
            Some(match frame.filter(|_| tile) {
                Some(size) => crate::shell::dock::tile_rects(icon, size).0,
                None => icon,
            })
        });
        icon.unwrap_or_else(|| {
            let size = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map_or_else(Default::default, |g| g.size);
            Rectangle::new((f64::from(size.w) / 2.0 - 32.0, f64::from(size.h) - 64.0).into(), (64.0, 64.0).into())
        })
    }

    /// A shown window came back (un-minimised): it grows out of its dock icon, or out of its minimise ghost mid-way.
    pub fn window_restored(&mut self, window: &Window) {
        let ghost = self.backend.as_mut().and_then(|b| {
            let i = b.ghosts.iter().position(|g| g.window == *window)?;
            Some(b.ghosts.remove(i).look(Instant::now()))
        });
        let Some(frame) = self.window_frame(window) else { return };
        let from = match ghost {
            Some(look) => look,
            None if self.config.appearance.reduce_motion => Look { rect: frame, alpha: 0.0 },
            None => Look { rect: self.dock_target(window), alpha: 0.0 },
        };
        self.move_from(window, from, Curve::Spring(SMOOTH));
    }

    /// `window` is about to go: its last frame goes on as a ghost into `to` (`None`: shrink to 0.95 and fade out).
    fn leave(&mut self, window: &Window, to: Option<Rectangle<f64, Logical>>, curve: Curve) {
        let from = self.current_look(window);
        let dark = crate::render::is_dark(&self.config.appearance);
        let reduce = self.config.appearance.reduce_motion;
        let curve = self.curve(curve);
        // On the display the window is on (the first when it is on none).
        let Some(output) = self.window_output(window).or_else(|| self.space.outputs().next().cloned()) else { return };
        let (Some(backend), Some(renderer)) = (self.backend.as_mut(), self.device.as_mut().and_then(crate::backend::Device::renderer)) else { return };
        let Some(mut ghost) = crate::render::motion::Ghost::capture(renderer, &backend.shaders, &mut self.text, &self.space, window, &output, dark) else {
            return;
        };
        backend.ghosts.retain(|g| g.window != *window);
        ghost.from = from.unwrap_or(ghost.from);
        ghost.to = match to {
            _ if reduce => Look { rect: ghost.from.rect, alpha: 0.0 },
            Some(rect) => Look { rect, alpha: 0.0 },
            None => Look::scaled(ghost.frame(), 0.95, 0.0),
        };
        ghost.anim = Anim::new(curve);
        let anim = ghost.anim;
        backend.ghosts.push(ghost);
        self.animate(&anim);
    }

    /// Minimise: it shrinks and slides into its dock tile or icon; its last frame stays on as the tile's thumbnail.
    pub fn window_minimising(&mut self, window: &Window) {
        let to = self.dock_target(window);
        self.leave(window, Some(to), Curve::Spring(SMOOTH));
        let Some(backend) = self.backend.as_mut().filter(|_| crate::shell::dock::is_minimised(window)) else { return };
        backend.tiles.retain(|g| g.window != *window);
        if let Some(ghost) = backend.ghosts.iter().find(|g| g.window == *window) {
            backend.tiles.push(ghost.still());
        }
    }

    /// Close: scale 1 → 0.95 and fade out over 150 ms.
    pub fn window_closing(&mut self, window: &Window) {
        self.leave(window, None, Curve::Ease(FADE_OUT));
    }

    /// Show Desktop (E5.3): the shown Space's windows spring aside, or back.
    pub fn toggle_desktop(&mut self) {
        if self.desktop_shown.is_some() {
            self.restore_desktop();
            return;
        }
        let Some(output) = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)) else { return };
        let windows: Vec<Window> = self.space.elements().filter(|w| self.on_current(w)).cloned().collect();
        let mut saved = Vec::new();
        for window in windows {
            let Some(geo) = self.space.element_geometry(&window) else { continue };
            let bar = crate::decorations::bar_height(&window);
            let frame = Rectangle::new(geo.loc - Point::from((0, bar)), geo.size + (0, bar).into());
            self.slide_window(&window, aside(frame, output));
            saved.push((window, frame.loc));
        }
        self.desktop_shown = Some(saved);
    }

    /// Show Desktop off: every pushed-aside window still there springs back.
    pub fn restore_desktop(&mut self) {
        for (window, loc) in self.desktop_shown.take().into_iter().flatten() {
            if self.space.elements().any(|w| *w == window) {
                self.slide_window(&window, loc);
            }
        }
    }

    /// Moves `window`'s frame to `loc` with a `smooth` spring; reduce motion: it jumps.
    fn slide_window(&mut self, window: &Window, loc: Point<i32, Logical>) {
        let from = self.current_look(window).filter(|_| !self.config.appearance.reduce_motion);
        self.set_visible_loc(window, loc);
        if let Some(from) = from {
            self.move_from(window, from, Curve::Spring(SMOOTH));
        }
    }
}

/// How much of a window Show Desktop leaves on screen.
const PEEK: i32 = 16;

/// Where Show Desktop pushes `frame`: past the nearer side edge of `output`, `PEEK` px still showing.
// ponytail: sideways only; up or down would cross into other Spaces' rows (spaces.rs stacks them vertically).
fn aside(frame: crate::wm::Rect, output: crate::wm::Rect) -> Point<i32, Logical> {
    let left = frame.loc.x + frame.size.w / 2 < output.loc.x + output.size.w / 2;
    let x = if left { output.loc.x - frame.size.w + PEEK } else { output.loc.x + output.size.w - PEEK };
    (x, frame.loc.y).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peak(s: Spring) -> f64 {
        (0..2000).map(|ms| s.value(f64::from(ms) / 1000.0)).fold(0.0, f64::max)
    }

    #[test]
    fn springs_start_at_zero_and_settle() {
        for s in [SNAPPY, SMOOTH, BOUNCY] {
            assert!(s.value(0.0).abs() < 1e-9);
            let settle = s.settle();
            assert!(settle > Duration::from_millis(100) && settle < Duration::from_secs(1), "{s:?} settles in {settle:?}");
            // Settled for good: every later sample is within EPS.
            assert!((0..1000).all(|ms| (s.value(settle.as_secs_f64() + f64::from(ms) / 1000.0) - 1.0).abs() < EPS));
        }
    }

    #[test]
    fn only_underdamped_presets_overshoot() {
        assert!(peak(SMOOTH) <= 1.0, "damping 1.0 never overshoots");
        assert!(peak(SNAPPY) > 1.0 && peak(SNAPPY) < 1.01, "barely: {}", peak(SNAPPY));
        assert!(peak(BOUNCY) > 1.03, "bouncy bounces: {}", peak(BOUNCY));
        // Critically damped, it is monotonic.
        let v: Vec<f64> = (0..600).map(|ms| SMOOTH.value(f64::from(ms) / 1000.0)).collect();
        assert!(v.windows(2).all(|w| w[1] >= w[0]));
    }

    #[test]
    fn eases_end_exactly() {
        let anim = Anim { start: Instant::now(), curve: Curve::Ease(FADE_OUT) };
        assert_eq!(anim.duration(), Duration::from_millis(150));
        assert_eq!(anim.progress(anim.start), 0.0);
        let half = anim.progress(anim.start + Duration::from_millis(75));
        assert!(half > 0.5 && half < 1.0, "ease-out is past halfway at half time: {half}");
        assert_eq!(anim.progress(anim.end()), 1.0);
        assert!(anim.done(anim.end()) && !anim.done(anim.start));
    }

    #[test]
    fn looks_lerp_and_map() {
        let frame = Rectangle::new((100.0, 100.0).into(), (200.0, 100.0).into());
        let small = Look::scaled(frame, 0.5, 0.0);
        assert_eq!(small.rect, Rectangle::new((150.0, 125.0).into(), (100.0, 50.0).into()));
        let mid = small.lerp(Look::at(frame), 0.5);
        assert_eq!(mid.rect.size, (150.0, 75.0).into());
        assert_eq!(mid.alpha, 0.5);
        // The shadow area 10 px around the frame shrinks with it.
        let area = Rectangle::new((90.0, 90.0).into(), (220.0, 120.0).into());
        assert_eq!(small.map(frame, area), Rectangle::new((145.0, 120.0).into(), (110.0, 60.0).into()));
        assert_eq!(Look::at(frame).map(frame, area), area);
    }

    #[test]
    fn show_desktop_pushes_to_the_nearer_side() {
        let output = Rectangle::new((0, 0).into(), (1280, 800).into());
        let left = Rectangle::new((100, 50).into(), (400, 300).into());
        assert_eq!(aside(left, output), Point::from((16 - 400, 50)));
        let right = Rectangle::new((800, 200).into(), (400, 300).into());
        assert_eq!(aside(right, output), Point::from((1280 - 16, 200)));
    }

    #[test]
    fn entrances_start_hidden_and_end_in_place() {
        let rect = Rectangle::new((900.0, 40.0).into(), (344.0, 80.0).into());
        let t0 = Instant::now();
        for (reduce, slide) in [(false, false), (false, true), (true, true)] {
            let e = Entrance { start: t0, reduce, slide };
            assert_eq!(e.look(rect, t0).alpha, 0.0);
            assert_eq!(e.look(rect, e.end()), Look::at(rect));
        }
        let banner = Entrance { start: t0, reduce: false, slide: true };
        assert!(banner.look(rect, t0).rect.loc.x > rect.loc.x + rect.size.w, "starts past its right edge");
        let reduced = Entrance { start: t0, reduce: true, slide: true };
        assert_eq!(reduced.look(rect, t0 + Duration::from_millis(50)).rect, rect, "reduce motion: no movement");
        assert_eq!(reduced.end(), t0 + CROSSFADE);
    }
}
