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
    pub input: Option<Device>,
    /// `None`: no battery (a desktop), or UPower isn't running.
    pub battery: Option<Battery>,
    /// `None`: power-profiles-daemon isn't running.
    pub power_mode: Option<PowerMode>,
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

#[derive(Clone, Debug)]
pub struct Wifi {
    pub enabled: bool,
    /// The primary connection's name.
    pub connection: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Bluetooth {
    pub powered: bool,
    /// Paired devices: name, connected.
    pub devices: Vec<(String, bool)>,
}

#[derive(Clone, Debug)]
pub struct Device {
    pub name: String,
    pub volume: u8,
    pub muted: bool,
}

const NM: &str = "org.freedesktop.NetworkManager";

pub async fn read() -> Status {
    let system = connect("DBUS_SYSTEM_BUS_ADDRESS", "/run/dbus/system_bus_socket".into()).await.ok();
    let (wifi, bluetooth, battery, power_mode) = match &system {
        Some(bus) => (wifi(bus).await, bluetooth(bus).await, battery(bus).await, power_mode(bus).await),
        None => (None, None, None, None),
    };
    Status { wifi, bluetooth, output: device(OUTPUT), input: device(INPUT), battery, power_mode }
}

/// The desktop's connect (`src/services/mod.rs`): on this thread, not zbus's pool, whose idle thread wakes every
/// 500 ms.
async fn connect(variable: &str, fallback: String) -> zbus::Result<Connection> {
    let address = std::env::var(variable).unwrap_or_default();
    let path = address.strip_prefix("unix:path=").map_or(fallback.as_str(), |rest| rest.split(',').next().unwrap_or(rest));
    let socket = UnixStream::connect(path)?;
    zbus::connection::Builder::async_io_unix_stream(socket).method_timeout(Duration::from_secs(2)).build().await
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
    Some(Wifi { enabled, connection })
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
    for interfaces in objects.values() {
        if let Some(on) = field(interfaces, "org.bluez.Adapter1", "Powered") {
            powered = Some(powered.unwrap_or(false) | bool::try_from(on).unwrap_or(false));
        }
        let paired = field(interfaces, "org.bluez.Device1", "Paired").and_then(|v| bool::try_from(v).ok()).unwrap_or(false);
        if paired {
            let name = field(interfaces, "org.bluez.Device1", "Alias").and_then(|v| String::try_from(v).ok()).unwrap_or_default();
            let connected = field(interfaces, "org.bluez.Device1", "Connected").and_then(|v| bool::try_from(v).ok()).unwrap_or(false);
            devices.push((name, connected));
        }
    }
    devices.sort();
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
    if let Ok(bus) = connect("DBUS_SYSTEM_BUS_ADDRESS", "/run/dbus/system_bus_socket".into()).await {
        let _ = set_property(&bus, PROFILES, PROFILES_PATH, PROFILES, "ActiveProfile", profile.as_str().into()).await;
    }
    read().await
}

/// Wi-Fi on or off, then the new status.
pub async fn set_wifi(on: bool) -> Status {
    if let Ok(bus) = connect("DBUS_SYSTEM_BUS_ADDRESS", "/run/dbus/system_bus_socket".into()).await {
        let _ = set_property(&bus, NM, "/org/freedesktop/NetworkManager", NM, "WirelessEnabled", on.into()).await;
    }
    read().await
}

/// Every adapter on or off, then the new status.
pub async fn set_bluetooth(on: bool) -> Status {
    if let Ok(bus) = connect("DBUS_SYSTEM_BUS_ADDRESS", "/run/dbus/system_bus_socket".into()).await
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
    let _ = wpctl(&["set-volume", if output { OUTPUT } else { INPUT }, &format!("{}%", percent.min(100))]);
    read().await
}

pub async fn set_muted(output: bool, muted: bool) -> Status {
    let _ = wpctl(&["set-mute", if output { OUTPUT } else { INPUT }, if muted { "1" } else { "0" }]);
    read().await
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
    fn volume_and_name() {
        assert_eq!(parse_volume("Volume: 0.89\n"), Some((89, false)));
        assert_eq!(parse_volume("Volume: 0.40 [MUTED]\n"), Some((40, true)));
        assert_eq!(parse_volume("nonsense"), None);
        assert_eq!(parse_name("  * node.description = \"Built-in Audio\"\n    node.nick = \"Speaker\"\n"), "Speaker");
        assert_eq!(parse_name("  node.description = \"HDMI\"\n"), "HDMI");
    }

    #[test]
    fn memory_and_gpu() {
        assert_eq!(installed(16_000_000.0), 16);
        assert_eq!(installed(7_900_000.0), 8);
        assert_eq!(gpu("i915"), "Intel Graphics");
        assert_eq!(gpu("virtio_gpu"), "virtio_gpu");
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
