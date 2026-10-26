//! What the system pages show: NetworkManager, BlueZ, UPower and power-profiles-daemon over D-Bus, PipeWire through `wpctl` (the desktop's own
//! paths, `src/services/`), and the About facts from `/proc`, `/sys` and `/etc`. Read once when a page opens.
//! ponytail: no change signals; the page re-reads after its own changes, a live follow if that falls short.

use std::{os::unix::net::UnixStream, process::Command, time::Duration};

use zbus::{
    Connection,
    fdo::{ObjectManagerProxy, PropertiesProxy},
    names::InterfaceName,
    zvariant::{OwnedValue, Value},
};

#[derive(Clone, Debug, Default)]
pub struct Status {
    /// `None`: NetworkManager isn't running.
    pub wifi: Option<Wifi>,
    /// `None`: no adapter, or BlueZ isn't running.
    pub bluetooth: Option<Bluetooth>,
    pub output: Option<Device>,
    /// The output's left/right balance, -1 … 1; `None` without `pactl`.
    pub balance: Option<f32>,
    pub input: Option<Device>,
    /// `None`: systemd-timedated isn't there.
    pub time: Option<Time>,
    /// `None`: no battery (a desktop), or UPower isn't running.
    pub battery: Option<Battery>,
    /// `None`: power-profiles-daemon isn't running.
    pub power_mode: Option<PowerMode>,
    /// The keyboard backlight in percent; `None` without one.
    pub kbd_backlight: Option<u8>,
    /// Every output and input, for the pickers.
    pub outputs: Vec<Choice>,
    pub inputs: Vec<Choice>,
}

#[derive(Clone, Debug)]
pub struct Time {
    /// Set automatically (NTP).
    pub ntp: bool,
    /// An NTP service is installed.
    pub can_ntp: bool,
    /// The system's time zone.
    pub zone: String,
}

/// UPower's display device (the desktop's `src/services/power.rs`).
#[derive(Clone, Debug)]
pub struct Battery {
    pub percent: u8,
    /// UPower's state: 1 charging, 2 discharging, 4 fully charged, 5/6 pending charge/discharge.
    pub state: u32,
    /// Until empty while discharging, until full while charging; 0 while estimating.
    pub seconds: i64,
}

/// power-profiles-daemon (the desktop's `src/services/profiles.rs`).
#[derive(Clone, Debug)]
pub struct PowerMode {
    /// `power-saver`, `balanced` or `performance`.
    pub active: String,
    pub available: Vec<String>,
}

/// A PipeWire sink or source from `wpctl status`.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub id: u32,
    pub name: String,
    /// The default one.
    pub current: bool,
}

#[derive(Clone, Debug)]
pub struct Wifi {
    pub enabled: bool,
    /// The primary connection's name.
    pub connection: Option<String>,
    pub network: crate::network::Network,
}

#[derive(Clone, Debug)]
pub struct Bluetooth {
    pub powered: bool,
    /// Paired devices, and named unpaired ones seen nearby, by name.
    pub devices: Vec<BtDevice>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BtDevice {
    /// BlueZ's object path.
    pub path: String,
    pub name: String,
    pub paired: bool,
    pub connected: bool,
}

#[derive(Clone, Debug)]
pub struct Device {
    pub name: String,
    pub volume: u8,
    pub muted: bool,
}

const NM: &str = "org.freedesktop.NetworkManager";

pub async fn read() -> Status {
    let system = system(2).await.ok();
    let (wifi, bluetooth, time, battery, power_mode) = match &system {
        Some(bus) => (wifi(bus).await, bluetooth(bus).await, time(bus).await, battery(bus).await, power_mode(bus).await),
        None => (None, None, None, None, None),
    };
    let kbd_backlight = match &system {
        Some(bus) => kbd_backlight(bus, None).await,
        None => None,
    };
    let pw = wpctl(&["status"]).unwrap_or_default();
    let pactl = crate::pactl::read();
    Status {
        wifi,
        bluetooth,
        output: device(OUTPUT).map(|mut d| {
            // wpctl reads one channel; with the balance off centre the louder one is the volume.
            d.volume = pactl.filter(|(_, b)| *b != 0.0).map_or(d.volume, |(loudest, _)| loudest);
            d
        }),
        balance: pactl.map(|(_, b)| b),
        input: device(INPUT),
        time,
        battery,
        power_mode,
        kbd_backlight,
        outputs: section(&pw, "Sinks:"),
        inputs: section(&pw, "Sources:"),
    }
}

/// The system bus, as the desktop connects (`src/services/mod.rs`): on this thread, not zbus's pool, whose idle
/// thread wakes every 500 ms. `timeout`: seconds a call may take.
pub async fn system(timeout: u64) -> zbus::Result<Connection> {
    let address = std::env::var("DBUS_SYSTEM_BUS_ADDRESS").unwrap_or_default();
    let path = address.strip_prefix("unix:path=").map_or("/run/dbus/system_bus_socket", |rest| rest.split(',').next().unwrap_or(rest));
    let socket = UnixStream::connect(path)?;
    zbus::connection::Builder::async_io_unix_stream(socket).method_timeout(Duration::from_secs(timeout)).build().await
}

async fn property(bus: &Connection, service: &str, path: &str, interface: &str, name: &str) -> zbus::Result<OwnedValue> {
    let proxy = PropertiesProxy::builder(bus).destination(service.to_owned())?.path(path.to_owned())?.build().await?;
    Ok(proxy.get(InterfaceName::try_from(interface.to_owned())?, name).await?)
}

async fn set_property(bus: &Connection, service: &str, path: &str, interface: &str, name: &str, value: Value<'_>) -> zbus::Result<()> {
    let proxy = PropertiesProxy::builder(bus).destination(service.to_owned())?.path(path.to_owned())?.build().await?;
    Ok(proxy.set(InterfaceName::try_from(interface.to_owned())?, name, value).await?)
}

async fn wifi(bus: &Connection) -> Option<Wifi> {
    let enabled = bool::try_from(property(bus, NM, "/org/freedesktop/NetworkManager", NM, "WirelessEnabled").await.ok()?).ok()?;
    let primary = property(bus, NM, "/org/freedesktop/NetworkManager", NM, "PrimaryConnection").await.ok();
    let primary = primary.and_then(|v| zbus::zvariant::OwnedObjectPath::try_from(v).ok()).filter(|p| p.as_str() != "/");
    let connection = match primary {
        Some(path) => {
            property(bus, NM, path.as_str(), "org.freedesktop.NetworkManager.Connection.Active", "Id").await.ok().and_then(|v| String::try_from(v).ok())
        }
        None => None,
    };
    Some(Wifi { enabled, connection, network: crate::network::read(bus).await })
}

async fn bluetooth(bus: &Connection) -> Option<Bluetooth> {
    let manager = ObjectManagerProxy::builder(bus).destination("org.bluez").ok()?.path("/").ok()?.build().await.ok()?;
    let objects = manager.get_managed_objects().await.ok()?;
    let field = |interfaces: &std::collections::HashMap<_, std::collections::HashMap<String, OwnedValue>>, interface: &str, name: &str| {
        interfaces
            .iter()
            .find(|(i, _): &(&zbus::names::OwnedInterfaceName, _)| i.as_str() == interface)
            .and_then(|(_, p)| p.get(name))
            .and_then(|v| v.try_clone().ok())
    };
    let mut powered = None;
    let mut devices = Vec::new();
    for (path, interfaces) in &objects {
        if let Some(on) = field(interfaces, "org.bluez.Adapter1", "Powered") {
            powered = Some(powered.unwrap_or(false) | bool::try_from(on).unwrap_or(false));
        }
        let paired = field(interfaces, "org.bluez.Device1", "Paired").and_then(|v| bool::try_from(v).ok()).unwrap_or(false);
        // Unnamed ones nearby (an address only) are left out.
        if paired || field(interfaces, "org.bluez.Device1", "Name").is_some() {
            let name = field(interfaces, "org.bluez.Device1", "Alias").and_then(|v| String::try_from(v).ok()).unwrap_or_default();
            let connected = field(interfaces, "org.bluez.Device1", "Connected").and_then(|v| bool::try_from(v).ok()).unwrap_or(false);
            devices.push(BtDevice { path: path.to_string(), name, paired, connected });
        }
    }
    devices.sort_by(|a, b| a.name.cmp(&b.name));
    Some(Bluetooth { powered: powered?, devices })
}

const UPOWER: &str = "org.freedesktop.UPower";
const DISPLAY_DEVICE: &str = "/org/freedesktop/UPower/devices/DisplayDevice";

async fn battery(bus: &Connection) -> Option<Battery> {
    let get = |name| property(bus, UPOWER, DISPLAY_DEVICE, "org.freedesktop.UPower.Device", name);
    if !bool::try_from(get("IsPresent").await.ok()?).ok()? {
        return None;
    }
    let percent = f64::try_from(get("Percentage").await.ok()?).ok()?.round().clamp(0.0, 100.0) as u8;
    let state = u32::try_from(get("State").await.ok()?).ok()?;
    let seconds = match state {
        1 => i64::try_from(get("TimeToFull").await.ok()?).ok()?,
        2 => i64::try_from(get("TimeToEmpty").await.ok()?).ok()?,
        _ => 0,
    };
    Some(Battery { percent, state, seconds })
}

const KBD_PATH: &str = "/org/freedesktop/UPower/KbdBacklight";
const KBD: &str = "org.freedesktop.UPower.KbdBacklight";

/// UPower's keyboard backlight in percent, after setting it to `set` percent first.
async fn kbd_backlight(bus: &Connection, set: Option<u8>) -> Option<u8> {
    let call = |method| async move { bus.call_method(Some(UPOWER), KBD_PATH, Some(KBD), method, &()).await.ok()?.body().deserialize::<i32>().ok() };
    let max = call("GetMaxBrightness").await.filter(|m| *m > 0)?;
    if let Some(percent) = set {
        let level = (i32::from(percent.min(100)) * max + 50) / 100;
        let _ = bus.call_method(Some(UPOWER), KBD_PATH, Some(KBD), "SetBrightness", &(level,)).await;
    }
    Some((call("GetBrightness").await? * 100 / max) as u8)
}

/// The keyboard backlight in percent, then the new status.
pub async fn set_kbd_backlight(percent: u8) -> Status {
    if let Ok(bus) = system(2).await {
        kbd_backlight(&bus, Some(percent)).await;
    }
    read().await
}

const PROFILES: &str = "org.freedesktop.UPower.PowerProfiles";
const PROFILES_PATH: &str = "/org/freedesktop/UPower/PowerProfiles";

async fn power_mode(bus: &Connection) -> Option<PowerMode> {
    let active = String::try_from(property(bus, PROFILES, PROFILES_PATH, PROFILES, "ActiveProfile").await.ok()?).ok()?;
    let profiles =
        Vec::<std::collections::HashMap<String, OwnedValue>>::try_from(property(bus, PROFILES, PROFILES_PATH, PROFILES, "Profiles").await.ok()?).ok()?;
    let available = profiles.iter().filter_map(|p| p.get("Profile")?.downcast_ref::<String>().ok()).collect();
    Some(PowerMode { active, available })
}

/// `power-saver`, `balanced` or `performance`, then the new status.
pub async fn set_power_mode(profile: String) -> Status {
    if let Ok(bus) = system(2).await {
        let _ = set_property(&bus, PROFILES, PROFILES_PATH, PROFILES, "ActiveProfile", profile.as_str().into()).await;
    }
    read().await
}

/// Wi-Fi on or off, then the new status.
pub async fn set_wifi(on: bool) -> Status {
    if let Ok(bus) = system(2).await {
        let _ = set_property(&bus, NM, "/org/freedesktop/NetworkManager", NM, "WirelessEnabled", on.into()).await;
    }
    read().await
}

/// Every adapter on or off, then the new status.
pub async fn set_bluetooth(on: bool) -> Status {
    if let Ok(bus) = system(2).await
        && let Ok(manager) = async { ObjectManagerProxy::builder(&bus).destination("org.bluez")?.path("/")?.build().await }.await
    {
        for (path, interfaces) in manager.get_managed_objects().await.unwrap_or_default() {
            if interfaces.keys().any(|i| i.as_str() == "org.bluez.Adapter1") {
                let _ = set_property(&bus, "org.bluez", path.as_str(), "org.bluez.Adapter1", "Powered", on.into()).await;
            }
        }
    }
    read().await
}

const TIMEDATE: &str = "org.freedesktop.timedate1";
const TIMEDATE_PATH: &str = "/org/freedesktop/timedate1";

async fn time(bus: &Connection) -> Option<Time> {
    let get = |name: &'static str| property(bus, TIMEDATE, TIMEDATE_PATH, TIMEDATE, name);
    Some(Time {
        ntp: bool::try_from(get("NTP").await.ok()?).ok()?,
        can_ntp: get("CanNTP").await.ok().and_then(|v| bool::try_from(v).ok()).unwrap_or(false),
        zone: get("Timezone").await.ok().and_then(|v| String::try_from(v).ok()).unwrap_or_default(),
    })
}

/// "Set time and date automatically", then the new status. Interactive: polkit asks for the password through the
/// desktop's agent, so the call waits for the answer instead of the usual 2 s.
pub async fn set_ntp(on: bool) -> Status {
    if let Ok(bus) = system(120).await {
        let _ = bus.call_method(Some(TIMEDATE), TIMEDATE_PATH, Some(TIMEDATE), "SetNTP", &(on, true)).await;
    }
    read().await
}

/// Date & Time's manual fields, as text keys.
pub const DATE: &str = "time.date";
pub const CLOCK: &str = "time.clock";

/// The date (`2026-10-27`) and time (`14:05` or `14:05:30`) typed by hand, in the local zone, as UTC microseconds;
/// `None` if either doesn't parse.
pub fn manual_time(date: &str, time: &str) -> Option<i64> {
    use chrono::{Local, NaiveDate, NaiveTime, TimeZone};
    let date = NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d").ok()?;
    let time = NaiveTime::parse_from_str(time.trim(), "%H:%M:%S").or_else(|_| NaiveTime::parse_from_str(time.trim(), "%H:%M")).ok()?;
    // A time the clocks skip (DST) has no local reading; one they repeat takes the first.
    Some(Local.from_local_datetime(&date.and_time(time)).earliest()?.timestamp_micros())
}

/// Sets the system clock (timedated `SetTime`), then the new status; interactive like `set_ntp`.
pub async fn set_time(usec: i64) -> Status {
    if let Ok(bus) = system(120).await {
        let _ = bus.call_method(Some(TIMEDATE), TIMEDATE_PATH, Some(TIMEDATE), "SetTime", &(usec, false, true)).await;
    }
    read().await
}

/// Every time-zone name timedated accepts (zones and their aliases), from tzdata.
pub static ZONES: std::sync::LazyLock<Vec<String>> =
    std::sync::LazyLock::new(|| zones(&std::fs::read_to_string("/usr/share/zoneinfo/tzdata.zi").unwrap_or_default()));

/// `Z Asia/Kathmandu …` (a zone) and `L Asia/Kathmandu Asia/Katmandu` (an alias) lines of `tzdata.zi`, sorted.
fn zones(tzdata: &str) -> Vec<String> {
    let mut zones: Vec<String> = tzdata
        .lines()
        .filter_map(|line| match line.split_whitespace().collect::<Vec<_>>()[..] {
            ["Z", name, ..] | ["L", _, name] => Some(name.to_owned()),
            _ => None,
        })
        .collect();
    zones.sort();
    zones
}

/// The zones whose name contains `query` (any case; a space matches `_`).
pub fn matching_zones(query: &str) -> impl Iterator<Item = &'static str> {
    let query = query.trim().to_lowercase().replace(' ', "_");
    ZONES.iter().map(String::as_str).filter(move |z| z.to_lowercase().contains(&query))
}

/// While the Bluetooth page shows: every adapter looks for nearby devices. Yields nothing; BlueZ stops a discovery
/// when the connection that started it closes, which dropping the subscription does.
pub fn discover() -> impl iced::futures::Stream<Item = ()> {
    iced::stream::channel(1, async |_: iced::futures::channel::mpsc::Sender<()>| {
        let bus = system(2).await.ok();
        if let Some(bus) = &bus
            && let Ok(manager) = async { ObjectManagerProxy::builder(bus).destination("org.bluez")?.path("/")?.build().await }.await
        {
            for (path, interfaces) in manager.get_managed_objects().await.unwrap_or_default() {
                if interfaces.keys().any(|i| i.as_str() == "org.bluez.Adapter1") {
                    let _ = bus.call_method(Some("org.bluez"), path.as_str(), Some("org.bluez.Adapter1"), "StartDiscovery", &()).await;
                }
            }
        }
        std::future::pending::<()>().await;
        drop(bus);
    })
}

/// Connects a device, pairing and trusting it first if need be (`on`), or disconnects it; then the new status and the
/// error, if any. Pairing waits on the desktop's agent (its alert), so the calls get a minute.
// ponytail: no "Connecting…" while it runs; the row changes when it is done.
pub async fn connect_device(path: String, on: bool) -> (Status, Option<String>) {
    let result = async {
        let bus = system(60).await?;
        let call = |method| bus.call_method(Some("org.bluez"), path.as_str(), Some("org.bluez.Device1"), method, &());
        if !on {
            return call("Disconnect").await.map(drop);
        }
        if !bool::try_from(property(&bus, "org.bluez", &path, "org.bluez.Device1", "Paired").await?)? {
            call("Pair").await?;
        }
        set_property(&bus, "org.bluez", &path, "org.bluez.Device1", "Trusted", true.into()).await?;
        call("Connect").await.map(drop)
    };
    let error = result.await.err().map(|err| format!("Couldn't {}: {err}", if on { "connect" } else { "disconnect" }));
    (read().await, error)
}

/// Removes a paired device from its adapter (the path's parent), then the new status and the error, if any.
pub async fn forget(path: String) -> (Status, Option<String>) {
    let result = async {
        let bus = system(10).await?;
        let adapter = path.rsplit_once('/').map_or("/", |(adapter, _)| adapter);
        let device = zbus::zvariant::ObjectPath::try_from(path.as_str())?;
        bus.call_method(Some("org.bluez"), adapter, Some("org.bluez.Adapter1"), "RemoveDevice", &(device,)).await.map(drop)
    };
    let error = result.await.err().map(|err| format!("Couldn't forget the device: {err}"));
    (read().await, error)
}

/// Text field key prefix for renaming a paired device: `bt.rename.` + its BlueZ path.
pub const RENAME: &str = "bt.rename.";

/// Gives a device a new name (BlueZ's `Alias`; empty brings back its own), then the new status and the error, if any.
pub async fn rename_device(path: String, name: String) -> (Status, Option<String>) {
    let result = async {
        let bus = system(10).await?;
        set_property(&bus, "org.bluez", &path, "org.bluez.Device1", "Alias", name.as_str().into()).await
    };
    let error = result.await.err().map(|err| format!("Couldn't rename the device: {err}"));
    (read().await, error)
}

const OUTPUT: &str = "@DEFAULT_AUDIO_SINK@";
const INPUT: &str = "@DEFAULT_AUDIO_SOURCE@";

fn wpctl(args: &[&str]) -> Option<String> {
    let out = Command::new("wpctl").args(args).stderr(std::process::Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn device(target: &str) -> Option<Device> {
    let (volume, muted) = parse_volume(&wpctl(&["get-volume", target])?)?;
    Some(Device { name: parse_name(&wpctl(&["inspect", target]).unwrap_or_default()), volume, muted })
}

/// Output (`true`) or input volume in percent, then the new status.
pub async fn set_volume(output: bool, percent: u8) -> Status {
    // wpctl would centre the balance; pactl keeps it.
    if output && crate::pactl::read().is_some_and(|(_, balance)| balance != 0.0 && crate::pactl::set(percent.min(100), balance)) {
        return read().await;
    }
    let _ = wpctl(&["set-volume", if output { OUTPUT } else { INPUT }, &format!("{}%", percent.min(100))]);
    read().await
}

/// The freedesktop sound theme's sounds, the ones the desktop can always play: (name, label), by label.
pub static SOUNDS: std::sync::LazyLock<Vec<(String, String)>> = std::sync::LazyLock::new(|| {
    let files = std::fs::read_dir(SOUND_DIR).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    sounds(files)
});

const SOUND_DIR: &str = "/usr/share/sounds/freedesktop/stereo";

/// `dialog-warning.oga` → ("dialog-warning", "Dialog warning"); test tones and channel checks left out.
fn sounds(files: Vec<String>) -> Vec<(String, String)> {
    let mut out: Vec<_> = files
        .into_iter()
        .filter_map(|file| Some(file.strip_suffix(".oga")?.to_owned()))
        .filter(|name| !name.is_empty() && !name.starts_with("audio-channel-") && name != "audio-test-signal")
        .map(|name| {
            let mut label = name.replace(['-', '_'], " ");
            if let Some(first) = label.get_mut(..1) {
                first.make_ascii_uppercase();
            }
            (name, label)
        })
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

/// Plays theme sound `name` as the desktop does: the sound theme's, else the freedesktop file straight to PipeWire.
pub fn play(name: &str) {
    let name = name.to_owned();
    std::thread::spawn(move || {
        let quiet = |c: &mut Command| c.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success());
        if !quiet(Command::new("canberra-gtk-play").args(["-i", &name])) {
            quiet(Command::new("pw-play").arg(format!("{SOUND_DIR}/{name}.oga")));
        }
    });
}

/// The default microphone's level, 0 … 1, twenty times a second while subscribed: `pw-record` to a pipe, ended
/// when the subscription is.
pub fn level() -> impl iced::futures::Stream<Item = f32> {
    iced::stream::channel(1, async |tx: iced::futures::channel::mpsc::Sender<f32>| {
        let mut sender = tx.clone();
        let spawned = std::thread::Builder::new().name("level".into()).spawn(move || {
            use std::io::Read;
            let args = ["--rate", "16000", "--channels", "1", "--format", "s16", "-"];
            let Ok(mut child) = Command::new("pw-record").args(args).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn() else {
                return;
            };
            let mut stdout = child.stdout.take().expect("piped");
            // 50 ms of 16 kHz mono s16.
            let mut chunk = [0u8; 1600];
            while !sender.is_closed() && stdout.read_exact(&mut chunk).is_ok() {
                let _ = sender.try_send(meter(chunk.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b))));
            }
            let _ = child.kill();
            let _ = child.wait();
        });
        if let Err(err) = spawned {
            eprintln!("level: {err}");
        }
        std::future::pending::<()>().await;
        drop(tx);
    })
}

/// The peak of `samples` on a 60 dB scale: -60 dBFS and below is 0, full scale is 1.
pub fn meter(samples: impl Iterator<Item = i16>) -> f32 {
    let peak = samples.map(i16::unsigned_abs).max().unwrap_or(0);
    let db = 20.0 * (f32::from(peak) / 32768.0).max(1e-6).log10();
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// The output's balance, -1 left … 1 right, its loudest channel kept; then the new status.
pub async fn set_balance(balance: f32) -> Status {
    if let Some((loudest, _)) = crate::pactl::read() {
        crate::pactl::set(loudest, balance);
    }
    read().await
}

pub async fn set_muted(output: bool, muted: bool) -> Status {
    let _ = wpctl(&["set-mute", if output { OUTPUT } else { INPUT }, if muted { "1" } else { "0" }]);
    read().await
}

/// Makes sink or source `id` the default, then the new status.
pub async fn set_default(id: u32) -> Status {
    let _ = wpctl(&["set-default", &id.to_string()]);
    read().await
}

/// The first (audio) `Sinks:` or `Sources:` section of `wpctl status`: `│  *   63. Speaker   [vol: 0.89]`. The
/// desktop's `sound::parse_outputs`.
fn section(status: &str, header: &str) -> Vec<Choice> {
    let lines = status.lines().skip_while(|line| !line.ends_with(header)).skip(1);
    // The section ends at its blank `│` line.
    lines
        .map_while(|line| {
            let line = line.trim_start_matches([' ', '│']);
            let (id, rest) = line.trim_start_matches(['*', ' ']).split_once(". ")?;
            let name = rest.rsplit_once(" [vol:").map_or(rest, |(name, _)| name).trim();
            Some(Choice { id: id.parse().ok()?, name: name.to_owned(), current: line.starts_with('*') })
        })
        .collect()
}

/// `Volume: 0.40 [MUTED]` → (40, true). The desktop's `sound::parse_volume`.
fn parse_volume(text: &str) -> Option<(u8, bool)> {
    let mut words = text.strip_prefix("Volume:")?.split_whitespace();
    let volume: f32 = words.next()?.parse().ok()?;
    Some(((volume * 100.0).round().clamp(0.0, 150.0) as u8, words.any(|w| w == "[MUTED]")))
}

/// `node.nick`, else `node.description`, from `wpctl inspect`.
fn parse_name(inspect: &str) -> String {
    let value = |key: &str| {
        inspect.lines().find_map(|line| {
            let rest = line.trim_start_matches([' ', '*']).strip_prefix(key)?.trim_start().strip_prefix('=')?;
            Some(rest.trim().trim_matches('"').to_owned())
        })
    };
    value("node.nick").or_else(|| value("node.description")).unwrap_or_default()
}

/// About: label, value.
pub fn about() -> Vec<(&'static str, String)> {
    let read = |path: &str| std::fs::read_to_string(path).unwrap_or_default();
    let cpuinfo = read("/proc/cpuinfo");
    let cpu = field(&cpuinfo, "model name").unwrap_or("Unknown").to_owned();
    let cores = cpuinfo.lines().filter(|l| l.starts_with("processor")).count();
    let memory = field(&read("/proc/meminfo"), "MemTotal").and_then(|v| v.trim_end_matches(" kB").trim().parse::<f64>().ok());
    let os = read("/etc/os-release").lines().find_map(|l| l.strip_prefix("PRETTY_NAME=")).map(|v| v.trim_matches('"').to_owned());
    let mut gpus: Vec<String> = displays_root()
        .filter(|card| !card.contains('-'))
        .filter_map(|card| field(&read(&format!("/sys/class/drm/{card}/device/uevent")).replace('=', ":"), "DRIVER").map(gpu))
        .collect();
    gpus.dedup();
    vec![
        ("Name", read("/proc/sys/kernel/hostname").trim().to_owned()),
        ("Processor", format!("{cpu} ({cores} threads)")),
        ("Memory", memory.map_or("Unknown".into(), |kb| format!("{} GB", installed(kb)))),
        ("Graphics", if gpus.is_empty() { "Unknown".into() } else { gpus.join(", ") }),
        ("System", os.unwrap_or_else(|| "Linux".into())),
        ("Kernel", read("/proc/sys/kernel/osrelease").trim().to_owned()),
        ("Disk", disk().unwrap_or_else(|| "Unknown".into())),
    ]
}

/// The kernel driver as the maker's name.
fn gpu(driver: &str) -> String {
    match driver {
        "i915" | "xe" => "Intel Graphics".into(),
        "amdgpu" | "radeon" => "AMD Radeon Graphics".into(),
        "nvidia" | "nouveau" => "NVIDIA Graphics".into(),
        other => other.to_owned(),
    }
}

/// `MemTotal` (kB) as the installed size: the kernel keeps some back, so round up to an even GB.
fn installed(kb: f64) -> u64 {
    ((kb / 1024.0 / 1024.0 / 2.0).ceil() * 2.0) as u64
}

fn displays_root() -> impl Iterator<Item = String> {
    let entries = std::fs::read_dir("/sys/class/drm").into_iter().flatten().flatten();
    let mut names: Vec<String> = entries.map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("card")).collect();
    names.sort();
    names.into_iter()
}

/// Connected monitors: connector (`eDP-1`), preferred mode (`2880x1800`).
pub fn displays() -> Vec<(String, String)> {
    let read = |path: String| std::fs::read_to_string(path).unwrap_or_default();
    displays_root()
        .filter_map(|card| {
            let connector = card.split_once('-')?.1.to_owned();
            (read(format!("/sys/class/drm/{card}/status")).trim() == "connected").then(|| {
                let mode = read(format!("/sys/class/drm/{card}/modes")).lines().next().unwrap_or("").replace('x', " × ");
                (connector, mode)
            })
        })
        .collect()
}

/// `key : value` lines (`/proc/cpuinfo`, `/proc/meminfo`).
fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        (k.trim() == key).then(|| v.trim())
    })
}

/// Free of total on `/`, from `df` (no statvfs in std).
fn disk() -> Option<String> {
    let out = Command::new("df").args(["-B1", "--output=size,avail", "/"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut numbers = text.lines().nth(1)?.split_whitespace().map(|n| n.parse::<f64>().ok());
    let (size, avail) = (numbers.next()??, numbers.next()??);
    let gb = |b: f64| b / 1e9;
    Some(format!("{:.0} GB available of {:.0} GB", gb(avail), gb(size)))
}

/// The login name and the full name from `/etc/passwd`.
pub fn user() -> (String, String) {
    let login = std::env::var("USER").unwrap_or_default();
    let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
    let full = passwd
        .lines()
        .find_map(|l| {
            let fields: Vec<&str> = l.split(':').collect();
            (fields.first() == Some(&login.as_str())).then(|| fields.get(4).and_then(|g| g.split(',').next()).unwrap_or("").to_owned())
        })
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| login.clone());
    (login, full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_time_parses_local_date_and_time() {
        use chrono::{Local, TimeZone};
        let at = |y, mo, d, h, mi, s| Local.with_ymd_and_hms(y, mo, d, h, mi, s).earliest().unwrap().timestamp_micros();
        assert_eq!(manual_time("2026-10-27", "14:05"), Some(at(2026, 10, 27, 14, 5, 0)));
        assert_eq!(manual_time(" 2026-12-31 ", "23:59:30"), Some(at(2026, 12, 31, 23, 59, 30)));
        assert_eq!(manual_time("2026-02-30", "10:00"), None);
        assert_eq!(manual_time("2026-10-27", "25:00"), None);
        assert_eq!(manual_time("27/10/2026", "10:00"), None);
        assert_eq!(manual_time("2026-10-27", ""), None);
    }

    #[test]
    fn theme_sounds() {
        let files = [".oga", "message.oga", "dialog-warning.oga", "audio-channel-front-left.oga", "audio-test-signal.oga", "index.theme", "bell.oga"];
        let names: Vec<_> = sounds(files.map(String::from).to_vec()).into_iter().map(|(n, l)| format!("{n}={l}")).collect();
        assert_eq!(names, ["bell=Bell", "dialog-warning=Dialog warning", "message=Message"]);
    }

    #[test]
    fn meter_is_peak_decibels() {
        assert_eq!(meter([].into_iter()), 0.0);
        assert_eq!(meter([0, 30, -32].into_iter()), 0.0, "below -60 dBFS");
        assert_eq!(meter([12, i16::MIN, 5].into_iter()), 1.0);
        // -20 dBFS: two thirds up.
        assert!((meter([3277].into_iter()) - 2.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn volume_and_name() {
        assert_eq!(parse_volume("Volume: 0.89\n"), Some((89, false)));
        assert_eq!(parse_volume("Volume: 0.40 [MUTED]\n"), Some((40, true)));
        assert_eq!(parse_volume("nonsense"), None);
        assert_eq!(parse_name("  * node.description = \"Built-in Audio\"\n    node.nick = \"Speaker\"\n"), "Speaker");
        assert_eq!(parse_name("  node.description = \"HDMI\"\n"), "HDMI");
    }

    #[test]
    fn sinks_and_sources() {
        let status = "Audio\n ├─ Devices:\n │      57. Raptor Lake-P/U/H cAVS              [alsa]\n │  \n ├─ Sinks:\n │  *   44. Raptor Lake-P/U/H cAVS Speaker      [vol: 0.96]\n │      49. Raptor Lake-P/U/H cAVS HDMI / DisplayPort 3 Output (Stereo) [vol: 1.00]\n │  \n ├─ Sources:\n │      72. Raptor Lake-P/U/H cAVS Digital Microphone [vol: 1.00]\n │  *   80. USB Headset [vol: 0.50 MUTED]\n │  \n ├─ Filters:\n\nVideo\n ├─ Sources:\n │  *   81. HP 5MP Camera (V4L2)               \n";
        let choice = |id, name: &str, current| Choice { id, name: name.into(), current };
        assert_eq!(
            section(status, "Sinks:"),
            [choice(44, "Raptor Lake-P/U/H cAVS Speaker", true), choice(49, "Raptor Lake-P/U/H cAVS HDMI / DisplayPort 3 Output (Stereo)", false)]
        );
        assert_eq!(section(status, "Sources:"), [choice(72, "Raptor Lake-P/U/H cAVS Digital Microphone", false), choice(80, "USB Headset", true)]);
        assert_eq!(section("", "Sinks:"), []);
    }

    #[test]
    fn memory_and_gpu() {
        assert_eq!(installed(16_000_000.0), 16);
        assert_eq!(installed(7_900_000.0), 8);
        assert_eq!(gpu("i915"), "Intel Graphics");
        assert_eq!(gpu("virtio_gpu"), "virtio_gpu");
    }

    #[test]
    fn tzdata_zones() {
        let tzdata = "# version\nR d 1916 o - Jun 14 23s 1 S\nZ Asia/Kathmandu 5:41:16 - LMT 1920\n5:30 - +0530 1986\nL Asia/Kathmandu Asia/Katmandu\nZ America/New_York -4:56:2 - LMT 1883\n";
        assert_eq!(zones(tzdata), ["America/New_York", "Asia/Kathmandu", "Asia/Katmandu"]);
    }

    #[test]
    fn proc_fields() {
        assert_eq!(field("model name\t: AMD Ryzen 7\nflags : x", "model name"), Some("AMD Ryzen 7"));
        assert_eq!(field("MemTotal:       32000000 kB\n", "MemTotal"), Some("32000000 kB"));
        assert_eq!(field("a: b", "c"), None);
    }
}
#[test]
fn tmp_live() {
    let s = futures_lite::future::block_on(read());
    eprintln!("LIVE {:?} {:?}", s.battery, s.power_mode);
}
