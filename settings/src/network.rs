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
const ACTIVE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const EDITOR: &str = "nm-connection-editor";
/// `App::texts` key prefix of a network's password field, then its name.
pub const PASSWORD: &str = "wifi.password.";
/// `App::texts` key prefix of the Details sheet's fields (`path`, `method`, `address`, …); open while `path` is there.
pub const DETAILS: &str = "wifi.details.";
/// `App::texts` key prefix of the "Other…" form's fields (`ssid`, `security`, …); open while its `ssid` is there.
pub const OTHER: &str = "wifi.other.";

type Settings = HashMap<String, HashMap<String, OwnedValue>>;
/// A connection's settings as sent to NetworkManager (`a{sa{sv}}`).
type Dict<'a> = HashMap<&'static str, HashMap<&'static str, Value<'a>>>;

#[derive(Clone, Debug, Default)]
pub struct Network {
    /// In range, strongest first, one per name.
    pub networks: Vec<Ap>,
    /// Saved Wi-Fi and wired connections, by name.
    pub known: Vec<Known>,
    /// Saved VPN and WireGuard connections.
    pub vpns: Vec<Vpn>,
    /// The Wi-Fi and Ethernet devices that are up.
    pub details: Vec<Details>,
    /// The Wi-Fi device ("Other…" joins through it).
    pub wifi_device: Option<String>,
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
    /// The network name; a wired connection's own name.
    pub ssid: String,
    pub wifi: bool,
    pub auto_join: bool,
    pub ip4: Ip4,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Vpn {
    pub path: String,
    pub name: String,
    /// Its active connection's path while it is up (or coming up).
    pub active: Option<String>,
}

/// A saved connection's IPv4 setup, as typed in its Details sheet.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ip4 {
    /// Manual addresses; else DHCP.
    pub manual: bool,
    pub address: String,
    pub prefix: String,
    pub gateway: String,
    /// Comma separated.
    pub dns: String,
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
    let (known, mut vpns) = known(bus).await.unwrap_or_default();
    let objects = async { ObjectManagerProxy::builder(bus).destination(NM)?.path("/org/freedesktop")?.build().await?.get_managed_objects().await };
    let objects = objects.await.unwrap_or_default();
    for vpn in &mut vpns {
        vpn.active = objects.keys().find(|a| path(get(&objects, a, ACTIVE, "Connection")).is_some_and(|c| c.as_str() == vpn.path)).map(|a| a.to_string());
    }
    let editor = std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(EDITOR).is_file()));
    let wifi_device = objects.iter().find(|(_, i)| i.keys().any(|i| i.as_str() == WIRELESS)).map(|(p, _)| p.to_string());
    Network { networks: networks(&objects, &known), details: details(&objects), known, vpns, wifi_device, editor, error: None }
}

/// Saved Wi-Fi and wired connections, and the VPNs.
async fn known(bus: &Connection) -> zbus::Result<(Vec<Known>, Vec<Vpn>)> {
    let settings = "/org/freedesktop/NetworkManager/Settings";
    let paths: Vec<OwnedObjectPath> =
        bus.call_method(Some(NM), settings, Some("org.freedesktop.NetworkManager.Settings"), "ListConnections", &()).await?.body().deserialize()?;
    let (mut found, mut vpns) = (Vec::new(), Vec::new());
    for path in paths {
        let settings: Settings = bus.call_method(Some(NM), &path, Some(SAVED), "GetSettings", &()).await?.body().deserialize()?;
        found.extend(saved(path.to_string(), &settings));
        vpns.extend(vpn(path.to_string(), &settings));
    }
    found.sort_by_key(|k| k.ssid.to_lowercase());
    vpns.sort_by_key(|v| v.name.to_lowercase());
    Ok((found, vpns))
}

/// A saved connection, if it is a Wi-Fi or wired one.
fn saved(path: String, settings: &Settings) -> Option<Known> {
    let connection = settings.get("connection")?;
    let wifi = match connection.get("type").and_then(text)?.as_str() {
        "802-11-wireless" => true,
        "802-3-ethernet" => false,
        _ => return None,
    };
    let ssid = match wifi {
        true => String::from_utf8_lossy(&settings.get("802-11-wireless")?.get("ssid").and_then(bytes)?).into_owned(),
        false => connection.get("id").and_then(text)?,
    };
    let auto_join = connection.get("autoconnect").and_then(|v| bool::try_from(v).ok()).unwrap_or(true);
    Some(Known { path, ssid, wifi, auto_join, ip4: settings.get("ipv4").map(ip4).unwrap_or_default() })
}

/// A saved connection, if it is a VPN (a NetworkManager VPN plugin's, or WireGuard).
fn vpn(path: String, settings: &Settings) -> Option<Vpn> {
    let connection = settings.get("connection")?;
    matches!(connection.get("type").and_then(text)?.as_str(), "vpn" | "wireguard").then_some(())?;
    Some(Vpn { path, name: connection.get("id").and_then(text)?, active: None })
}

/// VPN ▸ the switch: brings the connection up, or its active connection down.
pub async fn vpn_switch(bus: Connection, vpn: Vpn) -> zbus::Result<()> {
    let manager = "/org/freedesktop/NetworkManager";
    match &vpn.active {
        Some(active) => drop(bus.call_method(Some(NM), manager, Some(NM), "DeactivateConnection", &(ObjectPath::try_from(active.as_str())?,)).await?),
        None => {
            let none = ObjectPath::from_static_str_unchecked("/");
            drop(bus.call_method(Some(NM), manager, Some(NM), "ActivateConnection", &(ObjectPath::try_from(vpn.path.as_str())?, &none, &none)).await?)
        }
    }
    Ok(())
}

fn text(v: &OwnedValue) -> Option<String> {
    String::try_from(v.try_clone().ok()?).ok()
}

/// The `ipv4` setting: method, the first address, gateway and DNS (`dns` is `au` in network byte order).
fn ip4(setting: &HashMap<String, OwnedValue>) -> Ip4 {
    let address = dicts(setting.get("address-data")).into_iter().next().unwrap_or_default();
    let dns = setting.get("dns").and_then(|v| Vec::<u32>::try_from(v.try_clone().ok()?).ok()).unwrap_or_default();
    let dns: Vec<String> = dns.into_iter().map(|n| std::net::Ipv4Addr::from(n.to_ne_bytes()).to_string()).collect();
    Ip4 {
        manual: setting.get("method").and_then(text).as_deref() == Some("manual"),
        address: address.get("address").and_then(text).unwrap_or_default(),
        prefix: address.get("prefix").map(|p| number(Some(p)).to_string()).unwrap_or_default(),
        gateway: setting.get("gateway").and_then(text).unwrap_or_default(),
        dns: dns.join(", "),
    }
}

/// Writes `ip4` into a saved connection's settings; the error says which field isn't an address.
fn set_ip4(settings: &mut Settings, ip4: &Ip4) -> Result<(), String> {
    use std::net::Ipv4Addr;
    let parse = |what: &str, s: &str| s.trim().parse::<Ipv4Addr>().map_err(|_| format!("“{}” isn't an IPv4 {what}", s.trim()));
    let owned = |v: Value| OwnedValue::try_from(v).map_err(|e| e.to_string());
    let dns = ip4.dns.split([',', ' ']).filter(|s| !s.is_empty()).map(|s| parse("DNS server", s)).collect::<Result<Vec<_>, _>>()?;
    let mut new = HashMap::from([("method", owned(Value::from(if ip4.manual { "manual" } else { "auto" }))?)]);
    if !dns.is_empty() {
        new.insert("dns", owned(Value::from(dns.iter().map(|a| u32::from_ne_bytes(a.octets())).collect::<Vec<u32>>()))?);
    }
    if ip4.manual {
        let address = parse("address", &ip4.address)?;
        let prefix = ip4
            .prefix
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|p| (1..=32).contains(p))
            .ok_or_else(|| format!("“{}” isn't a prefix length (1–32)", ip4.prefix.trim()))?;
        new.insert("address-data", owned(Value::from(vec![HashMap::from([("address", Value::from(address.to_string())), ("prefix", Value::from(prefix))])]))?);
        if !ip4.gateway.trim().is_empty() {
            new.insert("gateway", owned(Value::from(parse("router", &ip4.gateway)?.to_string()))?);
        }
    }
    let setting = settings.entry("ipv4".into()).or_default();
    // The older `addresses` and newer `dns-data` would win over (or clash with) what is written here.
    for key in ["method", "addresses", "address-data", "gateway", "dns", "dns-data"] {
        setting.remove(key);
    }
    setting.extend(new.into_iter().map(|(k, v)| (k.to_owned(), v)));
    Ok(())
}

/// Details ▸ Update: the connection's IPv4 setup; it applies the next time the connection comes up.
pub async fn update_ip4(bus: Connection, path: String, ip4: Ip4) -> zbus::Result<()> {
    let mut settings: Settings = bus.call_method(Some(NM), path.as_str(), Some(SAVED), "GetSettings", &()).await?.body().deserialize()?;
    set_ip4(&mut settings, &ip4).map_err(zbus::Error::Failure)?;
    bus.call_method(Some(NM), path.as_str(), Some(SAVED), "Update", &(settings,)).await?;
    Ok(())
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
                saved: known.iter().find(|k| k.wifi && k.ssid == ssid).map(|k| k.path.clone()),
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

/// "Other…": a network that doesn't announce its name. `security` is "none", "psk" (WPA2/WPA3 Personal) or "eap"
/// (WPA2 Enterprise, with `eap` "peap" or "ttls", `inner` "mschapv2" or "pap", and an optional `ca` file).
#[derive(Default)]
pub struct Hidden {
    pub ssid: String,
    pub security: String,
    pub password: String,
    pub eap: String,
    pub inner: String,
    pub identity: String,
    pub ca: String,
}

impl Drop for Hidden {
    fn drop(&mut self) {
        wipe(&mut self.password);
    }
}

/// Zeroes a secret before its memory goes back to the allocator.
pub fn wipe(secret: &mut String) {
    let mut bytes = std::mem::take(secret).into_bytes();
    // SAFETY: plain byte writes inside the vector; volatile so they aren't optimised away before the free.
    bytes.iter_mut().for_each(|b| unsafe { std::ptr::write_volatile(b, 0) });
}

/// The new connection for a hidden network.
fn hidden_settings(h: &Hidden) -> Dict<'_> {
    let mut settings = Dict::from([
        ("connection", HashMap::from([("type", Value::from("802-11-wireless")), ("id", Value::from(h.ssid.as_str()))])),
        ("802-11-wireless", HashMap::from([("ssid", Value::from(h.ssid.as_bytes())), ("hidden", Value::from(true))])),
    ]);
    match h.security.as_str() {
        // ponytail: wpa-psk joins WPA2 and WPA2/WPA3 mixed networks; a WPA3-only one needs "sae" (Advanced…).
        "psk" => drop(settings.insert("802-11-wireless-security", HashMap::from([("key-mgmt", "wpa-psk".into()), ("psk", h.password.as_str().into())]))),
        "eap" => {
            settings.insert("802-11-wireless-security", HashMap::from([("key-mgmt", Value::from("wpa-eap"))]));
            let mut x = HashMap::from([
                ("eap", Value::from(vec![h.eap.as_str()])),
                ("phase2-auth", h.inner.as_str().into()),
                ("identity", h.identity.as_str().into()),
                ("password", h.password.as_str().into()),
            ]);
            if !h.ca.trim().is_empty() {
                // A path is given as a NUL-terminated `file://` URI in bytes.
                x.insert("ca-cert", Value::from(format!("file://{}\0", h.ca.trim()).into_bytes()));
            }
            settings.insert("802-1x", x);
        }
        _ => {}
    }
    settings
}

/// "Other…" ▸ Join: adds the hidden network's connection and joins it through `device`.
pub async fn join_hidden(bus: Connection, device: String, hidden: Hidden) -> zbus::Result<()> {
    let call = (hidden_settings(&hidden), ObjectPath::try_from(device.as_str())?, ObjectPath::from_static_str_unchecked("/"));
    bus.call_method(Some(NM), "/org/freedesktop/NetworkManager", Some(NM), "AddAndActivateConnection", &call).await?;
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
    let result = match crate::system::system(10).await {
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
    fn hidden_network_settings() {
        let text = |v: &Value| String::try_from(v.try_clone().unwrap()).unwrap();
        let mut h = Hidden::default();
        (h.ssid, h.security) = ("Lab".into(), "none".into());
        let open = hidden_settings(&h);
        assert_eq!(open["802-11-wireless"]["hidden"], Value::from(true));
        assert_eq!(Vec::<u8>::try_from(open["802-11-wireless"]["ssid"].try_clone().unwrap()).unwrap(), b"Lab");
        assert_eq!(text(&open["connection"]["type"]), "802-11-wireless");
        assert!(!open.contains_key("802-11-wireless-security"));
        drop(open);

        h.security = "psk".into();
        h.password = "hunter22".into();
        let psk = hidden_settings(&h);
        assert_eq!((text(&psk["802-11-wireless-security"]["key-mgmt"]), text(&psk["802-11-wireless-security"]["psk"])), ("wpa-psk".into(), "hunter22".into()));
        drop(psk);

        (h.ssid, h.security, h.password, h.eap, h.inner, h.identity, h.ca) =
            ("Corp".into(), "eap".into(), "pw".into(), "peap".into(), "mschapv2".into(), "me".into(), "/etc/ca.pem".into());
        let eap = hidden_settings(&h);
        assert_eq!(text(&eap["802-11-wireless-security"]["key-mgmt"]), "wpa-eap");
        let x = &eap["802-1x"];
        assert_eq!(Vec::<String>::try_from(x["eap"].try_clone().unwrap()).unwrap(), ["peap"]);
        assert_eq!([text(&x["phase2-auth"]), text(&x["identity"]), text(&x["password"])], ["mschapv2", "me", "pw"]);
        assert_eq!(Vec::<u8>::try_from(x["ca-cert"].try_clone().unwrap()).unwrap(), b"file:///etc/ca.pem\0");
        drop(eap);
        h.ca.clear();
        assert!(!hidden_settings(&h)["802-1x"].contains_key("ca-cert"));
    }

    #[test]
    fn manual_ip4_round_trip() {
        let mut settings: Settings = HashMap::from([(
            "ipv4".to_string(),
            HashMap::from([("method".to_string(), value(Value::from("auto"))), ("addresses".to_string(), value(Value::from(Vec::<Vec<u32>>::new())))]),
        )]);
        let typed = Ip4 { manual: true, address: "192.168.1.20".into(), prefix: "24".into(), gateway: "192.168.1.1".into(), dns: "1.1.1.1, 9.9.9.9".into() };
        set_ip4(&mut settings, &typed).unwrap();
        assert!(!settings["ipv4"].contains_key("addresses"));
        // `dns` is in network byte order: 1.1.1.1's bytes as they are.
        assert_eq!(Vec::<u32>::try_from(settings["ipv4"]["dns"].try_clone().unwrap()).unwrap()[0], u32::from_ne_bytes([1, 1, 1, 1]));
        assert_eq!(ip4(&settings["ipv4"]), typed);

        let auto = Ip4 { dns: String::new(), ..typed.clone() };
        set_ip4(&mut settings, &Ip4 { manual: false, ..auto }).unwrap();
        assert_eq!(ip4(&settings["ipv4"]), Ip4::default());

        for bad in [Ip4 { prefix: "33".into(), ..typed.clone() }, Ip4 { gateway: "router".into(), ..typed.clone() }, Ip4 { dns: "1.1.1".into(), ..typed }] {
            assert!(set_ip4(&mut settings, &bad).is_err());
        }
    }

    #[test]
    fn wipe_empties() {
        let mut secret = String::from("hunter22");
        wipe(&mut secret);
        assert!(secret.is_empty());
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
        assert_eq!((known.ssid.as_str(), known.wifi, known.auto_join), ("Home", true, true));
        assert!(!saved("/s/1".into(), &settings("802-11-wireless", Some(false))).unwrap().auto_join);
        // A wired connection needs its own name; a VPN isn't listed here.
        assert_eq!(saved("/s/2".into(), &settings("802-3-ethernet", None)), None);
        let mut wired = settings("802-3-ethernet", None);
        wired.get_mut("connection").unwrap().insert("id".into(), value(Value::from("Office")));
        assert_eq!(saved("/s/2".into(), &wired).map(|k| (k.ssid, k.wifi)), Some(("Office".into(), false)));
        assert_eq!(saved("/s/3".into(), &settings("vpn", None)), None);
        let mut wg = settings("wireguard", None);
        wg.get_mut("connection").unwrap().insert("id".into(), value(Value::from("Home VPN")));
        assert_eq!(vpn("/s/4".into(), &wg).map(|v| v.name), Some("Home VPN".into()));
        assert_eq!(vpn("/s/1".into(), &settings("802-11-wireless", None)), None);
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
        let known = [Known { path: "/s/1".into(), ssid: "Home".into(), wifi: true, auto_join: true, ip4: Ip4::default() }];
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
