//! Battery state from UPower's display device (E2.3): the combined battery the menu bar and Control Center show.
//! Also peripherals' batteries (wireless mice, keyboards, headsets) and the keyboard backlight, which UPower sets
//! without root.

use smithay::reexports::calloop::channel::Sender;
use zbus::{Connection, proxy::CacheProperties};

use super::Update;

const SERVICE: &str = "org.freedesktop.UPower";
const PATH: &str = "/org/freedesktop/UPower/devices/DisplayDevice";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
    /// Running on the battery (discharging), not on AC.
    pub on_battery: bool,
    /// Until empty while discharging, until full while charging; `None` while UPower is still estimating.
    pub minutes_left: Option<u32>,
}

#[zbus::proxy(
    interface = "org.freedesktop.UPower.Device",
    default_service = "org.freedesktop.UPower",
    default_path = "/org/freedesktop/UPower/devices/DisplayDevice"
)]
trait Device {
    #[zbus(property)]
    fn is_present(&self) -> zbus::Result<bool>;
    #[zbus(property, name = "Type")]
    fn type_(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn power_supply(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn model(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn percentage(&self) -> zbus::Result<f64>;
    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn time_to_empty(&self) -> zbus::Result<i64>;
    #[zbus(property)]
    fn time_to_full(&self) -> zbus::Result<i64>;
}

/// A peripheral's battery.
#[derive(Clone, Debug, PartialEq)]
pub struct Peripheral {
    /// Its model ("MX Master 3"), else its kind ("Mouse").
    pub name: String,
    pub percent: u8,
}

#[zbus::proxy(interface = "org.freedesktop.UPower", default_service = "org.freedesktop.UPower", default_path = "/org/freedesktop/UPower")]
trait UPower {
    fn enumerate_devices(&self) -> zbus::Result<Vec<zbus::zvariant::OwnedObjectPath>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.UPower.KbdBacklight",
    default_service = "org.freedesktop.UPower",
    default_path = "/org/freedesktop/UPower/KbdBacklight"
)]
trait KbdBacklight {
    fn get_brightness(&self) -> zbus::Result<i32>;
    fn get_max_brightness(&self) -> zbus::Result<i32>;
    fn set_brightness(&self, value: i32) -> zbus::Result<()>;
}

/// The keyboard backlight one step up or down, or (`None`) off and back on; sends the new level for the OSD.
pub async fn kbd_backlight(bus: &Connection, change: Option<bool>, updates: &Sender<Update>) {
    let result = async {
        let kbd = KbdBacklightProxy::new(bus).await?;
        let max = kbd.get_max_brightness().await?;
        let level = kbd_level(kbd.get_brightness().await?, max, change);
        kbd.set_brightness(level).await?;
        zbus::Result::Ok(if max > 0 { (level * 100 / max) as u8 } else { 0 })
    };
    match result.await {
        Ok(percent) => {
            let _ = updates.send(Update::KbdBacklight(percent));
        }
        // No keyboard backlight, or no UPower.
        Err(err) => tracing::debug!("keyboard backlight: {err}"),
    }
}

/// Idleness turns the keyboard backlight off (`off`), remembering its level in `saved`, or back to that level.
pub async fn kbd_idle(bus: &Connection, off: bool, saved: &mut Option<i32>) {
    let result = async {
        let kbd = KbdBacklightProxy::new(bus).await?;
        if off {
            let now = kbd.get_brightness().await?;
            if now > 0 {
                kbd.set_brightness(0).await?;
                *saved = Some(now);
            }
        } else if let Some(level) = saved.take() {
            kbd.set_brightness(level).await?;
        }
        zbus::Result::Ok(())
    };
    if let Err(err) = result.await {
        tracing::debug!("keyboard backlight: {err}");
    }
}

/// About 1/16 of the range per step (most keyboards have only 2 or 3 levels: one each).
// ponytail: toggling back on goes to full, not the level before; remember it if that annoys.
fn kbd_level(now: i32, max: i32, change: Option<bool>) -> i32 {
    let step = (max / 16).max(1);
    match change {
        Some(true) => (now + step).min(max),
        Some(false) => (now - step).max(0),
        None if now > 0 => 0,
        None => max,
    }
}

/// Sends the battery now and after every change, including UPower starting or stopping.
pub async fn watch(bus: &Connection, updates: &Sender<Update>) {
    super::follow(bus, SERVICE, PATH, updates, Update::Battery, read).await;
}

/// Sends the peripherals' batteries now and after every change, devices coming and going included.
pub async fn watch_peripherals(bus: &Connection, updates: &Sender<Update>) {
    super::follow(bus, SERVICE, "/org/freedesktop/UPower", updates, Update::Peripherals, read_peripherals).await;
}

async fn read_peripherals(bus: &Connection) -> Option<Vec<Peripheral>> {
    let paths = UPowerProxy::builder(bus).cache_properties(CacheProperties::No).build().await.ok()?.enumerate_devices().await.ok()?;
    let mut list = Vec::new();
    for path in paths {
        let Ok(builder) = DeviceProxy::builder(bus).path(path) else { continue };
        let Ok(device) = builder.cache_properties(CacheProperties::No).build().await else { continue };
        let (Ok(kind), Ok(supply), Ok(present), Ok(percent)) =
            (device.type_().await, device.power_supply().await, device.is_present().await, device.percentage().await)
        else {
            continue;
        };
        list.extend(peripheral(kind, supply, present, &device.model().await.unwrap_or_default(), percent));
    }
    Some(list)
}

/// A UPower device as a peripheral: present, not powering the computer, and a mouse, keyboard, game controller, pen,
/// touchpad, headset, speaker or headphones (UPower's types 5, 6, 12, 13, 14, 17, 18, 19).
fn peripheral(kind: u32, power_supply: bool, present: bool, model: &str, percent: f64) -> Option<Peripheral> {
    let fallback = match kind {
        5 => "Mouse",
        6 => "Keyboard",
        12 => "Game Controller",
        13 => "Pen",
        14 => "Trackpad",
        17 => "Headset",
        18 => "Speaker",
        19 => "Headphones",
        _ => return None,
    };
    let name = if model.trim().is_empty() { fallback } else { model.trim() };
    (present && !power_supply).then(|| Peripheral { name: name.to_owned(), percent: percent.round().clamp(0.0, 100.0) as u8 })
}

/// `None` when there is no battery or UPower doesn't answer.
async fn read(bus: &Connection) -> Option<Battery> {
    // Uncached: every read goes to UPower, so a restarted daemon can't leave stale values behind.
    let device = DeviceProxy::builder(bus).cache_properties(CacheProperties::No).build().await.ok()?;
    if !device.is_present().await.ok()? {
        return None;
    }
    let state = device.state().await.ok()?;
    // UPower states: 1 charging, 2 discharging, 4 fully charged, 5 pending charge, 6 pending discharge.
    let charging = state == 1;
    let seconds = match state {
        1 => device.time_to_full().await.ok()?,
        2 => device.time_to_empty().await.ok()?,
        _ => 0,
    };
    Some(Battery { on_battery: matches!(state, 2 | 6), ..from_raw(device.percentage().await.ok()?, charging, seconds) })
}

fn from_raw(percentage: f64, charging: bool, seconds: i64) -> Battery {
    Battery { percent: percentage.round().clamp(0.0, 100.0) as u8, charging, on_battery: false, minutes_left: (seconds > 0).then_some((seconds / 60) as u32) }
}

/// The battery menu's first row: time to empty or to full, or why there is none.
pub fn time_left(b: &Battery) -> String {
    match (b.charging, b.on_battery, b.minutes_left) {
        (true, _, Some(m)) => format!("{} until full", minutes(m)),
        (true, _, None) => "Charging".into(),
        (_, true, Some(m)) => format!("{} remaining", minutes(m)),
        (_, true, None) => "Calculating time remaining…".into(),
        _ if b.percent >= 99 => "Fully charged".into(),
        _ => "Not charging".into(),
    }
}

/// `190` → `3 h 10 min`.
fn minutes(m: u32) -> String {
    match (m / 60, m % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kbd_levels() {
        assert_eq!(kbd_level(1, 2, Some(true)), 2);
        assert_eq!(kbd_level(2, 2, Some(true)), 2);
        assert_eq!(kbd_level(0, 2, Some(false)), 0);
        assert_eq!(kbd_level(160, 255, Some(false)), 145);
        assert_eq!(kbd_level(1, 3, None), 0);
        assert_eq!(kbd_level(0, 3, None), 3);
    }

    #[test]
    fn time_left_words() {
        let b = |charging, on_battery, minutes_left| Battery { percent: 50, charging, on_battery, minutes_left };
        assert_eq!(time_left(&b(false, true, Some(190))), "3 h 10 min remaining");
        assert_eq!(time_left(&b(false, true, Some(45))), "45 min remaining");
        assert_eq!(time_left(&b(true, false, Some(120))), "2 h until full");
        assert_eq!(time_left(&b(false, true, None)), "Calculating time remaining…");
        assert_eq!(time_left(&b(true, false, None)), "Charging");
        assert_eq!(time_left(&b(false, false, None)), "Not charging");
        assert_eq!(time_left(&Battery { percent: 100, ..b(false, false, None) }), "Fully charged");
    }

    #[test]
    fn peripherals_only() {
        assert_eq!(peripheral(5, false, true, "MX Master 3", 61.6), Some(Peripheral { name: "MX Master 3".into(), percent: 62 }));
        assert_eq!(peripheral(17, false, true, " ", 40.0).map(|p| p.name), Some("Headset".into()), "no model: its kind");
        assert_eq!(peripheral(2, true, true, "BAT0", 80.0), None, "the laptop's own battery");
        assert_eq!(peripheral(1, true, true, "", 0.0), None, "the power adapter");
        assert_eq!(peripheral(6, false, false, "ELAN Keyboard", 0.0), None, "not present (a touchscreen's pen slot)");
        assert_eq!(peripheral(6, true, true, "Dock", 50.0), None, "powers the computer");
    }

    #[test]
    fn raw_values() {
        assert_eq!(from_raw(84.6, false, 11_400), Battery { percent: 85, charging: false, on_battery: false, minutes_left: Some(190) });
        // UPower reports 0 s while it is still estimating.
        assert_eq!(from_raw(100.0, true, 0).minutes_left, None);
        assert_eq!(from_raw(140.0, false, -5), Battery { percent: 100, charging: false, on_battery: false, minutes_left: None });
    }
}
