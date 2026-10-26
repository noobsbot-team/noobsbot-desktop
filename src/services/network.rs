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
    /// Saved VPN and WireGuard connections, by name.
    pub vpns: Vec<Vpn>,
    /// Behind a captive portal (NetworkManager's connectivity check was redirected): the check's URL, which the portal
    /// sends to its sign-in page.
    pub portal: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Vpn {
    pub name: String,
    /// The saved connection.
    pub path: String,
    /// Its active connection while it is up (or coming up).
    pub active: Option<String>,
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
    Wifi {
        ssid: String,
        bars: u8,
    },
    Ethernet,
    /// VPN, mobile broadband, …: shown by connection name.
    Other(String),
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait Manager {
    #[zbus(property)]
    fn wireless_enabled(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn set_wireless_enabled(&self, on: bool) -> zbus::Result<()>;
    #[zbus(property)]
    fn primary_connection(&self) -> zbus::Result<OwnedObjectPath>;
    /// `NM_CONNECTIVITY_*`: 2 = portal.
    #[zbus(property)]
    fn connectivity(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn connectivity_check_uri(&self) -> zbus::Result<String>;
    fn activate_connection(&self, connection: &ObjectPath<'_>, device: &ObjectPath<'_>, specific_object: &ObjectPath<'_>) -> zbus::Result<OwnedObjectPath>;
    fn deactivate_connection(&self, active: &ObjectPath<'_>) -> zbus::Result<()>;
    fn add_and_activate_connection(
        &self,
        connection: HashMap<&str, HashMap<&str, Value<'_>>>,
        device: &ObjectPath<'_>,
        specific_object: &ObjectPath<'_>,
    ) -> zbus::Result<(OwnedObjectPath, OwnedObjectPath)>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Settings",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager/Settings"
)]
trait Settings {
    fn list_connections(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
}

#[zbus::proxy(interface = "org.freedesktop.NetworkManager.Settings.Connection", default_service = "org.freedesktop.NetworkManager")]
trait Saved {
    fn get_settings(&self) -> zbus::Result<HashMap<String, HashMap<String, OwnedValue>>>;
    fn update(&self, properties: HashMap<String, HashMap<String, OwnedValue>>) -> zbus::Result<()>;
}

#[zbus::proxy(interface = "org.freedesktop.NetworkManager.Device.Wireless", default_service = "org.freedesktop.NetworkManager")]
trait Wireless {
    fn request_scan(&self, options: HashMap<&str, Value<'_>>) -> zbus::Result<()>;
}

const WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const ACCESS_POINT: &str = "org.freedesktop.NetworkManager.AccessPoint";
const ACTIVE: &str = "org.freedesktop.NetworkManager.Connection.Active";

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

/// Control Center ▸ Wi-Fi ▸ a network: its saved connection (with `password` as its new one), or a new WPA-PSK one.
pub async fn join(bus: &Connection, ssid: &str, ap: &str, device: &str, password: Option<&str>) {
    let result = async {
        let (ap, device) = (ObjectPath::try_from(ap)?, ObjectPath::try_from(device)?);
        let manager = ManagerProxy::builder(bus).cache_properties(CacheProperties::No).build().await?;
        let saved = saved(bus).await?.0.into_iter().find(|(_, s)| s == ssid.as_bytes()).map(|(path, _)| path);
        match plan(saved, password) {
            Join::Activate(connection) => drop(manager.activate_connection(&connection, &device, &ap).await?),
            Join::Update(connection, password) => {
                let saved = SavedProxy::builder(bus).path(&connection)?.cache_properties(CacheProperties::No).build().await?;
                saved.update(with_password(saved.get_settings().await?, password)?).await?;
                drop(manager.activate_connection(&connection, &device, &ap).await?);
            }
            Join::Add(password) => {
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

#[derive(Debug, PartialEq)]
enum Join<'a> {
    Activate(OwnedObjectPath),
    /// A saved network with a new password: its secrets change, no second connection.
    Update(OwnedObjectPath, &'a str),
    Add(Option<&'a str>),
}

fn plan(saved: Option<OwnedObjectPath>, password: Option<&str>) -> Join<'_> {
    match (saved, password) {
        (Some(connection), None) => Join::Activate(connection),
        (Some(connection), Some(password)) => Join::Update(connection, password),
        (None, password) => Join::Add(password),
    }
}

/// A saved connection's settings with `password` as its key: the WEP key on a WEP one, else the WPA one. Stored by
/// NetworkManager (flags 0), as a new connection's would be.
fn with_password(mut settings: Settings, password: &str) -> zbus::Result<Settings> {
    let value = |v: Value| OwnedValue::try_from(v).map_err(zbus::Error::from);
    let security = settings.entry("802-11-wireless-security".into()).or_default();
    let key_mgmt = security.get("key-mgmt").and_then(|v| String::try_from(v.try_clone().ok()?).ok()).unwrap_or_else(|| "wpa-psk".into());
    if key_mgmt == "none" {
        let index = security.get("wep-tx-keyidx").and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
        security.insert(format!("wep-key{index}"), value(Value::from(password))?);
        security.insert("wep-key-type".into(), value(Value::from(wep_key_type(password)))?);
        security.insert("wep-key-flags".into(), value(Value::from(0u32))?);
    } else {
        security.insert("key-mgmt".into(), value(Value::from(key_mgmt))?);
        security.insert("psk".into(), value(Value::from(password))?);
        security.insert("psk-flags".into(), value(Value::from(0u32))?);
    }
    Ok(settings)
}

/// NetworkManager's `wep-key-type`: 1, a key (10 or 26 hex digits, or 5 or 13 ASCII characters); 2, a passphrase.
fn wep_key_type(key: &str) -> u32 {
    let hex = matches!(key.len(), 10 | 26) && key.chars().all(|c| c.is_ascii_hexdigit());
    let ascii = matches!(key.len(), 5 | 13) && key.is_ascii();
    if hex || ascii { 1 } else { 2 }
}

/// Saved Wi-Fi connections and their network names, and the VPNs (not yet marked active).
async fn saved(bus: &Connection) -> zbus::Result<(Vec<(OwnedObjectPath, Vec<u8>)>, Vec<Vpn>)> {
    let (mut found, mut vpns) = (Vec::new(), Vec::new());
    for path in SettingsProxy::new(bus).await?.list_connections().await? {
        let settings = SavedProxy::builder(bus).path(&path)?.cache_properties(CacheProperties::No).build().await?.get_settings().await?;
        if let Some(ssid) = settings.get("802-11-wireless").and_then(|w| w.get("ssid")).and_then(bytes) {
            found.push((path, ssid));
        } else if let Some(name) = vpn_name(&settings) {
            vpns.push(Vpn { name, path: path.to_string(), active: None });
        }
    }
    vpns.sort_by_key(|v| v.name.to_lowercase());
    Ok((found, vpns))
}

/// A VPN plugin's or WireGuard connection's name.
fn vpn_name(settings: &Settings) -> Option<String> {
    let connection = settings.get("connection")?;
    let text = |key| String::try_from(connection.get(key)?.try_clone().ok()?).ok();
    matches!(text("type")?.as_str(), "vpn" | "wireguard").then(|| text("id"))?
}

/// Control Center ▸ a VPN: up, or its active connection down.
pub async fn vpn(bus: &Connection, path: &str, active: Option<&str>) {
    let result = async {
        let manager = ManagerProxy::builder(bus).cache_properties(CacheProperties::No).build().await?;
        let none = ObjectPath::from_static_str_unchecked("/");
        match active {
            Some(active) => manager.deactivate_connection(&ObjectPath::try_from(active)?).await,
            None => manager.activate_connection(&ObjectPath::try_from(path)?, &none, &none).await.map(drop),
        }
    };
    if let Err(err) = result.await {
        tracing::warn!("VPN {path}: {err}");
    }
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
    let (known, mut vpns) = saved(bus).await.unwrap_or_default();
    let known: Vec<Vec<u8>> = known.into_iter().map(|(_, s)| s).collect();
    let objects = async { ObjectManagerProxy::builder(bus).destination(SERVICE)?.path("/org/freedesktop")?.build().await?.get_managed_objects().await };
    let objects = objects.await.unwrap_or_default();
    let networks = if wifi_enabled { networks(&objects, &known) } else { Vec::new() };
    for vpn in &mut vpns {
        vpn.active = active_of(&objects, &vpn.path);
    }
    let portal = match manager.connectivity().await {
        Ok(PORTAL) => Some(manager.connectivity_check_uri().await.ok().filter(|u| !u.is_empty()).unwrap_or_else(|| "http://nmcheck.gnome.org".into())),
        _ => None,
    };
    Some(Network { wifi_enabled, link, networks, vpns, portal })
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

/// `NM_CONNECTIVITY_PORTAL` (3 is LIMITED). NetworkManager reports UNKNOWN (0) while its check is off.
const PORTAL: u32 = 2;
/// The "Sign in to …" notification's id (replaced, not stacked).
pub const PORTAL_NOTICE: u32 = 0xFFFA_0000;

/// A captive portal appeared (`Some(true)`) or went (`Some(false)`) between two network states.
pub fn portal_changed(old: Option<&Network>, new: Option<&Network>) -> Option<bool> {
    let behind = |n: Option<&Network>| n.is_some_and(|n| n.portal.is_some());
    (behind(old) != behind(new)).then(|| behind(new))
}

/// "Sign in to <network>"; clicking it opens the portal (`State::invoke`).
pub fn portal_notice(network: &Network) -> super::notifications::Notification {
    let name = match &network.link {
        Link::Wifi { ssid, .. } => ssid.clone(),
        Link::Other(name) => name.clone(),
        Link::Ethernet | Link::None => "the network".into(),
    };
    let icon = "network-wireless";
    super::notifications::Notification {
        id: PORTAL_NOTICE,
        app: "Network".into(),
        icon_name: icon.into(),
        icon: super::notifications::load_icon(icon).map(std::sync::Arc::new),
        summary: format!("Sign in to {name}"),
        body: "This network needs you to sign in before it reaches the internet.".into(),
        actions: vec![("default".into(), String::new())],
        urgency: 1,
        expire: -1,
        time: chrono::Local::now().timestamp(),
        ..Default::default()
    }
}

/// The active connection of the saved connection at `path`.
fn active_of(objects: &ManagedObjects, path: &str) -> Option<String> {
    let of = |i| get(i, ACTIVE, "Connection").and_then(|v| OwnedObjectPath::try_from(v.try_clone().ok()?).ok());
    objects.iter().find(|(_, i)| of(i).is_some_and(|c| c.as_str() == path)).map(|(a, _)| a.to_string())
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
    fn secret_only_for_wifi_with_interaction() {
        let ssid = OwnedValue::try_from(Value::from(b"Home".to_vec())).unwrap();
        let connection: Settings = HashMap::from([("802-11-wireless".to_string(), HashMap::from([("ssid".to_string(), ssid)]))]);
        assert_eq!(secret_ssid(&connection, "802-11-wireless-security", ALLOW_INTERACTION).as_deref(), Some("Home"));
        assert_eq!(secret_ssid(&connection, "802-11-wireless-security", 0), None);
        assert_eq!(secret_ssid(&connection, "802-1x", ALLOW_INTERACTION), None);
        assert_eq!(secret_ssid(&Settings::new(), "802-11-wireless-security", ALLOW_INTERACTION), None);
    }

    #[test]
    fn join_updates_a_saved_network() {
        let path = OwnedObjectPath::try_from("/s/1").unwrap();
        assert_eq!(plan(Some(path.clone()), None), Join::Activate(path.clone()));
        assert_eq!(plan(Some(path.clone()), Some("hunter22")), Join::Update(path, "hunter22"));
        assert_eq!(plan(None, Some("hunter22")), Join::Add(Some("hunter22")));
        assert_eq!(plan(None, None), Join::Add(None));
    }

    #[test]
    fn new_password_into_saved_settings() {
        let value = |v: Value| OwnedValue::try_from(v).unwrap();
        let text = |s: &Settings, k: &str| String::try_from(s["802-11-wireless-security"][k].try_clone().unwrap()).unwrap();
        let number = |s: &Settings, k: &str| u32::try_from(&s["802-11-wireless-security"][k]).unwrap();
        let wpa = with_password(Settings::new(), "hunter22").unwrap();
        assert_eq!((text(&wpa, "key-mgmt"), text(&wpa, "psk"), number(&wpa, "psk-flags")), ("wpa-psk".into(), "hunter22".into(), 0));
        let sae: Settings = HashMap::from([("802-11-wireless-security".into(), HashMap::from([("key-mgmt".into(), value(Value::from("sae")))]))]);
        assert_eq!(text(&with_password(sae, "pw").unwrap(), "key-mgmt"), "sae");
        let wep: Settings = HashMap::from([(
            "802-11-wireless-security".into(),
            HashMap::from([("key-mgmt".into(), value(Value::from("none"))), ("wep-tx-keyidx".into(), value(Value::from(2u32)))]),
        )]);
        let wep = with_password(wep, "abcde").unwrap();
        assert_eq!((text(&wep, "wep-key2"), number(&wep, "wep-key-type")), ("abcde".into(), 1));
        assert!(!wep["802-11-wireless-security"].contains_key("psk"));
    }

    #[test]
    fn wep_keys_and_passphrases() {
        assert_eq!(["abcde", "0123456789", "0123456789abcdef0123456789", "13 characters"].map(wep_key_type), [1; 4]);
        // Wrong lengths, and 10 characters that aren't hex, are passphrases.
        assert_eq!(["abcd", "0123456789x", "zzzzzzzzzz", "ábcde"].map(wep_key_type), [2; 4]);
    }

    #[test]
    fn networks_dedupe_and_sort() {
        use zbus::{names::OwnedInterfaceName, zvariant::Value};
        let path = |p: &str| OwnedObjectPath::try_from(p).unwrap();
        let value = |v: Value| OwnedValue::try_from(v).unwrap();
        let iface = |name: &str, props: Vec<(&str, OwnedValue)>| {
            (OwnedInterfaceName::try_from(name).unwrap(), props.into_iter().map(|(k, v)| (k.to_owned(), v)).collect::<HashMap<_, _>>())
        };
        let ap = |ssid: &[u8], strength: u8, rsn: u32| {
            HashMap::from([iface(
                ACCESS_POINT,
                vec![
                    ("Ssid", value(Value::from(ssid.to_vec()))),
                    ("Strength", value(Value::from(strength))),
                    ("Flags", value(Value::from(0u32))),
                    ("WpaFlags", value(Value::from(0u32))),
                    ("RsnFlags", value(Value::from(rsn))),
                ],
            )])
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
    fn vpns_and_their_active_connection() {
        use zbus::names::OwnedInterfaceName;
        let value = |v: Value| OwnedValue::try_from(v).unwrap();
        let settings = |kind: &str| -> Settings {
            HashMap::from([("connection".into(), HashMap::from([("type".into(), value(Value::from(kind))), ("id".into(), value(Value::from("Work")))]))])
        };
        assert_eq!(vpn_name(&settings("wireguard")).as_deref(), Some("Work"));
        assert_eq!(vpn_name(&settings("vpn")).as_deref(), Some("Work"));
        assert_eq!(vpn_name(&settings("802-3-ethernet")), None);
        let path = |p: &str| OwnedObjectPath::try_from(p).unwrap();
        let active =
            HashMap::from([(OwnedInterfaceName::try_from(ACTIVE).unwrap(), HashMap::from([("Connection".to_string(), value(Value::from(path("/s/7"))))]))]);
        let objects: ManagedObjects = HashMap::from([(path("/a/1"), active)]);
        assert_eq!(active_of(&objects, "/s/7").as_deref(), Some("/a/1"));
        assert_eq!(active_of(&objects, "/s/8"), None);
    }

    #[test]
    fn portal_appears_and_goes() {
        let network = |portal: Option<&str>| Network {
            wifi_enabled: true,
            link: Link::Wifi { ssid: "Cafe".into(), bars: 3 },
            networks: Vec::new(),
            vpns: Vec::new(),
            portal: portal.map(Into::into),
        };
        let (free, portal) = (network(None), network(Some("http://check.example/")));
        assert_eq!(portal_changed(Some(&free), Some(&portal)), Some(true));
        assert_eq!(portal_changed(None, Some(&portal)), Some(true));
        assert_eq!(portal_changed(Some(&portal), Some(&portal)), None);
        assert_eq!(portal_changed(Some(&portal), Some(&free)), Some(false));
        assert_eq!(portal_changed(Some(&portal), None), Some(false));
        assert_eq!(portal_changed(Some(&free), Some(&free)), None);
        let notice = portal_notice(&portal);
        assert_eq!((notice.summary.as_str(), notice.id), ("Sign in to Cafe", PORTAL_NOTICE));
    }

    #[test]
    fn strength_to_bars() {
        assert_eq!([0, 33, 34, 66, 67, 100].map(bars), [1, 1, 2, 2, 3, 3]);
    }
}

type Settings = HashMap<String, HashMap<String, OwnedValue>>;

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.NetworkManager.SecretAgent")]
enum SecretError {
    #[zbus(error)]
    ZBus(zbus::Error),
    NoSecrets(String),
    UserCanceled(String),
}

/// Our `org.freedesktop.NetworkManager.SecretAgent` (plan.md E2.3): NetworkManager asks it for a Wi-Fi password it
/// doesn't have, e.g. a saved network whose password changed; the password alert answers.
struct SecretAgent {
    updates: Sender<Update>,
    answer: super::Answer<Option<String>>,
}

/// NetworkManager's `NM_SECRET_AGENT_GET_SECRETS_FLAG_ALLOW_INTERACTION`.
const ALLOW_INTERACTION: u32 = 0x1;

/// The network a `GetSecrets` call may ask the user about: a Wi-Fi security setting, with interaction allowed.
fn secret_ssid(connection: &Settings, setting: &str, flags: u32) -> Option<String> {
    let ssid = connection.get("802-11-wireless").and_then(|w| w.get("ssid")).and_then(bytes)?;
    (setting == "802-11-wireless-security" && flags & ALLOW_INTERACTION != 0).then(|| String::from_utf8_lossy(&ssid).into())
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.SecretAgent")]
impl SecretAgent {
    // ponytail: WPA/WPA3 personal (`psk`) only; WEP keys and enterprise (802.1X) logins get NoSecrets.
    async fn get_secrets(
        &self,
        connection: Settings,
        _path: OwnedObjectPath,
        setting: String,
        _hints: Vec<String>,
        flags: u32,
    ) -> Result<Settings, SecretError> {
        let Some(ssid) = secret_ssid(&connection, &setting, flags) else { return Err(SecretError::NoSecrets("not a Wi-Fi password".into())) };
        let rx = self.answer.wait();
        let _ = self.updates.send(Update::WifiSecret(Some(ssid)));
        match rx.recv().await {
            Ok(Some(psk)) => {
                let psk = OwnedValue::try_from(Value::from(psk)).map_err(|err| SecretError::ZBus(err.into()))?;
                Ok(HashMap::from([(setting, HashMap::from([("psk".to_string(), psk)]))]))
            }
            _ => Err(SecretError::UserCanceled("cancelled".into())),
        }
    }

    fn cancel_get_secrets(&self, _path: OwnedObjectPath, _setting: String) {
        self.answer.cancel();
        let _ = self.updates.send(Update::WifiSecret(None));
    }

    // ponytail: no keyring yet; NetworkManager keeps system-owned secrets itself, agent-owned ones aren't kept.
    fn save_secrets(&self, _connection: Settings, _path: OwnedObjectPath) {}

    fn delete_secrets(&self, _connection: Settings, _path: OwnedObjectPath) {}
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.AgentManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager/AgentManager"
)]
trait AgentManager {
    fn register(&self, identifier: &str) -> zbus::Result<()>;
}

/// Serves the secret agent at the path NetworkManager calls and registers it.
// ponytail: registered once; if NetworkManager restarts, password prompts need a desktop restart.
pub async fn serve_agent(bus: &Connection, updates: &Sender<Update>, answer: &super::Answer<Option<String>>) {
    let result = async {
        bus.object_server().at("/org/freedesktop/NetworkManager/SecretAgent", SecretAgent { updates: updates.clone(), answer: answer.clone() }).await?;
        AgentManagerProxy::new(bus).await?.register("org.noobsbot.desktop").await
    };
    if let Err(err) = result.await {
        tracing::warn!("Wi-Fi password agent off: {err}");
    }
}
