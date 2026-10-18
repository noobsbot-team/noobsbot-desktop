//! The dock's timed behaviour (E3.1), with the Transparent-Tight theme's own values so it feels like Plank: the launch
//! and urgent bounces, the pulsing urgent glow at the edge of a hidden dock, the hide slide and the zoom easing; plus
//! LauncherEntry badges and the tiles of minimised windows. Frames are drawn only while something here moves.

use std::{
    cell::Cell,
    collections::HashMap,
    f64::consts::PI,
    time::{Duration, Instant},
};

use smithay::{
    desktop::Window,
    utils::{IsAlive, Logical, Point, Rectangle},
};

use super::dock::{Item, Layout};
use crate::{services::launcher::Entry, state::State, wm::Rect};

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
/// HideTime, FadeOpacity 1: the dock slides, no fade. Plank's zoom eases over the same time.
const HIDE_TIME: Duration = Duration::from_millis(250);

fn progress(t: Duration, of: Duration) -> f64 {
    t.as_secs_f64() / of.as_secs_f64()
}

/// Plank's launch bounce, px above the baseline `t` after the launch: two hops, the second lower.
pub fn launch_bounce(t: Duration, icon: f64) -> f64 {
    let p = progress(t, LAUNCH_TIME);
    if p >= 1.0 {
        return 0.0;
    }
    (2.0 * PI * p).sin().abs() * LAUNCH_HEIGHT * icon * (1.3333 * (1.0 - p)).min(1.0)
}

/// Plank's urgent bounce: one high hop.
pub fn urgent_bounce(t: Duration, icon: f64) -> f64 {
    let p = progress(t, URGENT_TIME);
    if p >= 1.0 { 0.0 } else { (PI * p).sin() * URGENT_HEIGHT * icon }
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

/// 0 → 1 at a constant speed over HideTime, turning back mid-way when the target flips.
#[derive(Debug, Clone, Copy, Default)]
pub struct Ease {
    on: bool,
    from: f64,
    since: Option<Instant>,
}

impl Ease {
    pub fn value(&self, now: Instant) -> f64 {
        let Some(since) = self.since else { return if self.on { 1.0 } else { 0.0 } };
        let step = progress(now.saturating_duration_since(since), HIDE_TIME);
        if self.on { (self.from + step).min(1.0) } else { (self.from - step).max(0.0) }
    }

    pub fn set(&mut self, on: bool, now: Instant) {
        if on != self.on {
            self.from = self.value(now);
            (self.on, self.since) = (on, Some(now));
        }
    }

    fn moving(&self, now: Instant) -> bool {
        let v = self.value(now);
        self.since.is_some() && v > 0.0 && v < 1.0
    }
}

/// Set on a window minimised (not hidden): it gets its own tile at the dock's right end.
struct Minimised(Cell<bool>);

fn minimised(window: &Window) -> &Cell<bool> {
    &window.user_data().get_or_insert(|| Minimised(Cell::new(false))).0
}

/// Hiding (not minimising) a window drops its tile; `hide_windows` calls this.
pub fn not_minimised(window: &Window) {
    minimised(window).set(false);
}

#[derive(Debug, Default)]
pub struct Fx {
    /// 1 = hidden.
    pub hide: Ease,
    /// 1 = zoomed in.
    pub zoom: Ease,
    /// The last pointer position over the dock, which the zoom eases out around after the pointer left.
    pub zoom_at: Option<Point<f64, Logical>>,
    /// Item key → when it was launched from the dock.
    launched: HashMap<String, Instant>,
    /// Windows that asked for attention, and since when.
    urgent: Vec<(Window, Instant)>,
    /// LauncherEntry state by app id, and since when each app has been urgent.
    entries: HashMap<String, (Entry, Option<Instant>)>,
}

/// What the dock draws this frame.
#[derive(Debug, Default)]
pub struct Frame {
    /// Slid and bounced; `None` while fully hidden.
    pub layout: Option<Layout>,
    /// Glow circles at the screen edge, front to back, with their colour and opacity.
    pub glows: Vec<(Rect, [f32; 4])>,
    /// Item index and its badge and progress.
    pub badges: Vec<(usize, Entry)>,
    /// Draw another frame.
    pub animating: bool,
}

impl Fx {
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

    pub fn urgent(&mut self, window: Window) {
        if !self.urgent.iter().any(|(w, _)| *w == window) {
            self.urgent.push((window, Instant::now()));
        }
    }
}

impl Layout {
    /// The whole dock moved down by `dy`.
    fn slid(mut self, dy: f64) -> Self {
        let d = dy.round() as i32;
        self.icons.iter_mut().for_each(|r| r.loc.y += dy);
        self.bar.loc.y += d;
        self.zone.loc.y += d;
        self
    }
}

impl State {
    /// The app id a LauncherEntry names `item` by: its key, or anything `Apps::find` resolves to it.
    fn entry_for(&self, item: &Item) -> Option<&(Entry, Option<Instant>)> {
        let fx = &self.dock_fx;
        fx.entries.get(&item.key).or_else(|| fx.entries.iter().find(|(k, _)| self.apps.find(k).is_some_and(|a| a.id == item.key)).map(|(_, e)| e))
    }

    /// Steps the dock's animations to now: slides it towards shown or hidden, bounces launching and urgent icons,
    /// pulses the urgent glow while hidden. Focusing an app ends its urgency.
    pub fn dock_frame(&mut self, items: &[Item]) -> Frame {
        let now = Instant::now();
        let Some(layout) = self.dock_layout(items.len()).filter(|_| !items.is_empty()) else { return Frame::default() };
        let visible = self.dock_visible(&layout);
        let focused = self.focused_window();
        let fx = &mut self.dock_fx;
        fx.hide.set(!visible, now);
        fx.urgent.retain(|(w, _)| w.alive() && focused.as_ref() != Some(w));
        fx.launched.retain(|_, at| now - *at < LAUNCH_TIME);
        let hide = fx.hide.value(now);
        let mut animating = fx.hide.moving(now) || fx.zoom.moving(now) || !fx.launched.is_empty();

        let urgent_since: Vec<Option<Instant>> = items
            .iter()
            .map(|item| {
                let windows = self.dock_fx.urgent.iter().filter(|(w, _)| item.windows.contains(w)).map(|(_, at)| *at);
                windows.chain(self.entry_for(item).and_then(|(_, since)| *since)).min()
            })
            .collect();
        let mut frame = Frame::default();
        let bottom = f64::from(layout.bar.loc.y + layout.bar.size.h);
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
                    let rect = Rectangle::new(((x - radius).round() as i32, (bottom - radius) as i32).into(), ((2.0 * radius) as i32, (2.0 * radius) as i32).into());
                    frame.glows.insert(0, (rect, [r, g, b, opacity * 0.3]));
                }
            }
        }
        if hide < 1.0 {
            let reach = bottom - layout.icons.iter().map(|r| r.loc.y).fold(f64::from(layout.bar.loc.y), f64::min);
            let mut layout = layout.slid(hide * reach);
            for (i, item) in items.iter().enumerate() {
                let launch = self.dock_fx.launched.get(&item.key).map_or(0.0, |at| launch_bounce(now - *at, layout.icon));
                let urgent = urgent_since[i].map_or(0.0, |at| urgent_bounce(now - at, layout.icon));
                layout.icons[i].loc.y -= launch.max(urgent);
            }
            frame.layout = Some(layout);
        }
        frame.badges = items.iter().enumerate().filter_map(|(i, item)| Some((i, self.entry_for(item)?.0))).collect();
        frame.animating = animating;
        frame
    }

    /// Minimises `windows`: hidden, each with its own tile at the dock's right end until it is shown again.
    pub fn minimise(&mut self, windows: Vec<Window>) {
        let marked = windows.clone();
        self.hide_windows(windows);
        marked.iter().for_each(|w| minimised(w).set(true));
    }

    /// One tile per minimised window, in the order they went, after the apps (E5.5 extra on top of Plank).
    // ponytail: the tile shows the app's icon; the window's live thumbnail comes with the minimise animation (E5.5).
    pub fn minimised_items(&self) -> Vec<Item> {
        self.hidden
            .iter()
            .filter(|(w, _)| minimised(w).get() && !self.is_quake(w))
            .filter_map(|(w, _)| {
                let raw = self.app_key(w)?;
                let app = self.apps.find(&raw).cloned();
                let key = format!("{}#minimised", app.as_ref().map_or(raw, |a| a.id.clone()));
                Some(Item { key, app, windows: vec![w.clone()] })
            })
            .collect()
    }

    /// `--show dock-badges`: sample counts and progress bars on the default pins.
    pub fn sample_badges(&mut self) {
        let samples = [("org.kde.dolphin", Some(3), None), ("org.kde.kate", None, Some(0.6)), ("noobsbot", Some(12), Some(0.35)), ("helium-browser", Some(128), None)];
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
        // The first hop peaks at a quarter of the time at 0.625 × icon (Plank's 1.3333 damping just starts there).
        assert!((launch_bounce(ms(150), 84.0) - 52.5).abs() < 0.01);
        assert!(launch_bounce(ms(300), 84.0) < 1e-9, "back down half-way");
        let second = launch_bounce(ms(450), 84.0);
        assert!(second > 0.0 && second < 52.5 * 0.5, "the second hop is lower: {second}");
        assert_eq!(launch_bounce(ms(600), 84.0), 0.0);
        assert_eq!(launch_bounce(ms(5000), 84.0), 0.0);
    }

    #[test]
    fn urgent_bounce_is_one_high_hop() {
        assert!((urgent_bounce(ms(300), 84.0) - 140.0).abs() < 1e-9, "1.67 × icon");
        assert!(urgent_bounce(ms(100), 84.0) > 0.0);
        assert_eq!(urgent_bounce(ms(600), 84.0), 0.0);
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
    fn ease_runs_at_constant_speed_and_turns_back() {
        let t0 = Instant::now();
        let mut e = Ease::default();
        assert_eq!(e.value(t0), 0.0);
        assert!(!e.moving(t0));
        e.set(true, t0);
        assert!((e.value(t0 + ms(125)) - 0.5).abs() < 1e-9);
        assert!(e.moving(t0 + ms(125)));
        assert_eq!(e.value(t0 + ms(250)), 1.0);
        assert!(!e.moving(t0 + ms(300)), "settled: no more frames");
        // Turning back half-way in takes half the time.
        let mut e = Ease::default();
        e.set(true, t0);
        e.set(false, t0 + ms(100));
        assert!((e.value(t0 + ms(150)) - 0.2).abs() < 1e-9);
        assert_eq!(e.value(t0 + ms(200)), 0.0);
        e.set(false, t0 + ms(300));
        assert_eq!(e.value(t0 + ms(300)), 0.0, "same target: no restart");
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
        let l = super::super::dock::layout(3, (1920, 1200).into(), 84, 1.0, None);
        let s = l.clone().slid(50.0);
        assert_eq!(s.bar.loc.y, l.bar.loc.y + 50);
        assert_eq!(s.zone.loc.y, l.zone.loc.y + 50);
        assert_eq!(s.icons[1].loc.y, l.icons[1].loc.y + 50.0);
        assert_eq!(s.columns, l.columns);
    }
}
