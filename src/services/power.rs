//! Battery state from UPower's display device (E2.3): the combined battery the menu bar and Control Center show.
//! Also the keyboard backlight, which UPower sets without root.

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
    #[zbus(property)]
    fn percentage(&self) -> zbus::Result<f64>;
    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn time_to_empty(&self) -> zbus::Result<i64>;
    #[zbus(property)]
    fn time_to_full(&self) -> zbus::Result<i64>;
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
    fn raw_values() {
        assert_eq!(from_raw(84.6, false, 11_400), Battery { percent: 85, charging: false, on_battery: false, minutes_left: Some(190) });
        // UPower reports 0 s while it is still estimating.
        assert_eq!(from_raw(100.0, true, 0).minutes_left, None);
        assert_eq!(from_raw(140.0, false, -5), Battery { percent: 100, charging: false, on_battery: false, minutes_left: None });
    }
}
