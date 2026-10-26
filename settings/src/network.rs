//! The Wi-Fi page's network part (E8.2 Network): networks in range, the saved ones, and the details of the links that
//! are up, from NetworkManager (the desktop's own paths, `src/services/network.rs`). One ObjectManager read gives the
//! devices, access points and IP configs; the saved connections take one `GetSettings` each.

use std::{collections::HashMap, process::Command};

use zbus::{
    Connection,
    fdo::{ManagedObjects, ObjectManagerProxy},
    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value},
};

use crate::system::Status;

const NM: &str = "org.freedesktop.NetworkManager";
const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const ACCESS_POINT: &str = "org.freedesktop.NetworkManager.AccessPoint";
const IP4: &str = "org.freedesktop.NetworkManager.IP4Config";
const SAVED: &str = "org.freedesktop.NetworkManager.Settings.Connection";
const EDITOR: &str = "nm-connection-editor";
/// `App::texts` key prefix of a network's password field, then its name.
pub const PASSWORD: &str = "wifi.password.";

type Settings = HashMap<String, HashMap<String, OwnedValue>>;

#[derive(Clone, Debug, Default)]
pub struct Network {
    /// In range, strongest first, one per name.
    pub networks: Vec<Ap>,
    /// Saved Wi-Fi connections, by network name.
    pub known: Vec<Known>,
    /// The Wi-Fi and Ethernet devices that are up.
    pub details: Vec<Details>,
    /// `nm-connection-editor` is installed ("Advanced…").
    pub editor: bool,
    /// The last change NetworkManager refused.
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ap {
    pub ssid: String,
    /// 0–100.
    pub strength: u8,
    /// Needs a password (WEP/WPA/WPA2/WPA3).
    pub secure: bool,
    /// The saved connection's path: joining asks for nothing.
    pub saved: Option<String>,
    pub connected: bool,
    /// NetworkManager paths of the strongest access point and its device, for joining.
    pub ap: String,
    pub device: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Known {
    pub path: String,
    pub ssid: String,
    pub auto_join: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Details {
    pub wifi: bool,
    pub mac: String,
    /// `192.168.1.20/24`.
    pub ip: String,
    pub router: String,
    pub dns: String,
}

// ponytail: no RequestScan; the list is NetworkManager's last scan (it rescans on its own every few minutes).
pub async fn read(bus: &Connection) -> Network {
    let known = known(bus).await.unwrap_or_default();
    let objects = async { ObjectManagerProxy::builder(bus).destination(NM)?.path("/org/freedesktop")?.build().await?.get_managed_objects().await };
    let objects = objects.await.unwrap_or_default();
    let editor = std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(EDITOR).is_file()));
    Network { networks: networks(&objects, &known), details: details(&objects), known, editor, error: None }
}

async fn known(bus: &Connection) -> zbus::Result<Vec<Known>> {
    let settings = "/org/freedesktop/NetworkManager/Settings";
    let paths: Vec<OwnedObjectPath> =
        bus.call_method(Some(NM), settings, Some("org.freedesktop.NetworkManager.Settings"), "ListConnections", &()).await?.body().deserialize()?;
    let mut found = Vec::new();
    for path in paths {
        let settings: Settings = bus.call_method(Some(NM), &path, Some(SAVED), "GetSettings", &()).await?.body().deserialize()?;
        found.extend(saved(path.to_string(), &settings));
    }
    found.sort_by_key(|k| k.ssid.to_lowercase());
    Ok(found)
}

/// A saved connection, if it is a Wi-Fi one.
fn saved(path: String, settings: &Settings) -> Option<Known> {
    let connection = settings.get("connection")?;
    let text = |v: &OwnedValue| String::try_from(v.try_clone().ok()?).ok();
    (connection.get("type").and_then(text)? == "802-11-wireless").then_some(())?;
    let ssid = String::from_utf8_lossy(&settings.get("802-11-wireless")?.get("ssid").and_then(bytes)?).into_owned();
    let auto_join = connection.get("autoconnect").and_then(|v| bool::try_from(v).ok()).unwrap_or(true);
    Some(Known { path, ssid, auto_join })
}

fn bytes(value: &OwnedValue) -> Option<Vec<u8>> {
    Vec::<u8>::try_from(value.try_clone().ok()?).ok()
}

fn get<'a>(objects: &'a ManagedObjects, path: &OwnedObjectPath, interface: &str, name: &str) -> Option<&'a OwnedValue> {
    objects.get(path)?.iter().find(|(i, _)| i.as_str() == interface)?.1.get(name)
}

fn number(value: Option<&OwnedValue>) -> u32 {
    value.and_then(|v| u32::try_from(v).ok()).unwrap_or(0)
}

fn path(value: Option<&OwnedValue>) -> Option<OwnedObjectPath> {
    OwnedObjectPath::try_from(value?.try_clone().ok()?).ok().filter(|p| p.as_str() != "/")
}

/// Access points by network name, strongest first (the desktop's `network::networks`).
fn networks(objects: &ManagedObjects, known: &[Known]) -> Vec<Ap> {
    let ssid = |ap: &OwnedObjectPath| get(objects, ap, ACCESS_POINT, "Ssid").and_then(bytes).filter(|s| !s.is_empty());
    let current: Vec<Vec<u8>> = objects.keys().filter_map(|d| path(get(objects, d, WIRELESS, "ActiveAccessPoint"))).filter_map(|ap| ssid(&ap)).collect();
    let mut found: Vec<Ap> = Vec::new();
    for device in objects.keys() {
        let Some(aps) = get(objects, device, WIRELESS, "AccessPoints").and_then(|v| Vec::<OwnedObjectPath>::try_from(v.try_clone().ok()?).ok()) else {
            continue;
        };
        for ap in aps {
            let Some(name) = ssid(&ap) else { continue };
            let flag = |n| number(get(objects, &ap, ACCESS_POINT, n));
            let ssid = String::from_utf8_lossy(&name).into_owned();
            found.push(Ap {
                strength: get(objects, &ap, ACCESS_POINT, "Strength").and_then(|v| u8::try_from(v).ok()).unwrap_or(0),
                // Flags bit 0 = privacy (WEP); any WPA/RSN flag = WPA.
                secure: flag("Flags") & 1 != 0 || flag("WpaFlags") != 0 || flag("RsnFlags") != 0,
                saved: known.iter().find(|k| k.ssid == ssid).map(|k| k.path.clone()),
                connected: current.contains(&name),
                ssid,
                ap: ap.to_string(),
                device: device.to_string(),
            });
        }
    }
    found.sort_by(|a, b| b.strength.cmp(&a.strength).then_with(|| a.ssid.cmp(&b.ssid)));
    let mut seen = std::collections::HashSet::new();
    found.retain(|n| seen.insert(n.ssid.clone()));
    found
}

/// Activated Wi-Fi (type 2) and Ethernet (type 1) devices: MAC and IPv4 setup.
fn details(objects: &ManagedObjects) -> Vec<Details> {
    let mut found: Vec<Details> = objects
        .keys()
        .filter(|d| number(get(objects, d, DEVICE, "State")) == 100)
        .filter_map(|d| {
            let wifi = match number(get(objects, d, DEVICE, "DeviceType")) {
                1 => false,
                2 => true,
                _ => return None,
            };
            let text = |v: Option<&OwnedValue>| v.and_then(|v| String::try_from(v.try_clone().ok()?).ok()).unwrap_or_default();
            let config = path(get(objects, d, DEVICE, "Ip4Config"));
            let config = |name| config.as_ref().and_then(|c| get(objects, c, IP4, name));
            let ip = dicts(config("AddressData")).first().map(|a| format!("{}/{}", text(a.get("address")), number(a.get("prefix")))).unwrap_or_default();
            let dns: Vec<String> = dicts(config("NameserverData")).iter().map(|a| text(a.get("address"))).collect();
            Some(Details { wifi, mac: text(get(objects, d, DEVICE, "HwAddress")), ip, router: text(config("Gateway")), dns: dns.join(", ") })
        })
        .collect();
    found.sort_by_key(|d| !d.wifi);
    found
}

/// `aa{sv}`: `AddressData`, `NameserverData`.
fn dicts(value: Option<&OwnedValue>) -> Vec<HashMap<String, OwnedValue>> {
    value.and_then(|v| Vec::<HashMap<String, OwnedValue>>::try_from(v.try_clone().ok()?).ok()).unwrap_or_default()
}

/// A network in range: its saved connection, or a new one; `password` makes it a WPA-PSK one.
pub async fn join(bus: Connection, ap: Ap, password: Option<String>) -> zbus::Result<()> {
    let (point, device) = (ObjectPath::try_from(ap.ap.as_str())?, ObjectPath::try_from(ap.device.as_str())?);
    let manager = Some("org.freedesktop.NetworkManager");
    match (&ap.saved, password) {
        (Some(saved), None) => {
            bus.call_method(
                Some(NM),
                "/org/freedesktop/NetworkManager",
                manager,
                "ActivateConnection",
                &(ObjectPath::try_from(saved.as_str())?, device, point),
            )
            .await?;
        }
        (_, password) => {
            // ponytail: a new password for a saved network adds a second connection, as in the desktop's join.
            let mut settings: HashMap<&str, HashMap<&str, Value>> = HashMap::new();
            if let Some(psk) = password {
                settings.insert("802-11-wireless-security", HashMap::from([("key-mgmt", Value::from("wpa-psk")), ("psk", Value::from(psk))]));
            }
            bus.call_method(Some(NM), "/org/freedesktop/NetworkManager", manager, "AddAndActivateConnection", &(settings, device, point)).await?;
        }
    }
    Ok(())
}

/// Known Networks ▸ Auto-Join: `connection.autoconnect`. NetworkManager keeps the stored secrets when `Update` has none.
pub async fn auto_join(bus: Connection, path: String, on: bool) -> zbus::Result<()> {
    let mut settings: Settings = bus.call_method(Some(NM), path.as_str(), Some(SAVED), "GetSettings", &()).await?.body().deserialize()?;
    settings.entry("connection".into()).or_default().insert("autoconnect".into(), OwnedValue::try_from(Value::from(on))?);
    bus.call_method(Some(NM), path.as_str(), Some(SAVED), "Update", &(settings,)).await?;
    Ok(())
}

/// Known Networks ▸ Forget.
// ponytail: no "Forget this network?" confirmation; add one if it's hit by accident.
pub async fn forget(bus: Connection, path: String) -> zbus::Result<()> {
    bus.call_method(Some(NM), path.as_str(), Some(SAVED), "Delete", &()).await?;
    Ok(())
}

/// Runs a change on the system bus, then the new status, with NetworkManager's refusal (if any) on the page.
pub async fn change<F: Future<Output = zbus::Result<()>>>(change: impl FnOnce(Connection) -> F) -> Status {
    let result = match crate::system::connect("DBUS_SYSTEM_BUS_ADDRESS", "/run/dbus/system_bus_socket".into()).await {
        Ok(bus) => change(bus).await,
        Err(err) => Err(err),
    };
    let mut status = crate::system::read().await;
    if let (Err(err), Some(wifi)) = (result, &mut status.wifi) {
        wifi.network.error = Some(err.to_string());
    }
    status
}

/// "Advanced…": `nm-connection-editor`, reaped on a thread so it leaves no zombie.
pub fn advanced() {
    if let Ok(mut child) = Command::new(EDITOR).spawn() {
        std::thread::spawn(move || child.wait());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::names::OwnedInterfaceName;

    fn value(v: Value) -> OwnedValue {
        OwnedValue::try_from(v).unwrap()
    }

    fn object(interface: &str, props: Vec<(&str, OwnedValue)>) -> HashMap<OwnedInterfaceName, HashMap<String, OwnedValue>> {
        HashMap::from([(OwnedInterfaceName::try_from(interface).unwrap(), props.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())])
    }

    fn path(p: &str) -> OwnedObjectPath {
        OwnedObjectPath::try_from(p).unwrap()
    }

    #[test]
    fn saved_wifi_only() {
        let settings = |kind: &str, auto: Option<bool>| -> Settings {
            let mut connection = HashMap::from([("type".to_string(), value(Value::from(kind)))]);
            if let Some(on) = auto {
                connection.insert("autoconnect".into(), value(Value::from(on)));
            }
            HashMap::from([
                ("connection".to_string(), connection),
                ("802-11-wireless".to_string(), HashMap::from([("ssid".to_string(), value(Value::from(b"Home".to_vec())))])),
            ])
        };
        let known = saved("/s/1".into(), &settings("802-11-wireless", None)).unwrap();
        assert_eq!((known.ssid.as_str(), known.auto_join), ("Home", true));
        assert!(!saved("/s/1".into(), &settings("802-11-wireless", Some(false))).unwrap().auto_join);
        assert_eq!(saved("/s/2".into(), &settings("802-3-ethernet", None)), None);
    }

    #[test]
    fn networks_and_details() {
        let ap = |ssid: &[u8], strength: u8, rsn: u32| {
            object(
                ACCESS_POINT,
                vec![("Ssid", value(Value::from(ssid.to_vec()))), ("Strength", value(Value::from(strength))), ("RsnFlags", value(Value::from(rsn)))],
            )
        };
        let address = |a: &'static str, prefix: u32| HashMap::from([("address", Value::from(a)), ("prefix", Value::from(prefix))]);
        let mut wifi = object(
            WIRELESS,
            vec![
                ("AccessPoints", value(Value::from(["/ap/1", "/ap/2", "/ap/3"].map(path).to_vec()))),
                ("ActiveAccessPoint", value(Value::from(path("/ap/1")))),
            ],
        );
        wifi.extend(object(
            DEVICE,
            vec![
                ("State", value(Value::from(100u32))),
                ("DeviceType", value(Value::from(2u32))),
                ("HwAddress", value(Value::from("AA:BB:CC:DD:EE:FF"))),
                ("Ip4Config", value(Value::from(path("/ip4/1")))),
            ],
        ));
        let ethernet = object(DEVICE, vec![("State", value(Value::from(30u32))), ("DeviceType", value(Value::from(1u32)))]);
        let objects: ManagedObjects = HashMap::from([
            (path("/dev/1"), wifi),
            (path("/dev/2"), ethernet),
            (path("/ap/1"), ap(b"Home", 40, 392)),
            (path("/ap/2"), ap(b"Cafe", 70, 0)),
            (path("/ap/3"), ap(b"Home", 90, 392)),
            (
                path("/ip4/1"),
                object(
                    IP4,
                    vec![
                        ("AddressData", value(Value::from(vec![address("192.168.1.20", 24)]))),
                        ("Gateway", value(Value::from("192.168.1.1"))),
                        ("NameserverData", value(Value::from(vec![address("1.1.1.1", 0), address("9.9.9.9", 0)]))),
                    ],
                ),
            ),
        ]);
        let known = [Known { path: "/s/1".into(), ssid: "Home".into(), auto_join: true }];
        let found = networks(&objects, &known);
        let summary: Vec<_> = found.iter().map(|n| (n.ssid.as_str(), n.strength, n.secure, n.saved.as_deref(), n.connected, n.ap.as_str())).collect();
        assert_eq!(summary, [("Home", 90, true, Some("/s/1"), true, "/ap/3"), ("Cafe", 70, false, None, false, "/ap/2")]);
        // The Ethernet device isn't up, so only Wi-Fi has details.
        let details = details(&objects);
        let expected =
            Details { wifi: true, mac: "AA:BB:CC:DD:EE:FF".into(), ip: "192.168.1.20/24".into(), router: "192.168.1.1".into(), dns: "1.1.1.1, 9.9.9.9".into() };
        assert_eq!(details, [expected]);
    }
}
