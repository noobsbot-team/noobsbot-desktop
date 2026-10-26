//! Display brightness (E2.3, E2.7): read from `/sys/class/backlight`, set through logind so no root is needed.

use smithay::reexports::calloop::channel::Sender;
use zbus::Connection;

use super::Update;
use crate::display_info;

#[zbus::proxy(interface = "org.freedesktop.login1.Session", default_service = "org.freedesktop.login1", default_path = "/org/freedesktop/login1/session/auto")]
trait Session {
    fn set_brightness(&self, subsystem: &str, name: &str, brightness: u32) -> zbus::Result<()>;
}

/// Sends the current brightness in percent; `None` without a backlight (desktop monitors).
// ponytail: read at start and after our own changes; firmware-handled brightness keys aren't seen until then.
// Listen to the backlight's udev change events if a machine needs it.
pub fn send_current(updates: &Sender<Update>) {
    let percent = display_info::brightness();
    tracing::debug!(?percent, "brightness");
    let _ = updates.send(Update::Brightness(percent));
}

pub async fn set(bus: &Connection, percent: u8, updates: &Sender<Update>) {
    let Some(name) = display_info::backlight() else { return };
    let Some(max) = display_info::backlight_number(&name, "max_brightness") else { return };
    let result = match SessionProxy::new(bus).await {
        Ok(session) => session.set_brightness("backlight", &name, to_raw(percent, max)).await,
        Err(err) => Err(err),
    };
    match result {
        Ok(()) => send_current(updates),
        Err(err) => tracing::warn!("brightness {percent}%: {err}"),
    }
}

/// Never 0: a backlight at 0 is a black screen with no way to see the slider.
fn to_raw(percent: u8, max: u32) -> u32 {
    ((u64::from(percent.min(100)) * u64::from(max)) / 100).max(1) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display_info::to_percent;

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
