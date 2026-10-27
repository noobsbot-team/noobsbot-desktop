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
/// Touchpad gestures let go: a spring from the fingers' position and speed (`Spring::release`).
pub const GESTURE: Spring = Spring { response: 0.40, damping: 0.85 };
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
        1.0 - self.release(1.0, 0.0, t)
    }

    /// Let go `x` away from its target, moving at `v` per second: how far from the target it is at `t` seconds.
    pub fn release(self, x: f64, v: f64, t: f64) -> f64 {
        let (w, z) = (self.omega(), self.damping);
        if z >= 1.0 {
            return (x + (v + w * x) * t) * (-w * t).exp();
        }
        let wd = w * (1.0 - z * z).sqrt();
        (-z * w * t).exp() * (x * (wd * t).cos() + (v + z * w * x) / wd * (wd * t).sin())
    }

    /// When it stays within `EPS` of the target: where the decay envelope drops below it.
    pub fn settle(self) -> Duration {
        self.settle_release(1.0, 0.0)
    }

    /// When a `release(x, v)` stays within `EPS` of its target.
    pub fn settle_release(self, x: f64, v: f64) -> Duration {
        let (w, z) = (self.omega(), self.damping);
        let envelope = |t: f64| {
            if z >= 1.0 {
                (x.abs() + (v + w * x).abs() * t) * (-w * t).exp()
            } else {
                let wd = w * (1.0 - z * z).sqrt();
                x.hypot((v + z * w * x) / wd) * (-z * w * t).exp()
            }
        };
        // Searched from where the envelope peaks: it only falls after.
        let peak = if z >= 1.0 && v + w * x != 0.0 { (1.0 / w - x.abs() / (v + w * x).abs()).max(0.0) } else { 0.0 };
        let ms = ((peak * 1000.0) as u32..10_000).find(|ms| envelope(f64::from(*ms) / 1000.0) < EPS).unwrap_or(10_000);
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

fn smoothstep(a: f64, b: f64, x: f64) -> f64 {
    if b - a < 1e-6 {
        return if x >= b { 1.0 } else { 0.0 };
    }
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The genie minimise (E5.5) at `q` (0: `area` as it is, 1: gone into `target`): the bottom narrows towards the
/// target first, then the rest follows it down. Each screen row shows one source row, squeezed. `genie.frag` does
/// the same maths per pixel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Genie {
    pub area: Rectangle<f64, Logical>,
    pub target: Rectangle<f64, Logical>,
    pub q: f64,
}

impl Genie {
    /// How far the narrowing has got (0..1), and the screen heights of the top and bottom rows.
    pub fn span(self) -> (f64, f64, f64) {
        let (a, t, q) = (self.area, self.target, self.q);
        let lerp = |x: f64, y: f64, k: f64| x + (y - x) * k;
        let top = lerp(a.loc.y, t.loc.y, smoothstep(0.3, 1.0, q));
        // Leads the top all the way, so the rows never fold over.
        let bottom = lerp(a.loc.y + a.size.h, t.loc.y + t.size.h, smoothstep(0.2, 0.85, q));
        (smoothstep(0.0, 0.45, q), top, bottom)
    }

    /// Left and right edges at screen height `y`: the narrower the closer to the target's top.
    pub fn edges(self, y: f64) -> (f64, f64) {
        let (a, t) = (self.area, self.target);
        let m = self.span().0 * smoothstep(a.loc.y, t.loc.y, y);
        (a.loc.x + (t.loc.x - a.loc.x) * m, a.loc.x + a.size.w + (t.loc.x + t.size.w - a.loc.x - a.size.w) * m)
    }

    /// Everything it covers: the edges move monotonically down the rows, so the top and bottom rows bound it.
    pub fn bounds(self) -> Rectangle<f64, Logical> {
        let (_, top, bottom) = self.span();
        let ((l0, r0), (l1, r1)) = (self.edges(top), self.edges(bottom));
        let (left, right) = (l0.min(l1), r0.max(r1));
        Rectangle::new((left, top).into(), (right - left, bottom - top).into())
    }
}

/// A genie playing from `q` = `from` to `to`, into `target` (where the window frame ends up).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GenieRun {
    pub target: Rectangle<f64, Logical>,
    pub from: f64,
    pub to: f64,
}

impl GenieRun {
    /// Its `q` at progress `p`.
    pub fn at(self, p: f64) -> f64 {
        self.from + (self.to - self.from) * p
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
    /// Un-minimising with the genie: drawn warped instead of at `look`.
    pub genie: Option<GenieRun>,
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

    /// The genie's target and `q` at `now`, if it is one.
    pub fn genie(&self, now: Instant) -> Option<(Rectangle<f64, Logical>, f64)> {
        self.genie.map(|g| (g.target, g.at(self.anim.progress(now))))
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

    /// `window`'s frame relative to its display, as the frame of its display draws it (render::scene).
    fn window_frame(&self, window: &Window) -> Option<Rectangle<f64, Logical>> {
        let loc = self.window_output(window).and_then(|o| self.space.output_geometry(&o)).map_or_else(|| self.first_origin(), |g| g.loc);
        motion::window_frame(&self.space, window, loc)
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
        motion::start(window, Motion { from, anim, genie: None });
        self.animate(&anim);
    }

    /// A new window was placed: scale 0.92 → 1 and fade in. The quake terminal slides down from the top edge.
    pub fn window_opened(&mut self, window: &Window) {
        let Some(frame) = self.window_frame(window) else { return };
        let reduce = self.config.appearance.reduce_motion;
        let from = match (reduce, self.is_quake(window)) {
            (true, _) => Look { rect: frame, alpha: 0.0 },
            (false, true) => Look::at(above(frame)),
            (false, false) => Look::scaled(frame, 0.92, 0.0),
        };
        self.move_from(window, from, Curve::Spring(SMOOTH));
    }

    /// About to zoom or un-zoom: the frame springs from here to its new size. Reduce motion: the old frame fades out
    /// over the new one.
    // ponytail: the snapshot is stretched until the app draws at the new size; a crossfade of old and new frames
    // looks cleaner if the stretch shows.
    pub fn window_reframing(&mut self, window: &Window) {
        if self.config.appearance.reduce_motion {
            self.leave(window, None, Curve::Ease(CROSSFADE));
        } else if let Some(look) = self.current_look(window) {
            self.move_from(window, look, Curve::Spring(SMOOTH));
        }
    }

    /// Where `window` minimises to, relative to its display: its app's icon on that display's dock; or the bottom
    /// centre without a shown dock there.
    fn dock_target(&self, window: &Window) -> Rectangle<f64, Logical> {
        let output = self.window_output(window).or_else(|| self.space.outputs().next().cloned());
        let docked = output.clone().filter(|o| self.dock_outputs().contains(o) && self.dock_shown_on(o));
        let items = self.dock_items_on(output.as_ref());
        let icon = docked.and_then(|o| self.dock_layout_on(&o, &items)).and_then(|l| {
            let i = items.iter().position(|i| i.windows.contains(window))?;
            l.icons.get(i).copied()
        });
        icon.unwrap_or_else(|| {
            let size = output.and_then(|o| self.space.output_geometry(&o)).map_or_else(Default::default, |g| g.size);
            Rectangle::new((f64::from(size.w) / 2.0 - 32.0, f64::from(size.h) - 64.0).into(), (64.0, 64.0).into())
        })
    }

    /// Minimising and restoring use the genie (E5.5); reduce motion wins.
    fn genie_on(&self) -> bool {
        self.config.dock.minimise_effect == crate::config::MinimiseEffect::Genie && !self.config.appearance.reduce_motion
    }

    /// A shown window came back (un-minimised): it grows out of its dock icon, or out of its minimise ghost mid-way.
    /// The quake terminal (E2.8) slides down from the top edge instead.
    pub fn window_restored(&mut self, window: &Window) {
        let quake = self.is_quake(window);
        let now = Instant::now();
        let ghost = self.backend.as_mut().and_then(|b| {
            let i = b.ghosts.iter().position(|g| g.window == *window)?;
            let ghost = b.ghosts.remove(i);
            Some((ghost.look(now), ghost.genie.map(|g| (g.target, g.at(ghost.anim.progress(now))))))
        });
        let Some(frame) = self.window_frame(window) else { return };
        let from = match ghost {
            Some((look, _)) => look,
            None if self.config.appearance.reduce_motion => Look { rect: frame, alpha: 0.0 },
            None if quake => Look::at(above(frame)),
            None => Look { rect: self.dock_target(window), alpha: 0.0 },
        };
        let genie = (self.genie_on() && !quake).then(|| {
            let (target, q) = ghost.and_then(|(_, g)| g).unwrap_or_else(|| (self.dock_target(window), 1.0));
            GenieRun { target, from: q, to: 0.0 }
        });
        let anim = Anim::new(self.curve(Curve::Spring(SMOOTH)));
        motion::start(window, Motion { from, anim, genie });
        self.animate(&anim);
    }

    /// `window` is about to go: its last frame goes on as a ghost to `to` (`None`: shrink to 0.95 and fade out).
    fn leave(&mut self, window: &Window, to: Option<Look>, curve: Curve) {
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
            Some(look) => look,
            None => Look::scaled(ghost.frame(), 0.95, 0.0),
        };
        ghost.anim = Anim::new(curve);
        let anim = ghost.anim;
        backend.ghosts.push(ghost);
        self.animate(&anim);
    }

    /// Minimise: it shrinks and slides into its app's dock icon.
    /// Genie: it narrows into it instead, picking up where a restore that is still playing got to.
    /// The quake terminal slides up out of the top edge instead.
    pub fn window_minimising(&mut self, window: &Window) {
        if self.is_quake(window) {
            let to = self.window_frame(window).map(above);
            self.leave(window, to.map(Look::at), Curve::Spring(SMOOTH));
            return;
        }
        let to = self.dock_target(window);
        let now = Instant::now();
        let q = motion::current(window, now).and_then(|m| m.genie(now)).map_or(0.0, |(_, q)| q);
        self.leave(window, Some(Look { rect: to, alpha: 0.0 }), Curve::Spring(SMOOTH));
        let genie = self.genie_on().then_some(GenieRun { target: to, from: q, to: 1.0 });
        if let Some(ghost) = self.backend.as_mut().and_then(|b| b.ghosts.iter_mut().find(|g| g.window == *window)) {
            ghost.genie = genie;
        }
    }

    /// Close: scale 1 → 0.95 and fade out over 150 ms.
    pub fn window_closing(&mut self, window: &Window) {
        self.leave(window, None, Curve::Ease(FADE_OUT));
    }

    /// Show Desktop (E5.3): the shown Space's windows spring aside, or back.
    pub fn toggle_desktop(&mut self) {
        let moves = self.desktop_moves();
        self.desktop_let_go(moves, true);
    }

    /// What Show Desktop does now: each shown window's frame and where it goes aside; while the desktop shows, each
    /// pushed-aside window still there and its place to come back to.
    pub fn desktop_moves(&self) -> Vec<DeskMove> {
        let frame = |window: &Window| {
            let geo = self.space.element_geometry(window)?;
            let bar = crate::decorations::bar_height(window);
            Some(Rectangle::new(geo.loc - Point::from((0, bar)), geo.size + (0, bar).into()))
        };
        if let Some(saved) = &self.desktop_shown {
            return saved.iter().filter_map(|(w, loc)| Some((w.clone(), frame(w)?.loc, *loc))).collect();
        }
        let screens: Vec<crate::wm::Rect> = self.space.outputs().filter_map(|o| self.space.output_geometry(o)).collect();
        let first = self.space.outputs().next().cloned();
        let windows = self.space.elements().filter(|w| self.on_current(w));
        windows
            .filter_map(|window| {
                let frame = frame(window)?;
                // Aside on its own display.
                let output = self.window_output(window).or_else(|| first.clone());
                let screen = output.and_then(|o| self.space.output_geometry(&o))?;
                Some((window.clone(), frame.loc, aside(frame, screen, &screens)))
            })
            .collect()
    }

    /// A spread holds Show Desktop `p` (0 → 1) of the way along `moves`.
    pub fn desktop_hold(&mut self, moves: &[DeskMove], p: f64) {
        let p = p.clamp(0.0, 1.0);
        for (window, from, to) in moves {
            if self.space.elements().any(|w| w == window) {
                self.set_visible_loc(window, (from.to_f64() + (*to - *from).to_f64().upscale(p)).to_i32_round());
            }
        }
    }

    /// Show Desktop done (`commit`), or let go short: each window still there springs to where `moves` take it, or
    /// back.
    pub fn desktop_let_go(&mut self, moves: Vec<DeskMove>, commit: bool) {
        if commit {
            self.desktop_shown = self.desktop_shown.is_none().then(|| moves.iter().map(|(w, from, _)| (w.clone(), *from)).collect());
        }
        for (window, from, to) in moves {
            if self.space.elements().any(|w| *w == window) {
                self.slide_window(&window, if commit { to } else { from });
            }
        }
    }

    /// Show Desktop off: every pushed-aside window still there springs back.
    pub fn restore_desktop(&mut self) {
        for (window, loc) in self.desktop_shown.take().into_iter().flatten() {
            if self.space.elements().any(|w| *w == window) {
                self.slide_window(&window, loc);
            }
        }
    }

    /// Moves `window`'s frame to `loc` with a `smooth` spring; reduce motion: it fades out there and in here.
    // ponytail: the fading-out copy is drawn over every window (ghosts are) for the 150 ms.
    fn slide_window(&mut self, window: &Window, loc: Point<i32, Logical>) {
        let reduce = self.config.appearance.reduce_motion;
        let from = self.current_look(window);
        if reduce {
            self.leave(window, None, Curve::Ease(CROSSFADE));
        }
        self.set_visible_loc(window, loc);
        let from = if reduce { self.window_frame(window).map(|rect| Look { rect, alpha: 0.0 }) } else { from };
        if let Some(from) = from {
            self.move_from(window, from, Curve::Spring(SMOOTH));
        }
    }
}

/// A window Show Desktop moves: from where, to where (frame locations).
pub type DeskMove = (Window, Point<i32, Logical>, Point<i32, Logical>);

/// `frame` (relative to its display) moved up just past the top edge, its shadow too: where the quake terminal hides.
fn above(frame: Rectangle<f64, Logical>) -> Rectangle<f64, Logical> {
    Rectangle::new((frame.loc.x, -frame.size.h - 48.0).into(), frame.size)
}

/// How much of a window Show Desktop leaves on screen.
const PEEK: i32 = 16;

/// Where Show Desktop pushes `frame`: past the nearer side edge of `output`, `PEEK` px still showing, unless another of
/// `screens` lies there (it would show the window): then past the other side.
// ponytail: sideways only; up or down would cross into other Spaces' rows (spaces.rs stacks them vertically). A
// display with neighbours on both sides gets the nearer side anyway.
fn aside(frame: crate::wm::Rect, output: crate::wm::Rect, screens: &[crate::wm::Rect]) -> Point<i32, Logical> {
    let at = |left: bool| -> Point<i32, Logical> {
        let x = if left { output.loc.x - frame.size.w + PEEK } else { output.loc.x + output.size.w - PEEK };
        (x, frame.loc.y).into()
    };
    let free = |loc: Point<i32, Logical>| !screens.iter().any(|s| *s != output && s.overlaps(Rectangle::new(loc, frame.size)));
    let near = frame.loc.x + frame.size.w / 2 < output.loc.x + output.size.w / 2;
    [at(near), at(!near)].into_iter().find(|&loc| free(loc)).unwrap_or(at(near))
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
    fn released_springs_start_where_let_go_and_settle() {
        for s in [SNAPPY, SMOOTH, BOUNCY, GESTURE] {
            for (x, v) in [(-0.6, 0.0), (-0.6, 3.0), (0.2, -2.0), (0.0, 1.0)] {
                assert!((s.release(x, v, 0.0) - x).abs() < 1e-9);
                // Its starting speed is `v`.
                assert!(((s.release(x, v, 1e-6) - x) / 1e-6 - v).abs() < 1e-3, "{s:?} {x} {v}");
                let settle = s.settle_release(x, v).as_secs_f64();
                assert!(settle < 2.0 && (0..1000).all(|ms| s.release(x, v, settle + f64::from(ms) / 1000.0).abs() < EPS));
            }
        }
        // Critically damped and let go with no speed: never past the target.
        assert!((0..1000).all(|ms| SMOOTH.release(-0.6, 0.0, f64::from(ms) / 1000.0) <= 0.0));
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
    fn genie_narrows_bottom_first_into_its_target() {
        let area = Rectangle::new((100.0, 100.0).into(), (600.0, 400.0).into());
        let target = Rectangle::new((380.0, 740.0).into(), (40.0, 40.0).into());
        let at = |q| Genie { area, target, q };
        assert_eq!(at(0.0).bounds(), area);
        assert_eq!(at(0.0).edges(450.0), (100.0, 700.0));
        assert_eq!(at(1.0).bounds(), target);
        assert_eq!(at(1.0).edges(760.0), (380.0, 420.0));
        for q in (0..=100).map(|i| f64::from(i) / 100.0) {
            let (squeeze, top, bottom) = at(q).span();
            assert!(top < bottom && (0.0..=1.0).contains(&squeeze), "rows never fold over at {q}");
            let b = at(q).bounds();
            let (l, r) = at(q).edges(bottom);
            assert!(l >= b.loc.x && r <= b.loc.x + b.size.w && l < r);
        }
        // Mid-way the top row is still wider than the bottom one, which is heading for the target.
        let (_, top, bottom) = at(0.4).span();
        let (wide, narrow) = (at(0.4).edges(top), at(0.4).edges(bottom));
        assert!(wide.1 - wide.0 > narrow.1 - narrow.0 + 100.0, "{wide:?} vs {narrow:?}");
        let run = GenieRun { target, from: 0.3, to: 0.0 };
        assert_eq!((run.at(0.0), run.at(1.0)), (0.3, 0.0));
    }

    #[test]
    fn quake_hides_above_the_top_edge() {
        let frame = Rectangle::new((0.0, 30.0).into(), (1280.0, 320.0).into());
        let up = above(frame);
        assert_eq!((up.size, up.loc.x), (frame.size, frame.loc.x));
        assert!(up.loc.y + up.size.h < 0.0, "fully off the top, shadow and all");
    }

    #[test]
    fn show_desktop_pushes_to_the_nearer_side() {
        let output = Rectangle::new((0, 0).into(), (1280, 800).into());
        let left = Rectangle::new((100, 50).into(), (400, 300).into());
        assert_eq!(aside(left, output, &[output]), Point::from((16 - 400, 50)));
        let right = Rectangle::new((800, 200).into(), (400, 300).into());
        assert_eq!(aside(right, output, &[output]), Point::from((1280 - 16, 200)));
    }

    #[test]
    fn show_desktop_stays_off_other_displays() {
        let (main, second) = (Rectangle::new((0, 0).into(), (1280, 800).into()), Rectangle::new((1280, 0).into(), (1920, 1080).into()));
        let screens = [main, second];
        // On the second display, nearer its left side: the main display is there, so it goes right.
        let near_left = Rectangle::new((1400, 50).into(), (400, 300).into());
        assert_eq!(aside(near_left, second, &screens), Point::from((1280 + 1920 - 16, 50)));
        let near_right = Rectangle::new((2600, 50).into(), (400, 300).into());
        assert_eq!(aside(near_right, second, &screens), Point::from((1280 + 1920 - 16, 50)));
        // On the main one, nearer its right side: the second display is there, so it goes left.
        let main_right = Rectangle::new((900, 50).into(), (300, 200).into());
        assert_eq!(aside(main_right, main, &screens), Point::from((16 - 300, 50)));
        // A display left of the main one, with a negative origin.
        let left = Rectangle::new((-1920, 0).into(), (1920, 1080).into());
        let on_left = Rectangle::new((-1000, 50).into(), (400, 300).into());
        assert_eq!(aside(on_left, left, &[main, left]), Point::from((-1920 - 400 + 16, 50)));
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
