//! Display brightness (E2.3, E2.7): read from `/sys/class/backlight`, set through logind so no root is needed.

use std::{fs, path::Path};

use smithay::reexports::calloop::channel::Sender;
use zbus::Connection;

use super::Update;

const SYSFS: &str = "/sys/class/backlight";

#[zbus::proxy(interface = "org.freedesktop.login1.Session", default_service = "org.freedesktop.login1", default_path = "/org/freedesktop/login1/session/auto")]
trait Session {
    fn set_brightness(&self, subsystem: &str, name: &str, brightness: u32) -> zbus::Result<()>;
}

/// The panel's backlight: firmware interfaces first, then platform, then raw (the order logind uses).
fn device() -> Option<String> {
    let rank = |name: &str| match fs::read_to_string(Path::new(SYSFS).join(name).join("type")).ok()?.trim() {
        "firmware" => Some(0),
        "platform" => Some(1),
        "raw" => Some(2),
        _ => None,
    };
    let names = fs::read_dir(SYSFS).ok()?.filter_map(|e| e.ok()?.file_name().into_string().ok());
    names.filter_map(|name| Some((rank(&name)?, name))).min().map(|(_, name)| name)
}

fn read_number(name: &str, file: &str) -> Option<u32> {
    fs::read_to_string(Path::new(SYSFS).join(name).join(file)).ok()?.trim().parse().ok()
}

/// Sends the current brightness in percent; `None` without a backlight (desktop monitors).
// ponytail: read at start and after our own changes; firmware-handled brightness keys aren't seen until then.
// Listen to the backlight's udev change events if a machine needs it.
pub fn send_current(updates: &Sender<Update>) {
    let percent = device().and_then(|name| Some(to_percent(read_number(&name, "brightness")?, read_number(&name, "max_brightness")?)));
    tracing::debug!(?percent, "brightness");
    let _ = updates.send(Update::Brightness(percent));
}

pub async fn set(bus: &Connection, percent: u8, updates: &Sender<Update>) {
    let Some(name) = device() else { return };
    let Some(max) = read_number(&name, "max_brightness") else { return };
    let result = match SessionProxy::new(bus).await {
        Ok(session) => session.set_brightness("backlight", &name, to_raw(percent, max)).await,
        Err(err) => Err(err),
    };
    match result {
        Ok(()) => send_current(updates),
        Err(err) => tracing::warn!("brightness {percent}%: {err}"),
    }
}

fn to_percent(raw: u32, max: u32) -> u8 {
    if max == 0 { 0 } else { (u64::from(raw.min(max)) * 100).div_ceil(u64::from(max)) as u8 }
}

/// Never 0: a backlight at 0 is a black screen with no way to see the slider.
fn to_raw(percent: u8, max: u32) -> u32 {
    ((u64::from(percent.min(100)) * u64::from(max)) / 100).max(1) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        assert_eq!(to_percent(96_000, 96_000), 100);
        assert_eq!(to_percent(1, 96_000), 1);
        assert_eq!(to_percent(48_000, 96_000), 50);
        assert_eq!(to_percent(5, 0), 0);
        assert_eq!(to_raw(0, 96_000), 1);
        assert_eq!(to_raw(50, 96_000), 48_000);
        assert_eq!(to_raw(200, 255), 255);
        for p in 1..=100 {
            assert_eq!(to_percent(to_raw(p, 96_000), 96_000), p);
        }
    }
}
