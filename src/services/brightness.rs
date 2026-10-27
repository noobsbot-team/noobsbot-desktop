//! Display brightness (E2.3, E2.7): read from `/sys/class/backlight`, set through logind so no root is needed.
//! External monitors follow the same value over DDC/CI, through `ddcutil` when it is installed.

use std::{
    io,
    os::fd::AsRawFd,
    process::Command,
    sync::{OnceLock, mpsc},
    time::{Duration, Instant},
};

use smithay::reexports::{
    calloop::channel::Sender,
    udev::{EventType, MonitorBuilder},
};
use zbus::Connection;

use super::Update;
use crate::display_info;

#[zbus::proxy(interface = "org.freedesktop.login1.Session", default_service = "org.freedesktop.login1", default_path = "/org/freedesktop/login1/session/auto")]
trait Session {
    fn set_brightness(&self, subsystem: &str, name: &str, brightness: u32) -> zbus::Result<()>;
}

/// Sends the current brightness in percent; `None` without a backlight (desktop monitors). At start, after our own
/// changes, and on the backlight's udev events (`watch`).
pub fn send_current(updates: &Sender<Update>) {
    let percent = display_info::brightness();
    tracing::debug!(?percent, "brightness");
    let _ = updates.send(Update::Brightness(percent));
}

/// The first reading, then the monitors' thread. Called once, before anything sets the brightness.
pub fn start(updates: &Sender<Update>) {
    send_current(updates);
    let (tx, rx) = mpsc::channel();
    let _ = DDC.set(tx);
    let ddc_updates = updates.clone();
    if let Err(err) = std::thread::Builder::new().name("ddc".into()).spawn(move || ddc(&ddc_updates, &rx)) {
        tracing::warn!("monitor brightness off: {err}");
    }
    if display_info::backlight().is_some() {
        let updates = updates.clone();
        let watch = move || {
            if let Err(err) = watch(&updates) {
                tracing::warn!("brightness changed elsewhere not followed: {err}");
            }
        };
        if let Err(err) = std::thread::Builder::new().name("backlight".into()).spawn(watch) {
            tracing::warn!("brightness changed elsewhere not followed: {err}");
        }
    }
}

/// Follows brightness changed outside the shell (firmware keys, `brightnessctl`): the slider moves, no OSD.
// ponytail: only drivers that send a uevent on change are seen (sysfs writes and most hotkeys do).
fn watch(updates: &Sender<Update>) -> io::Result<()> {
    let socket = MonitorBuilder::new()?.match_subsystem("backlight")?.listen()?;
    let mut fd = libc::pollfd { fd: socket.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    loop {
        // SAFETY: one valid pollfd, which outlives the call.
        if unsafe { libc::poll(&mut fd, 1, -1) } < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        // Drained in full: a burst of changes is one reading.
        if socket.iter().filter(|event| event.event_type() == EventType::Change).count() > 0 {
            send_current(updates);
        }
    }
}

/// Displays were plugged or unplugged: look for DDC/CI monitors again, once they settle.
pub fn redetect() {
    if let Some(ddc) = DDC.get() {
        let _ = ddc.send(Ddc::Detect);
    }
}

/// The built-in panel (no bus: skipped) and every DDC/CI monitor.
pub async fn set(bus: Option<&Connection>, percent: u8, updates: &Sender<Update>) {
    if let Some(ddc) = DDC.get() {
        let _ = ddc.send(Ddc::Set(percent));
    }
    let Some(bus) = bus else { return };
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

enum Ddc {
    Set(u8),
    Detect,
}

/// The monitors' brightness thread.
static DDC: OnceLock<mpsc::Sender<Ddc>> = OnceLock::new();

/// A dock brings up several displays one after another, and `ddcutil detect` takes seconds: detect once they settle.
const SETTLE: Duration = Duration::from_secs(2);

/// Finds the DDC/CI monitors, again after hotplug, then sets them to each new value (not echoed: the shell already
/// shows it). Without a backlight the first monitor's reading is the slider's.
// ponytail: all monitors share the one slider; give Settings › Displays a slider each if one value is not enough.
fn ddc(updates: &Sender<Update>, rx: &mpsc::Receiver<Ddc>) {
    let mut tops = Vec::new();
    // The last value set, else (no backlight) the first monitor's reading. At login monitors keep their own.
    let mut current = None;
    detect(&mut tops, &mut current, updates);
    let mut due: Option<Instant> = None;
    loop {
        let msg = match due {
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(msg) => msg,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    due = None;
                    current = display_info::brightness().or(current);
                    detect(&mut tops, &mut current, updates);
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            },
            None => match rx.recv() {
                Ok(msg) => msg,
                Err(_) => return,
            },
        };
        match msg {
            Ddc::Detect => due = Some(Instant::now() + SETTLE),
            Ddc::Set(mut percent) => {
                // ponytail: a call takes 50–200 ms, so a slider drag is coalesced to its latest value; a stuck monitor
                // still holds up the others (one thread each if that matters).
                let mut detect_too = false;
                for msg in rx.try_iter() {
                    match msg {
                        Ddc::Set(p) => percent = p,
                        Ddc::Detect => detect_too = true,
                    }
                }
                if detect_too {
                    due = Some(Instant::now() + SETTLE);
                }
                current = Some(percent);
                for &(bus, top) in &tops {
                    set_vcp(bus, top, percent);
                }
            }
        }
    }
}

/// Replaces `tops` (bus, maximum) with the monitors `ddcutil detect` finds; a new one is set to `current`, or, without
/// a backlight, gives the slider its reading.
fn detect(tops: &mut Vec<(u32, u32)>, current: &mut Option<u8>, updates: &Sender<Update>) {
    let buses = ddcutil(&["detect", "--brief"]).map(|out| parse_detect(&out)).unwrap_or_default();
    tracing::debug!(?buses, "ddc monitors");
    let read = |bus: u32| ddcutil(&["-b", &bus.to_string(), "getvcp", "10", "--brief"]).and_then(|out| parse_vcp(&out));
    let old = std::mem::take(tops);
    for bus in buses {
        if let Some(&known) = old.iter().find(|(b, _)| *b == bus) {
            tops.push(known);
            continue;
        }
        let reading = read(bus);
        match (*current, reading) {
            (Some(percent), _) => set_vcp(bus, reading.map_or(100, |(_, top)| top), percent),
            (None, Some((now, top))) if display_info::backlight().is_none() => {
                let percent = display_info::to_percent(now, top);
                *current = Some(percent);
                let _ = updates.send(Update::Brightness(Some(percent)));
            }
            (None, _) => {}
        }
        // A monitor that didn't answer gets the usual maximum.
        tops.push((bus, reading.map_or(100, |(_, top)| top)));
    }
}

fn set_vcp(bus: u32, top: u32, percent: u8) {
    ddcutil(&["-b", &bus.to_string(), "--noverify", "setvcp", "10", &to_raw(percent, top).to_string()]);
}

/// `ddcutil`'s output, or `None` (logged) when it is missing or fails.
fn ddcutil(args: &[&str]) -> Option<String> {
    match Command::new("ddcutil").args(args).output() {
        Ok(out) if out.status.success() => Some(String::from_utf8_lossy(&out.stdout).into_owned()),
        Ok(out) => {
            tracing::debug!("ddcutil {args:?}: {}", String::from_utf8_lossy(&out.stderr).trim());
            None
        }
        Err(err) => {
            tracing::debug!("ddcutil: {err}");
            None
        }
    }
}

/// The I2C bus numbers of the working displays in `ddcutil detect --brief` (not "Invalid display" or "Phantom").
fn parse_detect(out: &str) -> Vec<u32> {
    let mut buses = Vec::new();
    let mut valid = false;
    for line in out.lines() {
        if !line.starts_with(' ') && !line.is_empty() {
            valid = line.starts_with("Display ");
        } else if valid && let Some(bus) = line.trim().strip_prefix("I2C bus:") {
            buses.extend(bus.trim().strip_prefix("/dev/i2c-").and_then(|n| n.parse::<u32>().ok()));
        }
    }
    buses
}

/// Current and maximum from `getvcp 10 --brief` (`VCP 10 C 50 100`).
fn parse_vcp(out: &str) -> Option<(u32, u32)> {
    let mut words = out.split_whitespace();
    if words.next()? != "VCP" || words.nth(1)? != "C" {
        return None;
    }
    let (now, max) = (words.next()?.parse().ok()?, words.next()?.parse().ok()?);
    (max > 0).then_some((now, max))
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
    fn ddcutil_output() {
        let detect = "Invalid display\n   I2C bus:          /dev/i2c-11\n   DRM connector:    card1-eDP-1\n\n\
                      Display 1\n   I2C bus:          /dev/i2c-4\n   DRM connector:    card1-DP-1\n   Monitor:          DEL:DELL U2415:X\n\n\
                      Phantom display\n   I2C bus:          /dev/i2c-6\n\n\
                      Display 2\n   I2C bus:          /dev/i2c-5\n";
        assert_eq!(parse_detect(detect), [4, 5]);
        assert_eq!(parse_detect("No displays found.\n"), [] as [u32; 0]);
        assert_eq!(parse_vcp("VCP 10 C 50 100\n"), Some((50, 100)));
        assert_eq!(parse_vcp("VCP 10 ERR\n"), None);
        assert_eq!(parse_vcp("VCP 10 C 5 0"), None);
    }

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
