//! Network state from NetworkManager (E2.3): the primary connection and, on Wi-Fi, its network name and signal bars,
//! plus the networks in range.

use std::collections::HashMap;

use smithay::reexports::calloop::channel::Sender;
use zbus::{
    Connection,
    fdo::{ManagedObjects, ObjectManagerProxy},
    proxy::CacheProperties,
    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value},
};

use super::{Update, get};

const SERVICE: &str = "org.freedesktop.NetworkManager";
const ROOT: &str = "/org/freedesktop/NetworkManager";

#[derive(Clone, Debug, PartialEq)]
pub struct Network {
    pub wifi_enabled: bool,
    pub link: Link,
    /// Networks in range, strongest first, one per name.
    pub networks: Vec<WifiNetwork>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WifiNetwork {
    pub ssid: String,
    /// 1–3, as in [`Link::Wifi`].
    pub bars: u8,
    /// Needs a password (WEP/WPA/WPA2/WPA3).
    pub secure: bool,
    /// A saved connection exists, so joining asks for nothing.
    pub known: bool,
    /// NetworkManager paths of the strongest access point and its Wi-Fi device, for joining.
    pub ap: String,
    pub device: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Link {
    None,
    /// `bars`: 1–3, the arcs of the Wi-Fi symbol.
    Wifi { ssid: String, bars: u8 },
    Ethernet,
    /// VPN, mobile broadband, …: shown by connection name.
    Other(String),
}

#[zbus::proxy(interface = "org.freedesktop.NetworkManager", default_service = "org.freedesktop.NetworkManager", default_path = "/org/freedesktop/NetworkManager")]
trait Manager {
    #[zbus(property)]
    fn wireless_enabled(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn set_wireless_enabled(&self, on: bool) -> zbus::Result<()>;
    #[zbus(property)]
    fn primary_connection(&self) -> zbus::Result<OwnedObjectPath>;
    fn activate_connection(&self, connection: &ObjectPath<'_>, device: &ObjectPath<'_>, specific_object: &ObjectPath<'_>) -> zbus::Result<OwnedObjectPath>;
    fn add_and_activate_connection(
        &self,
        connection: HashMap<&str, HashMap<&str, Value<'_>>>,
        device: &ObjectPath<'_>,
        specific_object: &ObjectPath<'_>,
    ) -> zbus::Result<(OwnedObjectPath, OwnedObjectPath)>;
}

#[zbus::proxy(interface = "org.freedesktop.NetworkManager.Settings", default_service = "org.freedesktop.NetworkManager", default_path = "/org/freedesktop/NetworkManager/Settings")]
trait Settings {
    fn list_connections(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
}

#[zbus::proxy(interface = "org.freedesktop.NetworkManager.Settings.Connection", default_service = "org.freedesktop.NetworkManager")]
trait Saved {
    fn get_settings(&self) -> zbus::Result<HashMap<String, HashMap<String, OwnedValue>>>;
}

#[zbus::proxy(interface = "org.freedesktop.NetworkManager.Device.Wireless", default_service = "org.freedesktop.NetworkManager")]
trait Wireless {
    fn request_scan(&self, options: HashMap<&str, Value<'_>>) -> zbus::Result<()>;
}

const WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const ACCESS_POINT: &str = "org.freedesktop.NetworkManager.AccessPoint";

#[zbus::proxy(interface = "org.freedesktop.NetworkManager.Connection.Active", default_service = "org.freedesktop.NetworkManager")]
trait Active {
    #[zbus(property)]
    fn id(&self) -> zbus::Result<String>;
    #[zbus(property, name = "Type")]
    fn kind(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn specific_object(&self) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(interface = "org.freedesktop.NetworkManager.AccessPoint", default_service = "org.freedesktop.NetworkManager")]
trait AccessPoint {
    #[zbus(property)]
    fn strength(&self) -> zbus::Result<u8>;
    #[zbus(property)]
    fn ssid(&self) -> zbus::Result<Vec<u8>>;
}

/// Sends the network state now and after every change, including NetworkManager starting or stopping. Signals are
/// rare (a few per minute, mostly the access point's strength), and only a visible change reaches the frame loop.
pub async fn watch(bus: &Connection, updates: &Sender<Update>) {
    super::follow(bus, SERVICE, ROOT, updates, Update::Network, read).await;
}

/// Control Center ▸ Wi-Fi.
pub async fn set_wifi(bus: &Connection, on: bool) {
    let result = match ManagerProxy::builder(bus).cache_properties(CacheProperties::No).build().await {
        Ok(manager) => manager.set_wireless_enabled(on).await,
        Err(err) => Err(err),
    };
    if let Err(err) = result {
        tracing::warn!("Wi-Fi {}: {err}", if on { "on" } else { "off" });
    }
}

/// Opening Control Center's Wi-Fi list: NetworkManager rescans (it refuses while a scan runs; that's fine).
pub async fn scan(bus: &Connection) {
    let result = async {
        let objects = ObjectManagerProxy::builder(bus).destination(SERVICE)?.path("/org/freedesktop")?.build().await?.get_managed_objects().await?;
        for (path, interfaces) in objects {
            if interfaces.keys().any(|i| i.as_str() == WIRELESS) {
                WirelessProxy::builder(bus).path(path)?.build().await?.request_scan(HashMap::new()).await?;
            }
        }
        zbus::Result::Ok(())
    };
    if let Err(err) = result.await {
        tracing::debug!("Wi-Fi scan: {err}");
    }
}

/// Control Center ▸ Wi-Fi ▸ a network: its saved connection, or a new one; `password` makes it a WPA-PSK one.
pub async fn join(bus: &Connection, ssid: &str, ap: &str, device: &str, password: Option<&str>) {
    let result = async {
        let (ap, device) = (ObjectPath::try_from(ap)?, ObjectPath::try_from(device)?);
        let manager = ManagerProxy::builder(bus).cache_properties(CacheProperties::No).build().await?;
        let saved = saved(bus).await?.into_iter().find(|(_, s)| s == ssid.as_bytes());
        match (saved, password) {
            (Some((connection, _)), None) => drop(manager.activate_connection(&connection, &device, &ap).await?),
            (_, password) => {
                // ponytail: a new password for a saved network adds a second connection; the M7 Settings app edits them.
                let mut settings = HashMap::new();
                if let Some(psk) = password {
                    settings.insert("802-11-wireless-security", HashMap::from([("key-mgmt", Value::from("wpa-psk")), ("psk", Value::from(psk))]));
                }
                manager.add_and_activate_connection(settings, &device, &ap).await?;
            }
        }
        zbus::Result::Ok(())
    };
    if let Err(err) = result.await {
        tracing::warn!("joining {ssid:?}: {err}");
    }
}

/// Saved Wi-Fi connections and their network names.
async fn saved(bus: &Connection) -> zbus::Result<Vec<(OwnedObjectPath, Vec<u8>)>> {
    let mut found = Vec::new();
    for path in SettingsProxy::new(bus).await?.list_connections().await? {
        let settings = SavedProxy::builder(bus).path(&path)?.cache_properties(CacheProperties::No).build().await?.get_settings().await?;
        if let Some(ssid) = settings.get("802-11-wireless").and_then(|w| w.get("ssid")).and_then(bytes) {
            found.push((path, ssid));
        }
    }
    Ok(found)
}

fn bytes(value: &OwnedValue) -> Option<Vec<u8>> {
    Vec::<u8>::try_from(value.try_clone().ok()?).ok()
}

/// Access points by network name, strongest first; `known` names have a saved connection.
fn networks(objects: &ManagedObjects, known: &[Vec<u8>]) -> Vec<WifiNetwork> {
    let mut found: Vec<(u8, WifiNetwork)> = Vec::new();
    for (device, interfaces) in objects {
        let Some(aps) = get(interfaces, WIRELESS, "AccessPoints").and_then(|v| Vec::<OwnedObjectPath>::try_from(v.try_clone().ok()?).ok()) else { continue };
        for ap in aps {
            let Some(i) = objects.get(&ap) else { continue };
            let Some(ssid) = get(i, ACCESS_POINT, "Ssid").and_then(bytes).filter(|s| !s.is_empty()) else { continue };
            let number = |name| get(i, ACCESS_POINT, name).and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
            let strength = get(i, ACCESS_POINT, "Strength").and_then(|v| u8::try_from(v).ok()).unwrap_or(0);
            found.push((
                strength,
                WifiNetwork {
                    known: known.contains(&ssid),
                    ssid: String::from_utf8_lossy(&ssid).into_owned(),
                    bars: bars(strength),
                    // Flags bit 0 = privacy (WEP); any WPA/RSN flag = WPA.
                    secure: number("Flags") & 1 != 0 || number("WpaFlags") != 0 || number("RsnFlags") != 0,
                    ap: ap.to_string(),
                    device: device.to_string(),
                },
            ));
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.ssid.cmp(&b.1.ssid)));
    let mut seen = std::collections::HashSet::new();
    found.into_iter().map(|(_, n)| n).filter(|n| seen.insert(n.ssid.clone())).collect()
}

async fn read(bus: &Connection) -> Option<Network> {
    let manager = ManagerProxy::builder(bus).cache_properties(CacheProperties::No).build().await.ok()?;
    let wifi_enabled = manager.wireless_enabled().await.ok()?;
    let primary = manager.primary_connection().await.ok()?;
    // A connection that vanishes while we read it is simply gone; the signal that follows corrects the rest.
    let link = if primary.as_str() == "/" { Link::None } else { link(bus, primary).await.unwrap_or(Link::None) };
    // ponytail: re-reads every saved connection on each NetworkManager signal (a few a minute); cache it if that shows up.
    let networks = async {
        let known: Vec<Vec<u8>> = saved(bus).await.unwrap_or_default().into_iter().map(|(_, s)| s).collect();
        let objects = ObjectManagerProxy::builder(bus).destination(SERVICE).ok()?.path("/org/freedesktop").ok()?.build().await.ok()?.get_managed_objects().await.ok()?;
        Some(networks(&objects, &known))
    };
    let networks = if wifi_enabled { networks.await.unwrap_or_default() } else { Vec::new() };
    Some(Network { wifi_enabled, link, networks })
}

async fn link(bus: &Connection, path: OwnedObjectPath) -> zbus::Result<Link> {
    let active = ActiveProxy::builder(bus).path(path)?.cache_properties(CacheProperties::No).build().await?;
    Ok(match active.kind().await?.as_str() {
        "802-11-wireless" => {
            let ap = AccessPointProxy::builder(bus).path(active.specific_object().await?)?.cache_properties(CacheProperties::No).build().await?;
            Link::Wifi { ssid: String::from_utf8_lossy(&ap.ssid().await?).into_owned(), bars: bars(ap.strength().await?) }
        }
        "802-3-ethernet" => Link::Ethernet,
        _ => Link::Other(active.id().await?),
    })
}

/// NetworkManager's 0–100 strength as the 1–3 arcs of the menu-bar symbol.
fn bars(strength: u8) -> u8 {
    match strength {
        0..=33 => 1,
        34..=66 => 2,
        _ => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn networks_dedupe_and_sort() {
        use zbus::{names::OwnedInterfaceName, zvariant::Value};
        let path = |p: &str| OwnedObjectPath::try_from(p).unwrap();
        let value = |v: Value| OwnedValue::try_from(v).unwrap();
        let iface = |name: &str, props: Vec<(&str, OwnedValue)>| {
            (OwnedInterfaceName::try_from(name).unwrap(), props.into_iter().map(|(k, v)| (k.to_owned(), v)).collect::<HashMap<_, _>>())
        };
        let ap = |ssid: &[u8], strength: u8, rsn: u32| {
            HashMap::from([iface(ACCESS_POINT, vec![("Ssid", value(Value::from(ssid.to_vec()))), ("Strength", value(Value::from(strength))), ("Flags", value(Value::from(0u32))), ("WpaFlags", value(Value::from(0u32))), ("RsnFlags", value(Value::from(rsn)))])])
        };
        let aps = ["/ap/1", "/ap/2", "/ap/3", "/ap/4"].map(path).to_vec();
        let objects: ManagedObjects = HashMap::from([
            (path("/dev/2"), HashMap::from([iface(WIRELESS, vec![("AccessPoints", value(Value::from(aps)))])])),
            (path("/ap/1"), ap(b"Home", 40, 392)),
            (path("/ap/2"), ap(b"Home", 90, 392)),
            (path("/ap/3"), ap(b"Cafe", 70, 0)),
            (path("/ap/4"), ap(b"", 99, 0)),
        ]);
        let found = networks(&objects, &[b"Home".to_vec()]);
        let summary: Vec<_> = found.iter().map(|n| (n.ssid.as_str(), n.bars, n.secure, n.known, n.ap.as_str())).collect();
        // Hidden networks (no name) are left out; each name keeps its strongest access point.
        assert_eq!(summary, [("Home", 3, true, true, "/ap/2"), ("Cafe", 3, false, false, "/ap/3")]);
        assert_eq!(found[0].device, "/dev/2");
    }

    #[test]
    fn strength_to_bars() {
        assert_eq!([0, 33, 34, 66, 67, 100].map(bars), [1, 1, 2, 2, 3, 3]);
    }
}
