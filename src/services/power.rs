//! Battery state from UPower's display device (E2.3): the combined battery the menu bar and Control Center show.

use futures_lite::StreamExt;
use smithay::reexports::calloop::channel::Sender;
use zbus::{Connection, fdo, proxy::CacheProperties};

use super::Update;

const SERVICE: &str = "org.freedesktop.UPower";
const PATH: &str = "/org/freedesktop/UPower/devices/DisplayDevice";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
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

/// Sends the battery now and after every change, including UPower starting or stopping.
pub async fn watch(bus: &Connection, updates: &Sender<Update>) {
    if let Err(err) = watch_inner(bus, updates).await {
        tracing::warn!("battery status off: {err}");
        let _ = updates.send(Update::Battery(None));
    }
}

async fn watch_inner(bus: &Connection, updates: &Sender<Update>) -> zbus::Result<()> {
    // Uncached: every read goes to UPower, so a restarted daemon can't leave stale values behind.
    let device = DeviceProxy::builder(bus).cache_properties(CacheProperties::No).build().await?;
    let owners = fdo::DBusProxy::new(bus).await?.receive_name_owner_changed_with_args(&[(0, SERVICE)]).await?;
    let changes = fdo::PropertiesProxy::builder(bus).destination(SERVICE)?.path(PATH)?.build().await?.receive_properties_changed().await?;
    let mut events = owners.map(drop).or(changes.map(drop));
    let mut last = None;
    loop {
        let battery = read(&device).await;
        if battery != last {
            tracing::debug!(?battery, "battery");
            if updates.send(Update::Battery(battery)).is_err() {
                return Ok(());
            }
            last = battery;
        }
        if events.next().await.is_none() {
            return Ok(());
        }
    }
}

/// `None` when there is no battery or UPower doesn't answer.
async fn read(device: &DeviceProxy<'_>) -> Option<Battery> {
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
    Some(from_raw(device.percentage().await.ok()?, charging, seconds))
}

fn from_raw(percentage: f64, charging: bool, seconds: i64) -> Battery {
    Battery {
        percent: percentage.round().clamp(0.0, 100.0) as u8,
        charging,
        minutes_left: (seconds > 0).then_some((seconds / 60) as u32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_values() {
        assert_eq!(from_raw(84.6, false, 11_400), Battery { percent: 85, charging: false, minutes_left: Some(190) });
        // UPower reports 0 s while it is still estimating.
        assert_eq!(from_raw(100.0, true, 0).minutes_left, None);
        assert_eq!(from_raw(140.0, false, -5), Battery { percent: 100, charging: false, minutes_left: None });
    }
}
