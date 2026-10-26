//! Idle, sleep & power (E6.5): after `[idle]`'s times without input the screen dims, turns off (and locks), then the
//! machine sleeps. One timer, armed for the next step only; input just stamps the time, so typing costs nothing and an
//! idle desktop wakes only at the deadlines. Apps that play video hold `idle-inhibit`, which keeps it all away;
//! `ext-idle-notify` tells other tools (swayidle) when the seat is idle. Low battery warns at 20% and 10%, the
//! computer's and each wireless mouse's, keyboard's or headset's; UPower alone sleeps at 5% (install.sh's drop-in), so
//! that action has one owner.

use std::time::{Duration, Instant};

use smithay::{
    delegate_idle_inhibit, delegate_idle_notify,
    reexports::{
        calloop::{
            LoopHandle, RegistrationToken,
            timer::{TimeoutAction, Timer},
        },
        wayland_server::{DisplayHandle, protocol::wl_surface::WlSurface},
    },
    utils::IsAlive,
    wayland::{
        idle_inhibit::{IdleInhibitHandler, IdleInhibitManagerState},
        idle_notify::{IdleNotifierHandler, IdleNotifierState},
        session_lock::SessionLockManagerState,
    },
};

use crate::{
    config,
    services::{
        notifications::Notification,
        power::{Battery, Peripheral},
    },
    state::State,
};

/// How far idleness has gone, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Active,
    /// The keyboard backlight is off (when `keyboard_backlight_off` is set; every later stage keeps it off).
    KeysOff,
    Dim,
    Off,
    /// Off and locked (`lock_delay` after `Off`).
    Locked,
    Asleep,
}

/// Unplugging takes this much brightness (percent), never going below `DIM_FLOOR`.
const DIM_STEP: u8 = 10;
const DIM_FLOOR: u8 = 20;

/// The low-battery notification's id (replaced, not stacked); below the disk notices' ids. Peripherals take the ids
/// just above it, one per name (`peripheral_id`).
const LOW_BATTERY: u32 = 0xFFFE_0000;

pub struct Idle {
    pub session_lock: SessionLockManagerState,
    pub notifier: IdleNotifierState<State>,
    _inhibit: IdleInhibitManagerState,
    /// Surfaces holding an idle inhibitor.
    inhibitors: Vec<WlSurface>,
    last_input: Instant,
    pub stage: Stage,
    timer: Option<RegistrationToken>,
    /// Brightness taken on unplugging (`battery_dim`), given back on plugging in.
    battery_dim: u8,
}

impl Idle {
    pub fn new(dh: &DisplayHandle, handle: LoopHandle<'static, State>) -> Self {
        Self {
            session_lock: SessionLockManagerState::new::<State, _>(dh, crate::protocols::unsandboxed),
            notifier: IdleNotifierState::new(dh, handle),
            _inhibit: IdleInhibitManagerState::new::<State>(dh),
            inhibitors: Vec::new(),
            last_input: Instant::now(),
            stage: Stage::Active,
            timer: None,
            battery_dim: 0,
        }
    }
}

/// The step after `stage` and how long until it is due after `idle` without input; `None`: nothing more to do.
pub fn next(config: &config::Idle, on_battery: bool, stage: Stage, idle: Duration) -> Option<(Stage, Duration)> {
    let (off, sleep) = if on_battery { (config.screen_off_on_battery, config.sleep_on_battery) } else { (config.screen_off, config.sleep_on_ac) };
    let lock = if config.lock && off > 0 { off.saturating_add(config.lock_delay) } else { 0 };
    // On a tie the earlier stage comes first (`min_by_key` keeps the first), so Off still precedes Locked.
    // ponytail: past Dim the KeysOff step is skipped, so a backlight time longer than `dim` turns it off with the dim.
    [(Stage::KeysOff, config.keyboard_backlight_off), (Stage::Dim, config.dim), (Stage::Off, off), (Stage::Locked, lock), (Stage::Asleep, sleep)]
        .into_iter()
        .filter(|&(s, secs)| s > stage && secs > 0)
        .min_by_key(|&(_, secs)| secs)
        .map(|(s, secs)| (s, Duration::from_secs(secs.into()).saturating_sub(idle)))
}

/// The low-battery level `new` just fell to (20 or 10), coming from `old`.
pub fn low_battery(old: Option<&Battery>, new: Option<&Battery>) -> Option<u8> {
    let (old, new) = (old?, new?);
    crossed(old.percent, new.percent).filter(|_| new.on_battery)
}

/// The warning level (20 or 10) a fall from `old` to `new` percent passes; the lower one on a jump past both.
fn crossed(old: u8, new: u8) -> Option<u8> {
    [10, 20].into_iter().find(|&level| new <= level && old > level)
}

/// The peripherals in `new` that just fell to 20% or 10%, with the level; matched to `old` by name, so a device just
/// connected doesn't warn. ponytail: two devices of one model share a name and so a warning; UPower's path would tell
/// them apart.
pub fn peripheral_warnings<'a>(old: &[Peripheral], new: &'a [Peripheral]) -> Vec<(&'a Peripheral, u8)> {
    new.iter()
        .filter_map(|p| {
            let before = old.iter().find(|o| o.name == p.name)?;
            Some((p, crossed(before.percent, p.percent)?))
        })
        .collect()
}

/// A peripheral's own low-battery notification id, so devices don't replace one another.
fn peripheral_id(name: &str) -> u32 {
    LOW_BATTERY + 1 + name.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b.into())) % 0xFFFE
}

/// The low-battery notification: `id`, its summary and body, urgent at 10%.
fn low_battery_notice(id: u32, summary: String, body: String, level: u8) -> Notification {
    Notification {
        id,
        app: "Battery".into(),
        icon_name: "battery-caution".into(),
        icon: crate::services::notifications::load_icon("battery-caution").map(std::sync::Arc::new),
        summary,
        body,
        urgency: if level <= 10 { 2 } else { 1 },
        expire: -1,
        time: chrono::Local::now().timestamp(),
        ..Default::default()
    }
}

/// Brightness after a change of power source, and how much is now taken: unplugged, `DIM_STEP` lower but not below
/// `DIM_FLOOR`; plugged in, `taken` given back.
pub fn battery_dim(brightness: u8, on_battery: bool, taken: u8) -> (u8, u8) {
    if on_battery {
        let take = brightness.saturating_sub(DIM_FLOOR).min(DIM_STEP);
        (brightness - take, take)
    } else {
        (brightness.saturating_add(taken).min(100), 0)
    }
}

impl State {
    /// Input happened: undim, and tell idle-notify clients.
    pub fn idle_activity(&mut self) {
        self.idle.last_input = Instant::now();
        let seat = self.seat.clone();
        self.idle.notifier.notify_activity(&seat);
        if self.idle.stage != Stage::Active {
            self.idle.stage = Stage::Active;
            // Back to the level before; a no-op when it wasn't turned off.
            self.services.send(crate::services::Command::KbdBacklightIdle(false));
            self.schedule_redraw();
            self.idle_arm();
        } else if self.idle.timer.is_none() {
            self.idle_arm();
        }
    }

    /// Displays off now, as if idle had got there; the next input wakes them, and locking follows its usual delay.
    pub fn display_off(&mut self) {
        self.idle.stage = self.idle.stage.max(Stage::Off);
        self.schedule_redraw();
        self.idle_arm();
    }

    /// Arms the timer for the next step, if there is one.
    pub fn idle_arm(&mut self) {
        if let Some(token) = self.idle.timer.take() {
            self.loop_handle.remove(token);
        }
        let on_battery = self.battery.is_some_and(|b| b.on_battery);
        let Some((_, wait)) = next(&self.config.idle, on_battery, self.idle.stage, self.idle.last_input.elapsed()) else { return };
        let timer = self.loop_handle.insert_source(Timer::from_duration(wait), |_, _, state| {
            state.idle.timer = None;
            state.idle_due();
            TimeoutAction::Drop
        });
        self.idle.timer = timer.inspect_err(|err| tracing::warn!("idle timer: {err}")).ok();
    }

    /// The timer fired: take every step that is due (input meanwhile only moved the deadline), then re-arm.
    fn idle_due(&mut self) {
        self.idle.inhibitors.retain(IsAlive::alive);
        if self.idle_inhibited() {
            // A video is playing, or Keep Awake is on: that counts as being used.
            self.idle.last_input = Instant::now();
        }
        let on_battery = self.battery.is_some_and(|b| b.on_battery);
        let before = self.idle.stage;
        while let Some((stage, Duration::ZERO)) = next(&self.config.idle, on_battery, self.idle.stage, self.idle.last_input.elapsed()) {
            tracing::debug!(?stage, "idle");
            self.idle.stage = stage;
            self.schedule_redraw();
            match stage {
                Stage::Locked => self.lock_screen(),
                Stage::Asleep => self.sleep(),
                _ => {}
            }
        }
        if before < Stage::KeysOff && self.idle.stage >= Stage::KeysOff && self.config.idle.keyboard_backlight_off > 0 {
            self.services.send(crate::services::Command::KbdBacklightIdle(true));
        }
        self.idle_arm();
    }

    /// An app's idle inhibitor or Keep Awake holds idleness off.
    fn idle_inhibited(&self) -> bool {
        self.keep_awake || !self.idle.inhibitors.is_empty()
    }

    /// Keep Awake on or off; either way the countdown starts again now.
    pub fn set_keep_awake(&mut self, on: bool) {
        self.keep_awake = on;
        let inhibited = self.idle_inhibited();
        self.idle.notifier.set_is_inhibited(inhibited);
        self.idle_activity();
    }

    /// The apps whose windows hold an idle inhibitor, by name, once each.
    // ponytail: reads their `.desktop` names on every call; cache per surface if the open list shows in a profile.
    pub fn idle_blockers(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for surface in self.idle.inhibitors.iter().filter(|s| s.alive()) {
            // A video often plays in a subsurface: its window is the root's.
            let mut root = surface.clone();
            while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
                root = parent;
            }
            let name = self.window_for(&root).and_then(|w| self.app_id(w)).map_or_else(|| "Unknown App".to_owned(), |id| crate::render::menubar::app_name(&id));
            if !names.contains(&name) {
                names.push(name);
            }
        }
        names
    }

    /// How dark the idle shade over everything is: dimmed, or black while the screen is "off". On a real screen the
    /// udev backend turns the panel off (DPMS) at `Off` instead of drawing the black; nested, the black stands in.
    /// ponytail: `Dim` is drawn, not the backlight; wire it to brightness when the panel's dim looks wrong.
    pub fn idle_shade(&self) -> Option<f32> {
        match self.idle.stage {
            Stage::Active | Stage::KeysOff => None,
            Stage::Dim => Some(0.5),
            Stage::Off | Stage::Locked | Stage::Asleep => Some(1.0),
        }
    }

    /// UPower's peripherals; each warns once on the way down through 20% and 10%, in its own notification.
    pub fn peripherals_changed(&mut self, list: Vec<Peripheral>) {
        for (p, level) in peripheral_warnings(&self.peripherals, &list) {
            let body = format!("{} has {level}% battery remaining. Charge it soon.", p.name);
            let notice = low_battery_notice(peripheral_id(&p.name), format!("{} Battery Low", p.name), body, level);
            self.notify(notice);
        }
        self.peripherals = list;
    }

    /// UPower's new battery state; warns once on the way down through 20% and 10%, re-times idleness on a change of power
    /// source.
    pub fn battery_changed(&mut self, battery: Option<Battery>) {
        if let Some(level) = low_battery(self.battery.as_ref(), battery.as_ref()) {
            let body = format!("{level}% battery remaining. Plug in to keep working.");
            self.notify(low_battery_notice(LOW_BATTERY, "Low Battery".into(), body, level));
        }
        let replugged = self.battery.map(|b| b.on_battery) != battery.map(|b| b.on_battery);
        let old_known = self.battery.is_some();
        self.battery = battery;
        if replugged {
            // The display-off and sleep times differ by power source.
            self.idle_arm();
        }
        // A plug change, not start-up: dim on battery, or give back what was taken.
        if let (true, true, Some(b), Some(now)) = (replugged, old_known, battery, self.brightness)
            && (!b.on_battery || self.config.battery.dim_on_battery)
        {
            let (level, taken) = battery_dim(now, b.on_battery, self.idle.battery_dim);
            self.idle.battery_dim = taken;
            if level != now {
                self.brightness = Some(level);
                self.services.send(crate::services::Command::Brightness(level));
            }
        }
    }
}

impl IdleNotifierHandler for State {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
        &mut self.idle.notifier
    }
}

/// ponytail: an inhibitor counts while its surface lives, even hidden on another Space; the protocol says only while
/// visible.
impl IdleInhibitHandler for State {
    fn inhibit(&mut self, surface: WlSurface) {
        self.idle.inhibitors.push(surface);
        self.idle.notifier.set_is_inhibited(true);
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.idle.inhibitors.retain(|s| *s != surface && s.alive());
        let inhibited = self.idle_inhibited();
        self.idle.notifier.set_is_inhibited(inhibited);
        if !inhibited {
            // The countdown starts when the video stops.
            self.idle_activity();
        }
    }
}

delegate_idle_notify!(State);
delegate_idle_inhibit!(State);

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn steps_in_order_and_on_time() {
        let c = config::Idle { screen_off_on_battery: 300, ..Default::default() };
        assert_eq!(next(&c, true, Stage::Active, secs(0)), Some((Stage::KeysOff, secs(30))));
        assert_eq!(next(&c, true, Stage::KeysOff, secs(30)), Some((Stage::Dim, secs(90))));
        assert_eq!(next(&c, true, Stage::KeysOff, secs(150)), Some((Stage::Dim, Duration::ZERO)));
        assert_eq!(next(&c, true, Stage::Dim, secs(150)), Some((Stage::Off, secs(150))));
        assert_eq!(next(&c, true, Stage::Dim, secs(300)), Some((Stage::Off, Duration::ZERO)));
        assert_eq!(next(&c, true, Stage::Off, secs(300)), Some((Stage::Locked, Duration::ZERO)), "locks with the screen by default");
        assert_eq!(next(&c, true, Stage::Locked, secs(300)), Some((Stage::Asleep, secs(600))));
        assert_eq!(next(&c, false, Stage::Locked, secs(300)), None, "never sleeps on AC by default");
        assert_eq!(next(&c, true, Stage::Asleep, secs(5000)), None);
    }

    #[test]
    fn zero_is_never_and_order_follows_the_times() {
        let c = config::Idle { dim: 0, screen_off: 60, keyboard_backlight_off: 0, sleep_on_battery: 30, ..Default::default() };
        assert_eq!(next(&c, true, Stage::Active, secs(0)), Some((Stage::Asleep, secs(30))), "sleep first when it comes first");
        let off = config::Idle {
            dim: 0,
            screen_off: 0,
            screen_off_on_battery: 0,
            keyboard_backlight_off: 0,
            sleep_on_battery: 0,
            sleep_on_ac: 0,
            lock: true,
            ..Default::default()
        };
        assert_eq!(next(&off, true, Stage::Active, secs(0)), None);
    }

    #[test]
    fn display_off_follows_the_power_source() {
        let c = config::Idle { screen_off: 600, screen_off_on_battery: 180, ..Default::default() };
        assert_eq!(next(&c, true, Stage::Dim, secs(120)), Some((Stage::Off, secs(60))));
        assert_eq!(next(&c, false, Stage::Dim, secs(120)), Some((Stage::Off, secs(480))));
        let never = config::Idle { screen_off_on_battery: 0, lock: true, sleep_on_battery: 0, ..c };
        assert_eq!(next(&never, true, Stage::Dim, secs(120)), None, "0: never off, nor locked with it");
    }

    #[test]
    fn keyboard_backlight_goes_off_first_or_with_the_dim() {
        let c = config::Idle { keyboard_backlight_off: 300, ..Default::default() };
        assert_eq!(next(&c, false, Stage::Active, secs(0)), Some((Stage::Dim, secs(120))), "longer than dim: skipped");
        let never = config::Idle { keyboard_backlight_off: 0, ..Default::default() };
        assert_eq!(next(&never, false, Stage::Active, secs(0)), Some((Stage::Dim, secs(120))));
    }

    #[test]
    fn lock_waits_its_delay_after_the_screen_goes_off() {
        let c = config::Idle { lock_delay: 60, ..Default::default() };
        assert_eq!(next(&c, false, Stage::Off, secs(300)), Some((Stage::Locked, secs(60))));
        let unlocked = config::Idle { lock: false, ..Default::default() };
        assert_eq!(next(&unlocked, false, Stage::Off, secs(300)), None);
    }

    #[test]
    fn battery_dim_steps_down_to_the_floor_and_gives_back() {
        assert_eq!(battery_dim(80, true, 0), (70, 10));
        assert_eq!(battery_dim(70, false, 10), (80, 0));
        assert_eq!(battery_dim(25, true, 0), (20, 5), "stops at the floor");
        assert_eq!(battery_dim(15, true, 0), (15, 0), "already below it: untouched");
        assert_eq!(battery_dim(95, false, 10), (100, 0), "turned up meanwhile: capped");
        assert_eq!(battery_dim(60, false, 0), (60, 0), "nothing taken, nothing given");
    }

    #[test]
    fn peripherals_warn_once_per_crossing_each() {
        let p = |name: &str, percent| Peripheral { name: name.into(), percent };
        let old = [p("Mouse", 21), p("Headset", 15)];
        let new = [p("Mouse", 20), p("Headset", 10), p("Keyboard", 5)];
        let warned: Vec<_> = peripheral_warnings(&old, &new).into_iter().map(|(p, level)| (p.name.as_str(), level)).collect();
        assert_eq!(warned, [("Mouse", 20), ("Headset", 10)], "the keyboard just connected: no warning");
        assert!(peripheral_warnings(&new, &[p("Mouse", 19), p("Headset", 9)]).is_empty(), "already warned");
        assert!(peripheral_warnings(&[p("Mouse", 19)], &[p("Mouse", 25)]).is_empty(), "charging");
        assert_ne!(peripheral_id("Mouse"), peripheral_id("Headset"));
        assert!(peripheral_id("Mouse") > LOW_BATTERY && peripheral_id("Mouse") < 0xFFFF_0000);
    }

    #[test]
    fn low_battery_warns_once_per_level() {
        let b = |percent, on_battery| Some(Battery { percent, charging: false, on_battery, minutes_left: None });
        assert_eq!(low_battery(b(21, true).as_ref(), b(20, true).as_ref()), Some(20));
        assert_eq!(low_battery(b(20, true).as_ref(), b(19, true).as_ref()), None, "already warned at 20");
        assert_eq!(low_battery(b(11, true).as_ref(), b(9, true).as_ref()), Some(10));
        assert_eq!(low_battery(b(30, true).as_ref(), b(8, true).as_ref()), Some(10), "a jump past both warns for the lower");
        assert_eq!(low_battery(b(21, false).as_ref(), b(20, false).as_ref()), None, "not while plugged in");
        assert_eq!(low_battery(None, b(5, true).as_ref()), None, "no warning at start-up");
    }
}
