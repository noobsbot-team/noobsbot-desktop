//! The dock's timed behaviour (E3.1), with Plank's own values (its Transparent-Tight theme and internal constants)
//! so it feels like Plank: the hide modes and their delays, the hide slide and zoom easing, the launch and urgent
//! bounces, the pulsing urgent glow at the edge of a hidden dock, click and hover shading, icons sliding when they
//! move or arrive, the poof of an icon dragged off; plus LauncherEntry badges. Frames are drawn only while something
//! here moves, and a timer wakes the dock only for a real deadline (a hide delay).

use std::{
    collections::HashMap,
    f64::consts::PI,
    time::{Duration, Instant},
};

use smithay::{
    desktop::Window,
    output::Output,
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel,
    utils::{IsAlive, Logical, Point, Rectangle},
};

use super::dock::{Item, Layout};
use crate::{config::DockHide, services::launcher::Entry, state::State, wm::Rect};

/// LaunchBounceTime, LaunchBounceHeight (× icon).
const LAUNCH_TIME: Duration = Duration::from_millis(600);
const LAUNCH_HEIGHT: f64 = 0.625;
/// UrgentBounceTime, UrgentBounceHeight (× icon).
const URGENT_TIME: Duration = Duration::from_millis(600);
const URGENT_HEIGHT: f64 = 1.666_666_666_666_666_7;
/// GlowSize 30 (tenths of the icon size), GlowTime, GlowPulseTime, UrgentHueShift.
const GLOW_SIZE: f64 = 3.0;
const GLOW_TIME: Duration = Duration::from_secs(10);
const GLOW_PULSE: Duration = Duration::from_secs(2);
const HUE_SHIFT: f64 = 150.0;
/// HideTime (FadeOpacity 1: the dock slides, no fade).
pub const HIDE_TIME: Duration = Duration::from_millis(250);
/// Plank's DOCK_ZOOM_DURATION.
pub const ZOOM_TIME: Duration = Duration::from_millis(200);
/// ClickTime; Plank's ITEM_SCROLL_DURATION is the same.
pub const CLICK_TIME: Duration = Duration::from_millis(300);
/// Plank's ITEM_HOVER_DURATION: the hovered icon lightens by HOVER_LIGHTEN over it.
const HOVER_TIME: Duration = Duration::from_millis(150);
const HOVER_LIGHTEN: f64 = 0.2;
/// ItemMoveTime: icons sliding to a new place, or rising into the dock.
const MOVE_TIME: Duration = Duration::from_millis(450);
/// Plank's poof (PoofWindow RUN_LENGTH).
const POOF_TIME: Duration = Duration::from_millis(300);
/// Plank's HideManager UPDATE_TIMEOUT: windows settle this long before the dock looks at them.
const SETTLE: Duration = Duration::from_millis(200);
/// Plank's HOVER_DELAY_TIME before the tooltip.
pub const TIP_DELAY: Duration = Duration::from_millis(200);
/// Super held alone this long shows the Super+n number pills (longer than a drawer tap).
const NUMBERS_DELAY: Duration = Duration::from_millis(600);

fn progress(t: Duration, of: Duration) -> f64 {
    t.as_secs_f64() / of.as_secs_f64()
}

/// How far item `key` is faded in (0..1) over `time`: in since it became `on`, out since it became `off`.
fn fade(on: &Option<(String, Instant)>, off: &Option<(String, Instant)>, key: &str, now: Instant, time: Duration) -> f64 {
    let p = |at: Instant| progress(now - at, time).min(1.0);
    match (on, off) {
        (Some((k, at)), _) if k == key => p(*at),
        (_, Some((k, at))) if k == key => 1.0 - p(*at),
        _ => 0.0,
    }
}

fn cubic_in(t: f64) -> f64 {
    t * t * t
}

fn cubic_out(t: f64) -> f64 {
    1.0 - cubic_in(1.0 - t)
}

/// Plank's bounce: `n` hops over `of`, the last ones damped.
fn bounce(t: Duration, of: Duration, n: f64) -> f64 {
    let p = progress(t, of);
    if p >= 1.0 {
        return 0.0;
    }
    ((n * PI * p).sin() * ((1.0 - p) * 2.0 * n / (2.0 * n - 1.0)).min(1.0)).abs()
}

/// Plank's launch bounce, px above the baseline `t` after the launch: two hops, the second lower.
pub fn launch_bounce(t: Duration, icon: f64) -> f64 {
    bounce(t, LAUNCH_TIME, 2.0) * (LAUNCH_HEIGHT * icon).trunc()
}

/// Plank's urgent bounce: one high hop.
pub fn urgent_bounce(t: Duration, icon: f64) -> f64 {
    bounce(t, URGENT_TIME, 1.0) * (URGENT_HEIGHT * icon).trunc()
}

/// The click (and scroll) shading `t` after it: darkens and comes back over ClickTime, at most by half.
pub fn click_darken(t: Duration) -> f64 {
    let p = progress(t, CLICK_TIME);
    if p >= 1.0 { 0.0 } else { (PI * p).sin().max(0.0) * 0.5 }
}

/// The urgent glow's opacity `t` after the app asked, pulsing between 0.2 and 0.95; `None` once GlowTime is over.
pub fn glow_opacity(t: Duration) -> Option<f32> {
    (t < GLOW_TIME).then(|| (0.2 + 0.75 * ((2.0 * PI * progress(t, GLOW_PULSE)).sin() + 1.0) / 2.0) as f32)
}

/// The glow's colour: the icon's average colour (premultiplied RGBA) turned UrgentHueShift degrees, kept vivid.
pub fn glow_colour(pixels: &[u8]) -> [f32; 3] {
    let (mut sum, mut alpha) = ([0.0f64; 3], 0.0);
    for p in pixels.as_chunks::<4>().0 {
        (0..3).for_each(|c| sum[c] += f64::from(p[c]));
        alpha += f64::from(p[3]);
    }
    let [r, g, b] = if alpha > 0.0 { sum.map(|s| s / alpha) } else { [1.0; 3] };
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let hue = match max - min {
        0.0 => 0.0,
        d if max == r => 60.0 * ((g - b) / d).rem_euclid(6.0),
        d if max == g => 60.0 * ((b - r) / d + 2.0),
        d => 60.0 * ((r - g) / d + 4.0),
    };
    // ponytail: saturation floored at 0.6 and value at 1, so grey icons still glow in a colour; Plank's exact
    // brightening of the average isn't copied.
    let s = if max > 0.0 { ((max - min) / max).max(0.6) } else { 0.6 };
    let h = (hue + HUE_SHIFT).rem_euclid(360.0) / 60.0;
    let x = 1.0 - (h.rem_euclid(2.0) - 1.0).abs();
    let (r, g, b) = match h as u8 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    [r, g, b].map(|c| (1.0 - s + s * c) as f32)
}

/// 0 → 1 over `time` with Plank's cubic easing, turning back mid-way when the target flips (Plank mirrors the
/// elapsed time). `rise_in`: ease in on the way up and out on the way down (the hide slide), else the other way
/// round (the zoom).
#[derive(Debug, Clone, Copy)]
pub struct Ease {
    on: bool,
    since: Option<Instant>,
    time: Duration,
    rise_in: bool,
}

impl Ease {
    pub const fn new(time: Duration, rise_in: bool) -> Self {
        Self { on: false, since: None, time, rise_in }
    }

    pub fn value(&self, now: Instant) -> f64 {
        let t = self.since.map_or(1.0, |s| progress(now.saturating_duration_since(s), self.time).min(1.0));
        match (self.on, self.rise_in) {
            (true, true) => cubic_in(t),
            (true, false) => cubic_out(t),
            (false, true) => 1.0 - cubic_out(t),
            (false, false) => 1.0 - cubic_in(t),
        }
    }

    pub fn set(&mut self, on: bool, now: Instant) {
        if on == self.on {
            return;
        }
        let elapsed = self.since.map_or(self.time, |s| now.saturating_duration_since(s));
        self.on = on;
        self.since = Some(if elapsed < self.time { now.checked_sub(self.time - elapsed).unwrap_or(now) } else { now });
    }

    pub fn moving(&self, now: Instant) -> bool {
        self.since.is_some_and(|s| now.saturating_duration_since(s) < self.time)
    }
}

/// What the windows under the dock are doing: Plank's intersect flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Overlaps {
    /// Any window.
    pub any: bool,
    /// A window of the focused app (same dock item).
    pub app: bool,
    /// The focused window.
    pub active: bool,
    /// The focused window, maximised.
    pub maximised: bool,
    /// A dialog of the focused app.
    pub dialog: bool,
}

/// Whether `mode` wants the dock hidden: Plank's HideManager.update_hidden. The pointer on the dock always shows it.
pub fn hides(mode: DockHide, hovered: bool, o: Overlaps) -> bool {
    !hovered
        && match mode {
            DockHide::Never => false,
            DockHide::Auto => true,
            DockHide::Intelligent => o.app,
            DockHide::DodgeMaximized => o.maximised || o.dialog,
            DockHide::WindowDodge => o.any,
            DockHide::DodgeActive => o.active,
        }
}

/// The dock's hidden state over time: Plank's hide and unhide delays, and the settle time after windows change.
#[derive(Debug, Default, Clone, Copy)]
pub struct Hider {
    pub hidden: bool,
    /// A delayed change: the hidden state it sets, and when.
    pending: Option<(bool, Instant)>,
    /// The overlaps acted on; newer ones wait out SETTLE from `settling`.
    seen: Overlaps,
    settling: Option<Instant>,
}

impl Hider {
    /// The overlaps to act on now, given what the windows do now; and when to look again if they are settling.
    pub fn settle(&mut self, now_seen: Overlaps, now: Instant) -> (Overlaps, Option<Instant>) {
        if now_seen == self.seen {
            self.settling = None;
            return (self.seen, None);
        }
        let at = *self.settling.get_or_insert(now + SETTLE);
        if now < at {
            return (self.seen, Some(at));
        }
        (self.seen, self.settling) = (now_seen, None);
        (self.seen, None)
    }

    /// Moves towards `want` (hidden or not): at once, or after `hide_delay` (hiding) or `unhide_delay` (shown by the
    /// pointer, Plank's only delayed show). Returns when to look again while a delay runs.
    pub fn update(&mut self, want: bool, by_pointer: bool, hide_delay: Duration, unhide_delay: Duration, now: Instant) -> Option<Instant> {
        if want == self.hidden {
            self.pending = None;
            return None;
        }
        let delay = match (want, by_pointer) {
            (true, _) => hide_delay,
            (false, true) => unhide_delay,
            (false, false) => Duration::ZERO,
        };
        let at = match self.pending {
            Some((w, at)) if w == want => at,
            _ => now + delay,
        };
        if now >= at {
            (self.hidden, self.pending) = (want, None);
            None
        } else {
            self.pending = Some((want, at));
            Some(at)
        }
    }
}

/// Plank's move easing for an icon sliding `t` into its new place: what is left of the way, 1 → 0.
fn move_left(t: Duration) -> f64 {
    let p = progress(t, MOVE_TIME).min(1.0);
    // EASE_OUT_CIRC.
    1.0 - (1.0 - (p - 1.0) * (p - 1.0)).sqrt()
}

/// One display's dock: its hidden state and its zoom.
#[derive(Debug, Clone, Copy)]
pub struct Side {
    /// 1 = hidden.
    pub hide: Ease,
    /// 1 = zoomed in.
    pub zoom: Ease,
    pub hider: Hider,
    /// The last pointer position over the dock, which the zoom eases out around after the pointer left.
    pub zoom_at: Option<Point<f64, Logical>>,
}

impl Default for Side {
    fn default() -> Self {
        Self { hide: Ease::new(HIDE_TIME, true), zoom: Ease::new(ZOOM_TIME, false), hider: Hider::default(), zoom_at: None }
    }
}

#[derive(Debug, Default)]
pub struct Fx {
    /// Each display's dock, by output name.
    sides: HashMap<String, Side>,
    /// The deadline a timer is set for, so one is set once.
    timer: Option<Instant>,
    /// Item key → when it was launched from the dock.
    launched: HashMap<String, Instant>,
    /// Item key → when it was clicked to focus or scrolled (shaded), and when it last switched windows by scrolling.
    clicked: HashMap<String, Instant>,
    pub scrolled: HashMap<String, Instant>,
    /// The hovered item and the one before it, and since when (the lighten fades in and out).
    hover: Option<(String, Instant)>,
    unhover: Option<(String, Instant)>,
    /// A click or scroll hid the tooltip until the pointer moves to another icon.
    pub tip_hidden: bool,
    /// Windows that asked for attention, and since when.
    urgent: Vec<(Window, Instant)>,
    /// LauncherEntry state by app id, and since when each app has been urgent.
    entries: HashMap<String, (Entry, Option<Instant>)>,
    /// Last frame's item keys in order; icons that moved (slots left to go, since) or arrived (since).
    order: Vec<String>,
    moves: HashMap<String, (f64, Instant)>,
    added: HashMap<String, Instant>,
    /// Icons dragged off the dock: where they went up in smoke, and when.
    poofs: Vec<(Point<f64, Logical>, Instant)>,
}

/// How one icon is drawn this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shade {
    pub alpha: f32,
    /// 0..1: towards brighter (Plank adds the icon to itself), or black.
    pub lighten: f32,
    pub darken: f32,
}

/// What the dock draws this frame.
#[derive(Debug, Default)]
pub struct Frame {
    /// Slid, bounced and with icons in motion; `None` while fully hidden.
    pub layout: Option<Layout>,
    /// Per icon.
    pub shades: Vec<Shade>,
    /// Glow circles at the screen edge, front to back, with their colour and opacity.
    pub glows: Vec<(Rect, [f32; 4])>,
    /// Item index and its badge and progress.
    pub badges: Vec<(usize, Entry)>,
    /// The hovered app's name, and the number pills while Super is held: words centred over a point (bottom edge).
    pub tips: Vec<(String, Point<i32, Logical>)>,
    /// Poofs: where, and how far along (0..1).
    pub poofs: Vec<(Point<f64, Logical>, f64)>,
    /// Draw another frame.
    pub animating: bool,
}

impl Fx {
    /// The dock on display `output`.
    pub fn side(&self, output: &str) -> Side {
        self.sides.get(output).copied().unwrap_or_default()
    }

    pub fn side_mut(&mut self, output: &str) -> &mut Side {
        self.sides.entry(output.to_owned()).or_default()
    }

    /// A LauncherEntry update.
    pub fn entry(&mut self, app: String, entry: Entry) {
        let since = self.entries.get(&app).and_then(|(e, since)| since.filter(|_| e.urgent));
        let since = entry.urgent.then(|| since.unwrap_or_else(Instant::now));
        if entry == Entry::default() {
            self.entries.remove(&app);
        } else {
            self.entries.insert(app, (entry, since));
        }
    }

    pub fn launched(&mut self, key: String) {
        self.launched.insert(key, Instant::now());
    }

    pub fn clicked(&mut self, key: String) {
        self.clicked.insert(key, Instant::now());
        self.tip_hidden = true;
    }

    pub fn poof(&mut self, at: Point<f64, Logical>) {
        self.poofs.push((at, Instant::now()));
    }

    pub fn urgent(&mut self, window: Window) {
        if !self.urgent.iter().any(|(w, _)| *w == window) {
            self.urgent.push((window, Instant::now()));
        }
    }

    /// The urgent windows of `windows`.
    pub fn urgent_of(&self, windows: &[Window]) -> Vec<Window> {
        self.urgent.iter().filter(|(w, _)| windows.contains(w)).map(|(w, _)| w.clone()).collect()
    }

    /// The pointer is over item `key` (or none): its lighten fades in, the last one's fades out.
    pub fn hovered(&mut self, key: Option<&str>) {
        if self.hover.as_ref().map(|(k, _)| k.as_str()) == key {
            return;
        }
        let now = Instant::now();
        self.unhover = self.hover.take().map(|(k, _)| (k, now));
        self.hover = key.map(|k| (k.to_owned(), now));
        self.tip_hidden = false;
    }

    pub fn hovered_key(&self) -> Option<&str> {
        self.hover.as_ref().map(|(k, _)| k.as_str())
    }

    /// Notes this frame's order: icons that changed place slide from the old one, new icons rise in.
    fn track(&mut self, keys: &[&str], now: Instant) {
        if !self.order.is_empty() {
            for (i, key) in keys.iter().enumerate() {
                match self.order.iter().position(|k| k == key) {
                    Some(old) if old != i => {
                        let left = self.moves.get(*key).map_or(0.0, |(d, at)| d * move_left(now - *at));
                        self.moves.insert((*key).to_owned(), (left + old as f64 - i as f64, now));
                    }
                    Some(_) => {}
                    None => {
                        self.added.insert((*key).to_owned(), now);
                    }
                }
            }
        }
        self.order = keys.iter().map(|k| (*k).to_owned()).collect();
        self.moves.retain(|_, (_, at)| now - *at < MOVE_TIME);
        self.added.retain(|_, at| now - *at < MOVE_TIME);
    }

    /// The lighten of item `key`: Plank's hover fade.
    fn lighten(&self, key: &str, now: Instant) -> f64 {
        HOVER_LIGHTEN * fade(&self.hover, &self.unhover, key, now, HOVER_TIME)
    }

    /// Asks for a wake-up at `at` (a hide delay or the window settle time): a one-shot timer, set once per deadline.
    pub fn wake_at(&mut self, at: Instant) -> Option<Duration> {
        if self.timer.is_some_and(|t| t <= at && t > Instant::now()) {
            return None;
        }
        self.timer = Some(at);
        Some(at.saturating_duration_since(Instant::now()))
    }
}

impl Layout {
    /// The whole dock moved down by `dy`.
    fn slid(mut self, dy: f64) -> Self {
        let d = dy.round() as i32;
        self.icons.iter_mut().for_each(|r| r.loc.y += dy);
        self.zone.loc.y += d;
        self
    }
}

impl State {
    /// The app id a LauncherEntry names `item` by: its key, or anything `Apps::find` resolves to it.
    /// `[notification_apps]` turns the dock item `key`'s badge off: a rule by its name, its desktop id, or the
    /// name its notifications carry along with that id.
    fn badge_off(&self, key: &str) -> bool {
        let name = self.apps.find(key).map(|a| a.name.as_str());
        let sends = |app: &str| self.notifications.list.iter().any(|n| n.app == app && n.desktop_entry == key);
        self.config.notification_apps.iter().any(|(app, rule)| !rule.badge && (app == key || Some(app.as_str()) == name || sends(app)))
    }

    fn entry_for(&self, item: &Item) -> Option<&(Entry, Option<Instant>)> {
        let fx = &self.dock_fx;
        fx.entries.get(&item.key).or_else(|| fx.entries.iter().find(|(k, _)| self.apps.find(k).is_some_and(|a| a.id == item.key)).map(|(_, e)| e))
    }

    /// What the windows do under the dock's `zone` (relative to `output`), for the hide modes.
    pub fn dock_overlaps(&self, output: &Output, zone: Rect) -> Overlaps {
        let focused = self.focused_window();
        // By dock item, not process: every X11 window shares xwayland-satellite's pid.
        let key = focused.as_ref().and_then(|w| self.dock_key(w)).map(|(k, _)| k);
        let origin = self.space.output_geometry(output).map_or_else(Default::default, |g| g.loc);
        let mut o = Overlaps::default();
        for window in self.space.elements() {
            let Some(f) = self.frame(window) else { continue };
            if !Rectangle::new(f.loc - origin, f.size).overlaps(zone) {
                continue;
            }
            o.any = true;
            if key.is_none() || self.dock_key(window).map(|(k, _)| k) != key {
                continue;
            }
            o.app = true;
            let toplevel = window.toplevel();
            o.dialog |= toplevel.is_some_and(|t| t.parent().is_some());
            if focused.as_ref() == Some(window) {
                o.active = true;
                o.maximised = toplevel.is_some_and(|t| t.current_state().states.contains(xdg_toplevel::State::Maximized));
            }
        }
        o
    }

    /// A redraw at `at`: a one-shot timer, so nothing polls while the dock waits out a delay.
    pub(super) fn dock_wake(&mut self, at: Instant) {
        let Some(wait) = self.dock_fx.wake_at(at) else { return };
        let timer = smithay::reexports::calloop::timer::Timer::from_duration(wait);
        let _ = self.loop_handle.insert_source(timer, |_, _, state| {
            state.dock_fx.timer = None;
            state.schedule_redraw();
            smithay::reexports::calloop::timer::TimeoutAction::Drop
        });
    }

    /// Brings `output`'s dock's hidden state up to date (the hide mode, delays and settle time) and sets a timer for
    /// the next deadline. `true` if it changed.
    pub fn dock_update_hide(&mut self, output: &Output, layout: &Layout) -> bool {
        let now = Instant::now();
        let focused = self.focused_window();
        // Fullscreen on another display leaves this dock alone.
        let fullscreen = focused.is_some_and(|w| {
            w.toplevel().is_some_and(|t| t.current_state().states.contains(xdg_toplevel::State::Fullscreen))
                && self.space.outputs_for_element(&w).contains(output)
        });
        // The pointer is on the dock it last moved over (`dock_output`).
        let reveal = self.dock_reveal && self.dock_output().as_ref() == Some(output);
        let name = output.name();
        let menu = self.popup == Some(crate::shell::Popup::Menu(crate::shell::Menu::Dock));
        // Plank shows the dock while its menu is open or an icon is dragged; Launchpad and fullscreen are ours.
        let (want, settle) = if self.launchpad.is_some() || menu || self.dock_ui.dragging() {
            (false, None)
        } else if fullscreen {
            (true, None)
        } else {
            let seen = self.dock_overlaps(output, layout.zone);
            let (overlaps, settle) = self.dock_fx.side_mut(&name).hider.settle(seen, now);
            (hides(self.config.dock.hide, reveal, overlaps), settle)
        };
        let dock = &self.config.dock;
        let ms = |v: u32| Duration::from_millis(u64::from(v));
        let side = self.dock_fx.side_mut(&name);
        let was = side.hider.hidden;
        let delay = side.hider.update(want, reveal, ms(dock.hide_delay), ms(dock.unhide_delay), now);
        side.hide.set(side.hider.hidden, now);
        let changed = was != side.hider.hidden;
        if let Some(at) = settle.into_iter().chain(delay).min() {
            self.dock_wake(at);
        }
        changed
    }

    /// Steps `output`'s dock's animations to now: slides it towards shown or hidden, bounces launching and urgent
    /// icons, shades clicked and hovered ones, slides moved ones, pulses the urgent glow while hidden. Focusing an app
    /// ends its urgency. Bounces, shades, moves and badges are shared by every dock; the hover, tooltip and poofs
    /// show on the one the pointer is on (`dock_output`).
    pub fn dock_frame(&mut self, output: &Output, items: &[Item]) -> Frame {
        let now = Instant::now();
        let mut frame = Frame::default();
        let here = self.dock_output().as_ref() == Some(output);
        self.dock_fx.poofs.retain(|(_, at)| now - *at < POOF_TIME);
        if here {
            frame.poofs = self.dock_fx.poofs.iter().map(|(p, at)| (*p, progress(now - *at, POOF_TIME))).collect();
        }
        let Some(layout) = self.dock_layout_on(output, items).filter(|_| !items.is_empty()) else {
            frame.animating = !frame.poofs.is_empty();
            return frame;
        };
        self.dock_update_hide(output, &layout);
        let focused = self.focused_window();
        let side = self.dock_fx.side(&output.name());
        let fx = &mut self.dock_fx;
        fx.urgent.retain(|(w, _)| w.alive() && focused.as_ref() != Some(w));
        fx.launched.retain(|_, at| now - *at < LAUNCH_TIME);
        fx.clicked.retain(|_, at| now - *at < CLICK_TIME);
        let keys: Vec<&str> = items.iter().map(|i| i.key.as_str()).collect();
        fx.track(&keys, now);
        let hide = side.hide.value(now);
        let hover_fading = here && [&fx.hover, &fx.unhover].iter().any(|h| h.as_ref().is_some_and(|(_, at)| now - *at < HOVER_TIME));
        let mut animating = side.hide.moving(now)
            || side.zoom.moving(now)
            || !fx.launched.is_empty()
            || !fx.clicked.is_empty()
            || !fx.moves.is_empty()
            || !fx.added.is_empty()
            || !frame.poofs.is_empty()
            || hover_fading;

        let urgent_since: Vec<Option<Instant>> = items
            .iter()
            .map(|item| {
                let windows = self.dock_fx.urgent.iter().filter(|(w, _)| item.windows.contains(w)).map(|(_, at)| *at);
                windows.chain(self.entry_for(item).and_then(|(_, since)| *since)).min()
            })
            .collect();
        let bottom = f64::from(layout.zone.loc.y + layout.zone.size.h);
        for (i, item) in items.iter().enumerate() {
            let Some(since) = urgent_since[i] else { continue };
            let age = now - since;
            if hide < 1.0 {
                animating |= age < URGENT_TIME;
            } else if let Some(opacity) = glow_opacity(age) {
                // A soft circle of GlowSize at the screen edge under the icon: four discs, smaller ones more opaque.
                // ponytail: stacked discs stand in for Plank's radial gradient.
                animating = true;
                let [r, g, b] = self.icons.get(item.icon(), 16).map_or([1.0; 3], |p| glow_colour(&p.pixels));
                let x = (layout.columns[i].0 + layout.columns[i].1) / 2.0;
                for k in [1.0, 0.75, 0.5, 0.25] {
                    let radius = (GLOW_SIZE * layout.icon * k / 2.0).round();
                    let rect =
                        Rectangle::new(((x - radius).round() as i32, (bottom - radius) as i32).into(), ((2.0 * radius) as i32, (2.0 * radius) as i32).into());
                    frame.glows.insert(0, (rect, [r, g, b, opacity * 0.3]));
                }
            }
        }
        if hide < 1.0 {
            let reach = bottom - layout.icons.iter().map(|r| r.loc.y).fold(f64::from(layout.zone.loc.y), f64::min);
            let mut layout = layout.slid(hide * reach);
            let step = layout.icon + super::dock::item_pad(layout.icon);
            let fx = &self.dock_fx;
            let menu = self.dock_ui.menu.as_ref().filter(|_| here && self.popup == Some(crate::shell::Popup::Menu(crate::shell::Menu::Dock))).map(|(k, _)| k);
            for (i, item) in items.iter().enumerate() {
                let rect = &mut layout.icons[i];
                let launch = fx.launched.get(&item.key).map_or(0.0, |at| launch_bounce(now - *at, layout.icon));
                let urgent = urgent_since[i].map_or(0.0, |at| urgent_bounce(now - at, layout.icon));
                rect.loc.y -= launch.max(urgent);
                if let Some((slots, at)) = fx.moves.get(&item.key) {
                    let dx = slots * step * move_left(now - *at);
                    rect.loc.x += dx;
                    layout.columns[i].0 += dx;
                    layout.columns[i].1 += dx;
                }
                let lighten = if here { fx.lighten(&item.key, now) } else { 0.0 };
                let mut shade = Shade { alpha: 1.0, lighten: lighten as f32, darken: 0.0 };
                // Arriving: it rises from below the screen edge and fades in (EASE_IN_EXPO).
                if let Some(at) = fx.added.get(&item.key) {
                    let p = progress(now - *at, MOVE_TIME).min(1.0);
                    rect.loc.y += (1.0 - p) * (rect.size.h + super::dock::bottom_pad(layout.icon));
                    shade.alpha = if p >= 1.0 { 1.0 } else { 2f64.powf(10.0 * (p - 1.0)) as f32 };
                }
                shade.darken = fx.clicked.get(&item.key).map_or(0.0, |at| click_darken(now - *at)) as f32;
                if menu == Some(&item.key) {
                    shade.darken += 0.4;
                }
                if self.dock_ui.dragged() == Some(item.key.as_str()) {
                    shade.alpha = 0.0;
                }
                frame.shades.push(shade);
            }
            // Plank's tooltip: after HOVER_DELAY_TIME on one icon, centred over it, (zoomed size − half the icon) above
            // its resting centre; not while the dock moves, a menu is open or an icon is dragged.
            let shown = self.config.dock.tooltips
                && here
                && !fx.tip_hidden
                && hide == 0.0
                && menu.is_none()
                && !self.dock_ui.dragging()
                && self.dock_ui.peek.as_ref().is_none_or(|p| !p.shown());
            let hovered = fx.hover.clone().filter(|_| shown);
            if let Some((key, at)) = hovered
                && let Some(i) = items.iter().position(|i| i.key == key)
            {
                if now - at >= TIP_DELAY {
                    let zoom = if self.config.dock.zoom { f64::from(self.config.dock.zoom_percent) / 100.0 } else { 1.0 };
                    let centre = f64::from(layout.zone.loc.y) + layout.icon / 2.0;
                    let x = (layout.columns[i].0 + layout.columns[i].1) / 2.0;
                    let y = centre - ((layout.icon * zoom).round() - layout.icon / 2.0);
                    frame.tips.push((items[i].title().to_owned(), (x.round() as i32, y.round() as i32).into()));
                } else {
                    self.dock_wake(at + TIP_DELAY);
                }
            }
            // Super held alone: 1…9 over the first nine icons, for Super+n (E8.3).
            if let Some(down) = self.super_tap.down() {
                if now - down >= NUMBERS_DELAY {
                    for (i, rect) in layout.icons.iter().enumerate().take(9) {
                        let x = (layout.columns[i].0 + layout.columns[i].1) / 2.0;
                        // Half a pill below the icon's centre, so the pill sits on it.
                        let y = rect.loc.y + rect.size.h / 2.0 + 14.0;
                        frame.tips.push(((i + 1).to_string(), (x.round() as i32, y.round() as i32).into()));
                    }
                } else {
                    self.dock_wake(down + NUMBERS_DELAY);
                }
            }
            frame.layout = Some(layout);
        }
        frame.badges =
            items.iter().enumerate().filter(|(_, item)| !self.badge_off(&item.key)).filter_map(|(i, item)| Some((i, self.entry_for(item)?.0))).collect();
        frame.animating = animating;
        frame
    }

    /// Minimises `windows`: into tiles at the dock's right end, or (`[dock] minimise_into_icon`) their app's icon.
    pub fn minimise(&mut self, windows: Vec<Window>) {
        windows.iter().for_each(|w| super::dock::set_minimised(w, true));
        self.hide_windows(windows);
    }

    /// `--show dock-badges`: sample counts and progress bars on the default pins.
    pub fn sample_badges(&mut self) {
        let samples =
            [("org.kde.dolphin", Some(3), None), ("org.kde.kate", None, Some(0.6)), ("noobsbot", Some(12), Some(0.35)), ("helium-browser", Some(128), None)];
        for (app, count, progress) in samples {
            self.dock_fx.entry(app.into(), Entry { count, progress, urgent: false });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn launch_bounce_hops_twice_and_lands() {
        assert_eq!(launch_bounce(ms(0), 84.0), 0.0);
        // The first hop peaks at a quarter of the time at LaunchBounceHeight 0.625 × 84 = 52 px (Plank truncates).
        assert!((launch_bounce(ms(150), 84.0) - 52.0).abs() < 0.01);
        assert!(launch_bounce(ms(300), 84.0) < 1e-9, "back down half-way");
        let second = launch_bounce(ms(450), 84.0);
        assert!(second > 0.0 && second < 52.0 * 0.5, "the second hop is lower: {second}");
        assert_eq!(launch_bounce(ms(600), 84.0), 0.0);
        assert_eq!(launch_bounce(ms(5000), 84.0), 0.0);
    }

    #[test]
    fn urgent_bounce_is_one_high_hop() {
        // 1.67 × 84 = 140; Plank's damping (1 − p) × 2 caps the hop at its half-way point.
        assert!((urgent_bounce(ms(300), 84.0) - 140.0).abs() < 1e-9);
        assert!(urgent_bounce(ms(100), 84.0) > 0.0);
        assert_eq!(urgent_bounce(ms(600), 84.0), 0.0);
    }

    #[test]
    fn clicks_shade_and_come_back() {
        assert_eq!(click_darken(ms(0)), 0.0);
        assert!((click_darken(ms(150)) - 0.5).abs() < 1e-9, "darkest half-way");
        assert_eq!(click_darken(ms(300)), 0.0);
    }

    #[test]
    fn glow_pulses_for_ten_seconds() {
        let at = |t| glow_opacity(ms(t)).unwrap();
        assert!((at(0) - 0.575).abs() < 1e-6);
        assert!((at(500) - 0.95).abs() < 1e-6, "brightest a quarter pulse in");
        assert!((at(1500) - 0.2).abs() < 1e-6, "dimmest at three quarters");
        assert!((at(2000) - at(0)).abs() < 1e-6, "2 s pulses");
        assert!(glow_opacity(ms(9999)).is_some());
        assert_eq!(glow_opacity(ms(10_000)), None);
    }

    #[test]
    fn glow_colour_turns_the_hue() {
        let solid = |rgb: [u8; 3]| [rgb[0], rgb[1], rgb[2], 255].repeat(4);
        // Pure red, turned 150° → a green-cyan between green (120°) and cyan (180°).
        let [r, g, b] = glow_colour(&solid([255, 0, 0]));
        assert!((r, g, b) == (0.0, 1.0, 0.5), "{r} {g} {b}");
        // Blue (240°) → 30°: orange.
        assert_eq!(glow_colour(&solid([0, 0, 255])), [1.0, 0.5, 0.0]);
        // Transparent pixels don't count; premultiplied half-alpha red is still red.
        let mixed = [[128, 0, 0, 128], [0, 0, 0, 0]].concat();
        assert_eq!(glow_colour(&mixed), glow_colour(&solid([255, 0, 0])));
    }

    #[test]
    fn hide_eases_cubic_and_turns_back_mirrored() {
        let t0 = Instant::now();
        let mut e = Ease::new(HIDE_TIME, true);
        assert_eq!(e.value(t0), 0.0);
        assert!(!e.moving(t0));
        e.set(true, t0);
        // Hiding eases in: 1/8 at half time.
        assert!((e.value(t0 + ms(125)) - 0.125).abs() < 1e-9);
        assert!(e.moving(t0 + ms(125)));
        assert_eq!(e.value(t0 + ms(250)), 1.0);
        assert!(!e.moving(t0 + ms(300)), "settled: no more frames");
        // Showing eases out: 7/8 of the way back at half time.
        e.set(false, t0 + ms(300));
        assert!((e.value(t0 + ms(425)) - 0.125).abs() < 1e-9);
        // Turned back after 50 ms of hiding: the show runs as if 200 ms in, so 50 ms bring it home.
        let mut e = Ease::new(HIDE_TIME, true);
        e.set(true, t0 + ms(1000));
        e.set(false, t0 + ms(1050));
        assert!(e.value(t0 + ms(1050)) < 0.01);
        assert_eq!(e.value(t0 + ms(1100)), 0.0);
        e.set(false, t0 + ms(1200));
        assert_eq!(e.value(t0 + ms(1200)), 0.0, "same target: no restart");
        // The zoom eases the other way round over 200 ms.
        let mut z = Ease::new(ZOOM_TIME, false);
        z.set(true, t0);
        assert!((z.value(t0 + ms(100)) - 0.875).abs() < 1e-9);
    }

    #[test]
    fn hide_modes_follow_plank() {
        let none = Overlaps::default();
        let other = Overlaps { any: true, ..none };
        let app = Overlaps { any: true, app: true, ..none };
        let active = Overlaps { any: true, app: true, active: true, ..none };
        let maximised = Overlaps { maximised: true, ..active };
        let dialog = Overlaps { dialog: true, ..app };
        use DockHide::*;
        let table = [
            (Intelligent, [false, false, true, true, true, true]),
            (Auto, [true, true, true, true, true, true]),
            (DodgeMaximized, [false, false, false, false, true, true]),
            (WindowDodge, [false, true, true, true, true, true]),
            (DodgeActive, [false, false, false, true, true, false]),
            (Never, [false; 6]),
        ];
        for (mode, expected) in table {
            let got = [none, other, app, active, maximised, dialog].map(|o| hides(mode, false, o));
            assert_eq!(got, expected, "{mode:?}");
            assert!(!hides(mode, true, maximised), "{mode:?}: the pointer on it shows it");
        }
    }

    #[test]
    fn hider_waits_out_the_delays() {
        let t0 = Instant::now();
        let (none, d) = (Duration::ZERO, ms(400));
        let mut h = Hider::default();
        assert_eq!(h.update(true, false, none, none, t0), None);
        assert!(h.hidden, "no delay: at once");
        // Unhide delay only for the pointer.
        assert_eq!(h.update(false, true, none, d, t0), Some(t0 + d));
        assert!(h.hidden);
        assert_eq!(h.update(false, true, none, d, t0 + ms(100)), Some(t0 + d), "the same deadline");
        assert_eq!(h.update(false, true, none, d, t0 + d), None);
        assert!(!h.hidden);
        // A pending hide is dropped when the reason goes away.
        assert_eq!(h.update(true, false, d, none, t0), Some(t0 + d));
        assert_eq!(h.update(false, false, d, none, t0 + ms(100)), None);
        assert_eq!(h.update(true, false, d, none, t0 + ms(200)), Some(t0 + ms(600)), "starts over");
        // A window moving away shows it at once even with an unhide delay.
        let mut h = Hider { hidden: true, ..Default::default() };
        assert_eq!(h.update(false, false, none, d, t0), None);
        assert!(!h.hidden);
    }

    #[test]
    fn overlaps_settle_for_200_ms() {
        let t0 = Instant::now();
        let mut h = Hider::default();
        let app = Overlaps { any: true, app: true, ..Default::default() };
        assert_eq!(h.settle(app, t0), (Overlaps::default(), Some(t0 + ms(200))));
        assert_eq!(h.settle(app, t0 + ms(150)), (Overlaps::default(), Some(t0 + ms(200))));
        assert_eq!(h.settle(app, t0 + ms(200)), (app, None));
        assert_eq!(h.settle(app, t0 + ms(300)), (app, None));
    }

    #[test]
    fn moved_icons_slide_and_new_ones_rise() {
        let t0 = Instant::now();
        let mut fx = Fx::default();
        fx.track(&["a", "b", "c"], t0);
        assert!(fx.moves.is_empty() && fx.added.is_empty(), "the first frame just notes the order");
        fx.track(&["b", "a", "c", "d"], t0 + ms(10));
        assert_eq!(fx.moves["a"].0, -1.0, "a came from one slot to the left");
        assert_eq!(fx.moves["b"].0, 1.0);
        assert!(!fx.moves.contains_key("c") && fx.added.contains_key("d"));
        assert!((move_left(ms(0)) - 1.0).abs() < 1e-9 && move_left(MOVE_TIME) == 0.0);
        assert!(move_left(ms(225)) < 0.2, "ease-out: mostly there half-way");
        fx.track(&["b", "a", "c", "d"], t0 + ms(500));
        assert!(fx.moves.is_empty() && fx.added.is_empty(), "done after ItemMoveTime");
    }

    #[test]
    fn hover_lightens_and_fades() {
        let mut fx = Fx::default();
        fx.hovered(Some("a"));
        let now = Instant::now();
        assert!(fx.lighten("a", now + HOVER_TIME) - 0.2 < 1e-9);
        fx.hovered(Some("b"));
        assert!(fx.lighten("a", Instant::now() + HOVER_TIME).abs() < 1e-9, "faded out");
        assert_eq!(fx.lighten("c", now), 0.0);
        fx.tip_hidden = true;
        fx.hovered(Some("b"));
        assert!(fx.tip_hidden, "same icon: the tooltip stays away after a click");
        fx.hovered(None);
        assert!(!fx.tip_hidden);
    }

    #[test]
    fn launcher_entries_remember_when_urgency_started() {
        let mut fx = Fx::default();
        fx.entry("a".into(), Entry { count: Some(1), progress: None, urgent: true });
        let since = fx.entries["a"].1.expect("urgent");
        fx.entry("a".into(), Entry { count: Some(2), progress: None, urgent: true });
        assert_eq!(fx.entries["a"].1, Some(since), "still the first request");
        fx.entry("a".into(), Entry { count: Some(2), progress: None, urgent: false });
        assert_eq!(fx.entries["a"].1, None);
        fx.entry("a".into(), Entry::default());
        assert!(fx.entries.is_empty(), "cleared");
    }

    #[test]
    fn sliding_moves_the_whole_dock() {
        let l = super::super::dock::layout(3, 0, (1920, 1200).into(), 84, 1.0, 0.0, None);
        let s = l.clone().slid(50.0);
        assert_eq!(s.zone.loc.y, l.zone.loc.y + 50);
        assert_eq!(s.icons[1].loc.y, l.icons[1].loc.y + 50.0);
        assert_eq!(s.columns, l.columns);
    }
}
