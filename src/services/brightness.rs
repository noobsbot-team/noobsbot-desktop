//! Display brightness (E2.3, E2.7): read from `/sys/class/backlight`, set through logind so no root is needed.
//! External monitors follow the same value over DDC/CI, through `ddcutil` when it is installed; Settings › Displays
//! sets each display on its own.

use std::{
    fs, io,
    os::fd::AsRawFd,
    path::Path,
    process::Command,
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
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

/// Our own changes to every display asked for (`Services::send` counts them) and not yet done. A drag asks once per
/// pixel: only the newest is set, and readings meanwhile are stale, pulling the slider back under the finger.
pub static PENDING: AtomicUsize = AtomicUsize::new(0);

/// Sends the current brightness in percent; `None` without a backlight (desktop monitors). At start, after our own
/// changes, and on the backlight's udev events (`watch`). Not while one of ours is pending: its own reading follows.
pub fn send_current(updates: &Sender<Update>) {
    if PENDING.load(Ordering::Acquire) > 0 {
        return;
    }
    let percent = panel();
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
    if backlight().is_some() {
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

/// The built-in panel (no bus: skipped) and every DDC/CI monitor; skipped when a newer value is queued (`PENDING`).
pub async fn set(bus: Option<&Connection>, percent: u8, updates: &Sender<Update>) {
    let newest = PENDING.load(Ordering::Acquire) <= 1;
    if newest && let Some(ddc) = DDC.get() {
        let _ = ddc.send(Ddc::Set(percent));
    }
    let done = newest && set_backlight(bus, percent).await;
    PENDING.fetch_sub(1, Ordering::AcqRel);
    if done {
        send_current(updates);
    }
}

/// One display, by connector: the built-in panel's backlight, or a DDC/CI monitor.
pub async fn set_output(bus: Option<&Connection>, connector: String, percent: u8, updates: &Sender<Update>) {
    if display_info::built_in(&connector) {
        if set_backlight(bus, percent).await {
            send_current(updates);
        }
    } else if let Some(ddc) = DDC.get() {
        let _ = ddc.send(Ddc::SetOne(connector, percent));
    }
}

/// The built-in panel, through logind; no bus: skipped. Whether it was set.
async fn set_backlight(bus: Option<&Connection>, percent: u8) -> bool {
    let Some(bus) = bus else { return false };
    let Some(name) = backlight() else { return false };
    let Some(max) = backlight_number(&name, "max_brightness") else { return false };
    let result = match SessionProxy::new(bus).await {
        Ok(session) => session.set_brightness("backlight", &name, to_raw(percent, max)).await,
        Err(err) => Err(err),
    };
    result.inspect_err(|err| tracing::warn!("brightness {percent}%: {err}")).is_ok()
}

enum Ddc {
    /// Every monitor.
    Set(u8),
    /// One monitor, by connector.
    SetOne(String, u8),
    Detect,
}

/// A DDC/CI monitor.
struct Monitor {
    bus: u32,
    /// The brightness's maximum (VCP 10).
    top: u32,
    /// `DP-1`; empty when ddcutil can't tell.
    connector: String,
    /// Percent, as last set or read; `None` when unknown.
    percent: Option<u8>,
    /// Percent to set next: the latest of a drag.
    want: Option<u8>,
}

/// The monitors' brightness thread.
static DDC: OnceLock<mpsc::Sender<Ddc>> = OnceLock::new();

/// A dock brings up several displays one after another, and `ddcutil detect` takes seconds: detect once they settle.
const SETTLE: Duration = Duration::from_secs(2);

/// Finds the DDC/CI monitors, again after hotplug, then sets them, all or one, to each new value. Without a backlight
/// the first monitor's reading is the slider's.
fn ddc(updates: &Sender<Update>, rx: &mpsc::Receiver<Ddc>) {
    let mut monitors = Vec::new();
    // The last value set to all, else (no backlight) the first monitor's reading. At login monitors keep their own.
    let mut current = None;
    detect(&mut monitors, &mut current, updates);
    let mut due: Option<Instant> = None;
    loop {
        let msg = match due {
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(msg) => msg,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    due = None;
                    current = panel().or(current);
                    detect(&mut monitors, &mut current, updates);
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            },
            None => match rx.recv() {
                Ok(msg) => msg,
                Err(_) => return,
            },
        };
        // ponytail: a call takes 50–200 ms, so a slider drag is coalesced to its latest value per monitor; a stuck
        // monitor still holds up the others (one thread each if that matters).
        if queue(&mut monitors, &mut current, std::iter::once(msg).chain(rx.try_iter())) {
            due = Some(Instant::now() + SETTLE);
        }
        for m in &mut monitors {
            if let Some(percent) = m.want.take() {
                set_vcp(m.bus, m.top, percent);
                m.percent = Some(percent);
            }
        }
        send_levels(&monitors, updates);
    }
}

/// Takes queued messages, oldest first: each monitor's `want` becomes the latest value meant for it. Whether one
/// asked to detect again.
fn queue(monitors: &mut [Monitor], current: &mut Option<u8>, msgs: impl Iterator<Item = Ddc>) -> bool {
    let mut detect = false;
    for msg in msgs {
        match msg {
            Ddc::Set(percent) => {
                *current = Some(percent);
                monitors.iter_mut().for_each(|m| m.want = Some(percent));
            }
            Ddc::SetOne(connector, percent) => monitors.iter_mut().filter(|m| m.connector == connector).for_each(|m| m.want = Some(percent)),
            Ddc::Detect => detect = true,
        }
    }
    detect
}

/// Each monitor's brightness by connector, for Settings › Displays (through `displays.json`).
fn send_levels(monitors: &[Monitor], updates: &Sender<Update>) {
    let levels = monitors.iter().filter(|m| !m.connector.is_empty()).filter_map(|m| Some((m.connector.clone(), m.percent?))).collect();
    let _ = updates.send(Update::Monitors(levels));
}

/// Replaces `monitors` with the ones `ddcutil detect` finds. A new one is set to a value set before (keeping its own
/// at login), or, without a backlight, gives the slider its reading.
fn detect(monitors: &mut Vec<Monitor>, current: &mut Option<u8>, updates: &Sender<Update>) {
    let found = ddcutil(&["detect", "--brief"]).map(|out| parse_detect(&out)).unwrap_or_default();
    tracing::debug!(?found, "ddc monitors");
    let read = |bus: u32| ddcutil(&["-b", &bus.to_string(), "getvcp", "10", "--brief"]).and_then(|out| parse_vcp(&out));
    let set_to = *current;
    let mut old = std::mem::take(monitors);
    for (bus, connector) in found {
        if let Some(known) = old.iter().position(|m| m.bus == bus) {
            monitors.push(old.swap_remove(known));
            continue;
        }
        let reading = read(bus);
        // A monitor that didn't answer gets the usual maximum.
        let top = reading.map_or(100, |(_, top)| top);
        let percent = match (set_to, reading) {
            (Some(percent), _) => {
                set_vcp(bus, top, percent);
                Some(percent)
            }
            (None, Some((now, top))) => {
                let percent = to_percent(now, top);
                if current.is_none() && backlight().is_none() {
                    *current = Some(percent);
                    let _ = updates.send(Update::Brightness(Some(percent)));
                }
                Some(percent)
            }
            (None, None) => None,
        };
        monitors.push(Monitor { bus, top, connector, percent, want: None });
    }
    send_levels(monitors, updates);
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

/// The working displays in `ddcutil detect --brief` (not "Invalid display" or "Phantom"): I2C bus number and connector
/// (`DP-1`; empty when ddcutil can't tell).
fn parse_detect(out: &str) -> Vec<(u32, String)> {
    let mut found = Vec::new();
    let mut valid = false;
    for line in out.lines() {
        if !line.starts_with(' ') && !line.is_empty() {
            valid = line.starts_with("Display ");
            if valid {
                found.push((None, String::new()));
            }
        } else if let (true, Some(last)) = (valid, found.last_mut()) {
            let (key, value) = line.split_once(':').unwrap_or_default();
            match key.trim() {
                "I2C bus" => last.0 = value.trim().strip_prefix("/dev/i2c-").and_then(|n| n.parse::<u32>().ok()),
                // `card1-DP-1`: the compositor's name has no card.
                "DRM connector" => last.1 = value.trim().split_once('-').map_or("", |(_, name)| name).to_owned(),
                _ => {}
            }
        }
    }
    found.into_iter().filter_map(|(bus, connector)| Some((bus?, connector))).collect()
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

const BACKLIGHT: &str = "/sys/class/backlight";

/// The built-in panel's backlight: firmware interfaces first, then platform, then raw (the order logind uses).
fn backlight() -> Option<String> {
    let rank = |name: &str| match fs::read_to_string(Path::new(BACKLIGHT).join(name).join("type")).ok()?.trim() {
        "firmware" => Some(0),
        "platform" => Some(1),
        "raw" => Some(2),
        _ => None,
    };
    let names = fs::read_dir(BACKLIGHT).ok()?.filter_map(|e| e.ok()?.file_name().into_string().ok());
    names.filter_map(|name| Some((rank(&name)?, name))).min().map(|(_, name)| name)
}

/// A number from the backlight's sysfs `file` (`brightness`, `max_brightness`).
fn backlight_number(name: &str, file: &str) -> Option<u32> {
    fs::read_to_string(Path::new(BACKLIGHT).join(name).join(file)).ok()?.trim().parse().ok()
}

/// The built-in panel's brightness in percent; `None` without a backlight (desktop monitors).
pub fn panel() -> Option<u8> {
    let name = backlight()?;
    Some(to_percent(backlight_number(&name, "brightness")?, backlight_number(&name, "max_brightness")?))
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
    fn ddcutil_output() {
        let detect = "Invalid display\n   I2C bus:          /dev/i2c-11\n   DRM connector:    card1-eDP-1\n\n\
                      Display 1\n   I2C bus:          /dev/i2c-4\n   DRM connector:    card1-DP-1\n   drm_connector_id: 508\n   Monitor:          DEL:DELL U2415:X\n\n\
                      Phantom display\n   I2C bus:          /dev/i2c-6\n\n\
                      Display 2\n   I2C bus:          /dev/i2c-5\n";
        assert_eq!(parse_detect(detect), [(4, "DP-1".to_owned()), (5, String::new())]);
        assert!(parse_detect("No displays found.\n").is_empty());
        assert_eq!(parse_vcp("VCP 10 C 50 100\n"), Some((50, 100)));
        assert_eq!(parse_vcp("VCP 10 ERR\n"), None);
        assert_eq!(parse_vcp("VCP 10 C 5 0"), None);
    }

    #[test]
    fn a_drag_sets_each_monitor_to_its_latest_value() {
        let monitor = |connector: &str| Monitor { bus: 0, top: 100, connector: connector.into(), percent: None, want: None };
        let mut monitors = [monitor("DP-1"), monitor("DP-2")];
        let mut current = None;
        let msgs = [Ddc::SetOne("DP-1".into(), 70), Ddc::Set(40), Ddc::SetOne("DP-2".into(), 10), Ddc::SetOne("DP-2".into(), 20)];
        assert!(!queue(&mut monitors, &mut current, msgs.into_iter()));
        assert_eq!((monitors[0].want, monitors[1].want, current), (Some(40), Some(20), Some(40)));
        assert!(queue(&mut monitors, &mut current, [Ddc::Detect, Ddc::SetOne("HDMI-A-1".into(), 5)].into_iter()));
        assert_eq!((monitors[0].want, monitors[1].want, current), (Some(40), Some(20), Some(40)), "no other monitor");
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

    #[test]
    fn a_drag_hides_readings_until_its_last_change() {
        let mut events = smithay::reexports::calloop::EventLoop::<usize>::try_new().unwrap();
        let (tx, rx) = smithay::reexports::calloop::channel::channel();
        events.handle().insert_source(rx, |_, _, readings| *readings += 1).unwrap();
        let mut readings = 0;
        // Three changes queued: the udev reading of the first is stale, and so are the first two.
        PENDING.store(3, Ordering::Release);
        send_current(&tx);
        futures_lite::future::block_on(set(None, 40, &tx));
        futures_lite::future::block_on(set(None, 45, &tx));
        events.dispatch(Some(Duration::ZERO), &mut readings).unwrap();
        assert_eq!((PENDING.load(Ordering::Acquire), readings), (1, 0));
        futures_lite::future::block_on(set(None, 50, &tx));
        send_current(&tx);
        events.dispatch(Some(Duration::ZERO), &mut readings).unwrap();
        assert_eq!((PENDING.load(Ordering::Acquire), readings), (0, 1), "the last one done: readings again");
    }
}
