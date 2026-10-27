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
/// `App::texts` key of VPN ▸ Import WireGuard's path field.
pub const WIREGUARD: &str = "wifi.wireguard";
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
    pub ip4: Ip,
    pub ip6: Ip,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Vpn {
    pub path: String,
    pub name: String,
    /// Its active connection's path while it is up (or coming up).
    pub active: Option<String>,
}

/// A saved connection's IPv4 or IPv6 setup, as typed in its Details sheet.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ip {
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

pub async fn read(bus: &Connection) -> Network {
    let (known, mut vpns) = known(bus).await.unwrap_or_default();
    let objects = objects(bus).await;
    for vpn in &mut vpns {
        vpn.active = objects.keys().find(|a| path(get(&objects, a, ACTIVE, "Connection")).is_some_and(|c| c.as_str() == vpn.path)).map(|a| a.to_string());
    }
    let editor = std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(EDITOR).is_file()));
    let wifi_device = objects.iter().find(|(_, i)| i.keys().any(|i| i.as_str() == WIRELESS)).map(|(p, _)| p.to_string());
    Network { networks: networks(&objects, &known), details: details(&objects), known, vpns, wifi_device, editor, error: None }
}

/// NetworkManager's devices, access points, IP configs and active connections.
async fn objects(bus: &Connection) -> ManagedObjects {
    let objects = async { ObjectManagerProxy::builder(bus).destination(NM)?.path("/org/freedesktop")?.build().await?.get_managed_objects().await };
    objects.await.unwrap_or_default()
}

/// While the Network page shows: every Wi-Fi device rescans now and every 10 s (the desktop's `network::scan`), and
/// `()` 4 s after each request, for a re-read. NetworkManager refuses scans too close together; the re-read then
/// shows its last one.
// ponytail: a fixed 4 s wait, not `LastScan`; a slow scan shows on the next round.
pub fn scans() -> impl iced::futures::Stream<Item = ()> {
    iced::stream::channel(1, async |tx: iced::futures::channel::mpsc::Sender<()>| {
        let mut sender = tx.clone();
        let spawned = std::thread::Builder::new().name("wifi-scan".into()).spawn(move || {
            let bus = futures_lite::future::block_on(crate::system::system(2)).ok();
            while !sender.is_closed() {
                if let Some(bus) = &bus {
                    futures_lite::future::block_on(async {
                        for (path, interfaces) in objects(bus).await {
                            if interfaces.keys().any(|i| i.as_str() == WIRELESS) {
                                let options = HashMap::<&str, Value>::new();
                                let _ = bus.call_method(Some(NM), &path, Some(WIRELESS), "RequestScan", &(options,)).await;
                            }
                        }
                    });
                }
                std::thread::sleep(std::time::Duration::from_secs(4));
                let _ = sender.try_send(());
                std::thread::sleep(std::time::Duration::from_secs(6));
            }
        });
        if let Err(err) = spawned {
            eprintln!("wifi scan: {err}");
        }
        std::future::pending::<()>().await;
        drop(tx);
    })
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
    let (ip4, ip6) = [false, true].map(|v6| settings.get(if v6 { "ipv6" } else { "ipv4" }).map(|s| ip(s, v6)).unwrap_or_default()).into();
    Some(Known { path, ssid, wifi, auto_join, ip4, ip6 })
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

/// One `[Peer]` of a WireGuard `.conf`; the preshared key is zeroed when dropped.
#[derive(Default)]
struct Peer {
    public_key: String,
    preshared_key: String,
    endpoint: String,
    allowed_ips: Vec<String>,
    keepalive: Option<u32>,
}

impl Drop for Peer {
    fn drop(&mut self) {
        wipe(&mut self.preshared_key);
    }
}

/// A WireGuard `.conf` as NetworkManager needs it; the private key is zeroed when dropped.
#[derive(Default)]
pub struct WireGuard {
    /// The connection's and interface's name.
    name: String,
    private_key: String,
    addresses: Vec<(std::net::IpAddr, u32)>,
    dns: Vec<std::net::IpAddr>,
    /// `DNS =` entries that aren't addresses.
    search: Vec<String>,
    listen_port: Option<u32>,
    mtu: Option<u32>,
    peers: Vec<Peer>,
}

impl Drop for WireGuard {
    fn drop(&mut self) {
        wipe(&mut self.private_key);
    }
}

/// A WireGuard key: 32 bytes in base64 (43 characters and `=`; the last one carries 2 zero bits).
fn is_key(key: &str) -> bool {
    let b = key.as_bytes();
    b.len() == 44 && b[43] == b'=' && b[..43].iter().all(|c| c.is_ascii_alphanumeric() || *c == b'+' || *c == b'/') && b"AEIMQUYcgkosw048".contains(&b[42])
}

/// Reads a WireGuard `.conf`; `name` (the file's name) becomes the interface's. Errors name the line and key, never
/// a key's value. Keys only `wg-quick` knows (PostUp, Table, …) are skipped.
pub fn parse_wireguard(name: &str, conf: &str) -> Result<WireGuard, String> {
    let interface: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')).take(15).collect();
    let mut wg = WireGuard::default();
    wg.name = if interface.is_empty() { "wg0".into() } else { interface };
    let mut section = String::new();
    for (n, line) in conf.lines().enumerate() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let at = |what: String| format!("line {}: {what}", n + 1);
        if line.starts_with('[') {
            section = line.to_lowercase();
            match section.as_str() {
                "[interface]" => {}
                "[peer]" => wg.peers.push(Peer::default()),
                _ => return Err(at(format!("unknown section {line}"))),
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { return Err(at("not “Key = value”".into())) };
        let (key, value) = (key.trim().to_lowercase(), value.trim());
        let list = || value.split(',').map(str::trim).filter(|s| !s.is_empty());
        let number = || value.parse::<u32>().map_err(|_| at(format!("{key} isn't a number")));
        let key_value = || if is_key(value) { Ok(value.to_owned()) } else { Err(at(format!("{key} isn't a WireGuard key"))) };
        match (section.as_str(), key.as_str()) {
            ("[interface]", "privatekey") => wg.private_key = key_value()?,
            ("[interface]", "address") => {
                for a in list() {
                    let (ip, prefix) = a.split_once('/').unwrap_or((a, ""));
                    let ip: std::net::IpAddr = ip.parse().map_err(|_| at(format!("“{a}” isn't an address")))?;
                    let prefix = if prefix.is_empty() {
                        if ip.is_ipv4() { 32 } else { 128 }
                    } else {
                        prefix.parse().map_err(|_| at(format!("“{a}” isn't an address")))?
                    };
                    wg.addresses.push((ip, prefix));
                }
            }
            ("[interface]", "dns") => {
                for d in list() {
                    match d.parse() {
                        Ok(ip) => wg.dns.push(ip),
                        Err(_) => wg.search.push(d.to_owned()),
                    }
                }
            }
            ("[interface]", "listenport") => wg.listen_port = Some(number()?),
            ("[interface]", "mtu") => wg.mtu = Some(number()?),
            ("[peer]", key) => {
                let peer = wg.peers.last_mut().expect("a [Peer] section has a peer");
                match key {
                    "publickey" => peer.public_key = key_value()?,
                    "presharedkey" => peer.preshared_key = key_value()?,
                    "endpoint" => peer.endpoint = value.to_owned(),
                    "allowedips" => peer.allowed_ips.extend(list().map(str::to_owned)),
                    "persistentkeepalive" if value != "off" => peer.keepalive = Some(number()?),
                    _ => {}
                }
            }
            ("", _) => return Err(at(format!("{key} is outside [Interface] and [Peer]"))),
            _ => {}
        }
    }
    if wg.private_key.is_empty() {
        return Err("no PrivateKey in [Interface]".into());
    }
    if wg.peers.is_empty() || wg.peers.iter().any(|p| p.public_key.is_empty()) {
        return Err("every [Peer] needs a PublicKey".into());
    }
    Ok(wg)
}

/// The new `wireguard` connection; it doesn't come up by itself.
fn wireguard_settings(wg: &WireGuard) -> Dict<'_> {
    let peers: Vec<HashMap<&str, Value>> = wg
        .peers
        .iter()
        .map(|p| {
            let mut peer = HashMap::from([("public-key", Value::from(p.public_key.as_str())), ("allowed-ips", Value::from(p.allowed_ips.clone()))]);
            if !p.endpoint.is_empty() {
                peer.insert("endpoint", p.endpoint.as_str().into());
            }
            if !p.preshared_key.is_empty() {
                peer.insert("preshared-key", p.preshared_key.as_str().into());
                peer.insert("preshared-key-flags", 0u32.into());
            }
            if let Some(seconds) = p.keepalive {
                peer.insert("persistent-keepalive", seconds.into());
            }
            peer
        })
        .collect();
    // NetworkManager stores the private key itself (flags 0), as the file did.
    let mut wireguard = HashMap::from([("private-key", Value::from(wg.private_key.as_str())), ("private-key-flags", 0u32.into()), ("peers", peers.into())]);
    if let Some(port) = wg.listen_port {
        wireguard.insert("listen-port", port.into());
    }
    if let Some(mtu) = wg.mtu {
        wireguard.insert("mtu", mtu.into());
    }
    let ip = |v4: bool| {
        let addresses: Vec<HashMap<&str, Value>> = wg
            .addresses
            .iter()
            .filter(|(a, _)| a.is_ipv4() == v4)
            .map(|(a, prefix)| HashMap::from([("address", Value::from(a.to_string())), ("prefix", Value::from(*prefix))]))
            .collect();
        let mut setting = HashMap::from([("method", Value::from(if addresses.is_empty() { "disabled" } else { "manual" }))]);
        if !addresses.is_empty() {
            setting.insert("address-data", addresses.into());
        }
        setting
    };
    // `ipv4.dns` is `au` in network byte order, `ipv6.dns` `aay` of 16 bytes each.
    let (mut ipv4, mut ipv6) = (ip(true), ip(false));
    let v4: Vec<u32> = wg
        .dns
        .iter()
        .filter_map(|a| match a {
            std::net::IpAddr::V4(a) => Some(u32::from_ne_bytes(a.octets())),
            std::net::IpAddr::V6(_) => None,
        })
        .collect();
    let v6: Vec<Vec<u8>> = wg
        .dns
        .iter()
        .filter_map(|a| match a {
            std::net::IpAddr::V6(a) => Some(a.octets().to_vec()),
            std::net::IpAddr::V4(_) => None,
        })
        .collect();
    // NetworkManager refuses DNS on a family whose method is `disabled`.
    if !v4.is_empty() && ipv4.contains_key("address-data") {
        ipv4.insert("dns", v4.into());
    }
    if !v6.is_empty() && ipv6.contains_key("address-data") {
        ipv6.insert("dns", v6.into());
    }
    // Search domains go on IPv4, or IPv6 when only it has addresses.
    if !wg.search.is_empty()
        && let Some(ip) = [&mut ipv4, &mut ipv6].into_iter().find(|ip| ip.contains_key("address-data"))
    {
        ip.insert("dns-search", wg.search.clone().into());
    }
    let connection = HashMap::from([
        ("id", Value::from(wg.name.as_str())),
        ("type", "wireguard".into()),
        ("interface-name", wg.name.as_str().into()),
        ("autoconnect", false.into()),
    ]);
    Dict::from([("connection", connection), ("wireguard", wireguard), ("ipv4", ipv4), ("ipv6", ipv6)])
}

/// VPN ▸ Import WireGuard: reads the `.conf` at `path` and saves it as a NetworkManager connection.
pub async fn import_wireguard(bus: Connection, path: String) -> zbus::Result<()> {
    let path = std::path::PathBuf::from(path.trim());
    let mut conf = std::fs::read_to_string(&path).map_err(|err| zbus::Error::Failure(format!("{}: {err}", path.display())))?;
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let parsed = parse_wireguard(&name, &conf);
    wipe(&mut conf);
    let wg = parsed.map_err(|err| zbus::Error::Failure(format!("{}: {err}", path.display())))?;
    let settings = "/org/freedesktop/NetworkManager/Settings";
    bus.call_method(Some(NM), settings, Some("org.freedesktop.NetworkManager.Settings"), "AddConnection", &(wireguard_settings(&wg),)).await?;
    Ok(())
}

fn text(v: &OwnedValue) -> Option<String> {
    String::try_from(v.try_clone().ok()?).ok()
}

/// The `ipv4` (`v6`: `ipv6`) setting: method, the first address, gateway and DNS (`dns` is `au` in network byte order
/// for IPv4, `aay` of 16 bytes each for IPv6).
fn ip(setting: &HashMap<String, OwnedValue>, v6: bool) -> Ip {
    use std::net::{Ipv4Addr, Ipv6Addr};
    let address = dicts(setting.get("address-data")).into_iter().next().unwrap_or_default();
    let dns = setting.get("dns").and_then(|v| v.try_clone().ok());
    let dns: Vec<String> = match (dns, v6) {
        (Some(v), false) => Vec::<u32>::try_from(v).unwrap_or_default().into_iter().map(|n| Ipv4Addr::from(n.to_ne_bytes()).to_string()).collect(),
        (Some(v), true) => Vec::<Vec<u8>>::try_from(v)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|b| <[u8; 16]>::try_from(b).ok().map(|b| Ipv6Addr::from(b).to_string()))
            .collect(),
        (None, _) => Vec::new(),
    };
    Ip {
        manual: setting.get("method").and_then(text).as_deref() == Some("manual"),
        address: address.get("address").and_then(text).unwrap_or_default(),
        prefix: address.get("prefix").map(|p| number(Some(p)).to_string()).unwrap_or_default(),
        gateway: setting.get("gateway").and_then(text).unwrap_or_default(),
        dns: dns.join(", "),
    }
}

/// Writes `typed` into a saved connection's `ipv4` (`v6`: `ipv6`) setting; the error says which field isn't an
/// address. A setup left as it was read stays untouched, so an IPv6 method other than DHCP or manual (ignore, …) keeps.
fn set_ip(settings: &mut Settings, typed: &Ip, v6: bool) -> Result<(), String> {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    let name = if v6 { "ipv6" } else { "ipv4" };
    if settings.get(name).map(|s| ip(s, v6)).unwrap_or_default() == *typed {
        return Ok(());
    }
    let version = if v6 { "IPv6" } else { "IPv4" };
    let parse = |what: &str, s: &str| {
        let s = s.trim();
        let ip = if v6 { s.parse::<Ipv6Addr>().map(IpAddr::V6) } else { s.parse::<Ipv4Addr>().map(IpAddr::V4) };
        ip.map_err(|_| format!("“{s}” isn't an {version} {what}"))
    };
    let owned = |v: Value| OwnedValue::try_from(v).map_err(|e| e.to_string());
    let dns = typed.dns.split([',', ' ']).filter(|s| !s.is_empty()).map(|s| parse("DNS server", s)).collect::<Result<Vec<_>, _>>()?;
    let mut new = HashMap::from([("method", owned(Value::from(if typed.manual { "manual" } else { "auto" }))?)]);
    if !dns.is_empty() {
        let dns = match v6 {
            false => {
                Value::from(dns.iter().filter_map(|a| if let IpAddr::V4(a) = a { Some(u32::from_ne_bytes(a.octets())) } else { None }).collect::<Vec<u32>>())
            }
            true => Value::from(dns.iter().filter_map(|a| if let IpAddr::V6(a) = a { Some(a.octets().to_vec()) } else { None }).collect::<Vec<Vec<u8>>>()),
        };
        new.insert("dns", owned(dns)?);
    }
    if typed.manual {
        let address = parse("address", &typed.address)?;
        let most = if v6 { 128 } else { 32 };
        let prefix = typed
            .prefix
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|p| (1..=most).contains(p))
            .ok_or_else(|| format!("“{}” isn't a prefix length (1–{most})", typed.prefix.trim()))?;
        new.insert("address-data", owned(Value::from(vec![HashMap::from([("address", Value::from(address.to_string())), ("prefix", Value::from(prefix))])]))?);
        if !typed.gateway.trim().is_empty() {
            new.insert("gateway", owned(Value::from(parse("router", &typed.gateway)?.to_string()))?);
        }
    }
    let setting = settings.entry(name.into()).or_default();
    // The older `addresses` and newer `dns-data` would win over (or clash with) what is written here.
    for key in ["method", "addresses", "address-data", "gateway", "dns", "dns-data"] {
        setting.remove(key);
    }
    setting.extend(new.into_iter().map(|(k, v)| (k.to_owned(), v)));
    Ok(())
}

/// Details ▸ Update: the connection's IPv4 and IPv6 setup; it applies the next time the connection comes up.
pub async fn update_ip(bus: Connection, path: String, ip4: Ip, ip6: Ip) -> zbus::Result<()> {
    let mut settings: Settings = bus.call_method(Some(NM), path.as_str(), Some(SAVED), "GetSettings", &()).await?.body().deserialize()?;
    set_ip(&mut settings, &ip4, false).and_then(|()| set_ip(&mut settings, &ip6, true)).map_err(zbus::Error::Failure)?;
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

/// A network in range: its saved connection (with `password` as its new one), or a new WPA-PSK one.
pub async fn join(bus: Connection, ap: Ap, password: Option<String>) -> zbus::Result<()> {
    let (point, device) = (ObjectPath::try_from(ap.ap.as_str())?, ObjectPath::try_from(ap.device.as_str())?);
    let manager = Some("org.freedesktop.NetworkManager");
    match (&ap.saved, password) {
        (Some(saved), password) => {
            if let Some(password) = password {
                let settings: Settings = bus.call_method(Some(NM), saved.as_str(), Some(SAVED), "GetSettings", &()).await?.body().deserialize()?;
                bus.call_method(Some(NM), saved.as_str(), Some(SAVED), "Update", &(with_password(settings, &password)?,)).await?;
            }
            bus.call_method(
                Some(NM),
                "/org/freedesktop/NetworkManager",
                manager,
                "ActivateConnection",
                &(ObjectPath::try_from(saved.as_str())?, device, point),
            )
            .await?;
        }
        (None, password) => {
            let mut settings: HashMap<&str, HashMap<&str, Value>> = HashMap::new();
            if let Some(psk) = password {
                settings.insert("802-11-wireless-security", HashMap::from([("key-mgmt", Value::from("wpa-psk")), ("psk", Value::from(psk))]));
            }
            bus.call_method(Some(NM), "/org/freedesktop/NetworkManager", manager, "AddAndActivateConnection", &(settings, device, point)).await?;
        }
    }
    Ok(())
}

/// A saved connection's settings with `password` as its key: the WEP key on a WEP one, else the WPA one, stored by
/// NetworkManager (flags 0). The desktop's join (src/services/network.rs) does the same.
fn with_password(mut settings: Settings, password: &str) -> zbus::Result<Settings> {
    let value = |v: Value| OwnedValue::try_from(v).map_err(zbus::Error::from);
    let security = settings.entry("802-11-wireless-security".into()).or_default();
    let key_mgmt = security.get("key-mgmt").and_then(|v| String::try_from(v.try_clone().ok()?).ok()).unwrap_or_else(|| "wpa-psk".into());
    if key_mgmt == "none" {
        let index = security.get("wep-tx-keyidx").and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
        // wep-key-type: 1, a key (10 or 26 hex digits, or 5 or 13 ASCII characters); 2, a passphrase.
        let key =
            (matches!(password.len(), 10 | 26) && password.chars().all(|c| c.is_ascii_hexdigit())) || (matches!(password.len(), 5 | 13) && password.is_ascii());
        security.insert(format!("wep-key{index}"), value(Value::from(password))?);
        security.insert("wep-key-type".into(), value(Value::from(if key { 1u32 } else { 2 }))?);
        security.insert("wep-key-flags".into(), value(Value::from(0u32))?);
    } else {
        security.insert("key-mgmt".into(), value(Value::from(key_mgmt))?);
        security.insert("psk".into(), value(Value::from(password))?);
        security.insert("psk-flags".into(), value(Value::from(0u32))?);
    }
    Ok(settings)
}

/// "Other…": a network that doesn't announce its name. `security` is "none", "psk" (WPA2/WPA3 Personal), "sae" (WPA3
/// Personal only) or "eap"
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
        // wpa-psk joins WPA2 and WPA2/WPA3 mixed networks; a WPA3-only one needs sae.
        "psk" | "sae" => {
            let key_mgmt = if h.security == "sae" { "sae" } else { "wpa-psk" };
            settings.insert("802-11-wireless-security", HashMap::from([("key-mgmt", key_mgmt.into()), ("psk", h.password.as_str().into())]));
        }
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

    #[test]
    fn new_password_into_saved_settings() {
        let value = |v: Value| OwnedValue::try_from(v).unwrap();
        let text = |s: &Settings, k: &str| String::try_from(s["802-11-wireless-security"][k].try_clone().unwrap()).unwrap();
        let number = |s: &Settings, k: &str| u32::try_from(&s["802-11-wireless-security"][k]).unwrap();
        let wpa = with_password(Settings::new(), "hunter22").unwrap();
        assert_eq!((text(&wpa, "key-mgmt"), text(&wpa, "psk"), number(&wpa, "psk-flags")), ("wpa-psk".into(), "hunter22".into(), 0));
        let wep = |key: &str| {
            let saved = HashMap::from([("802-11-wireless-security".into(), HashMap::from([("key-mgmt".into(), value(Value::from("none")))]))]);
            with_password(saved, key).unwrap()
        };
        assert_eq!((text(&wep("0123456789"), "wep-key0"), number(&wep("0123456789"), "wep-key-type")), ("0123456789".into(), 1));
        assert_eq!(number(&wep("a long passphrase"), "wep-key-type"), 2);
    }

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
        h.security = "sae".into();
        let sae = hidden_settings(&h);
        assert_eq!((text(&sae["802-11-wireless-security"]["key-mgmt"]), text(&sae["802-11-wireless-security"]["psk"])), ("sae".into(), "hunter22".into()));
        drop(sae);

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
        let typed = Ip { manual: true, address: "192.168.1.20".into(), prefix: "24".into(), gateway: "192.168.1.1".into(), dns: "1.1.1.1, 9.9.9.9".into() };
        set_ip(&mut settings, &typed, false).unwrap();
        assert!(!settings["ipv4"].contains_key("addresses"));
        // `dns` is in network byte order: 1.1.1.1's bytes as they are.
        assert_eq!(Vec::<u32>::try_from(settings["ipv4"]["dns"].try_clone().unwrap()).unwrap()[0], u32::from_ne_bytes([1, 1, 1, 1]));
        assert_eq!(ip(&settings["ipv4"], false), typed);

        let auto = Ip { dns: String::new(), ..typed.clone() };
        set_ip(&mut settings, &Ip { manual: false, ..auto }, false).unwrap();
        assert_eq!(ip(&settings["ipv4"], false), Ip::default());

        for bad in [Ip { prefix: "33".into(), ..typed.clone() }, Ip { gateway: "router".into(), ..typed.clone() }, Ip { dns: "1.1.1".into(), ..typed }] {
            assert!(set_ip(&mut settings, &bad, false).is_err());
        }
    }

    #[test]
    fn manual_ip6_round_trip() {
        let mut settings: Settings = HashMap::from([("ipv6".to_string(), HashMap::from([("method".to_string(), value(Value::from("ignore")))]))]);
        // Left as read: "ignore" stays.
        set_ip(&mut settings, &Ip::default(), true).unwrap();
        assert_eq!(text(&settings["ipv6"]["method"]).as_deref(), Some("ignore"));

        let typed =
            Ip { manual: true, address: "2001:db8::20".into(), prefix: "64".into(), gateway: "fe80::1".into(), dns: "2606:4700:4700::1111, ::1".into() };
        set_ip(&mut settings, &typed, true).unwrap();
        let dns = Vec::<Vec<u8>>::try_from(settings["ipv6"]["dns"].try_clone().unwrap()).unwrap();
        assert_eq!(dns[1], std::net::Ipv6Addr::LOCALHOST.octets());
        assert_eq!(ip(&settings["ipv6"], true), typed);
        assert!(!settings.contains_key("ipv4"), "IPv6 alone");

        let bad = [
            Ip { prefix: "129".into(), ..typed.clone() },
            Ip { address: "192.168.1.20".into(), ..typed.clone() },
            Ip { address: "2001:db8::g".into(), ..typed.clone() },
            Ip { gateway: "router".into(), ..typed.clone() },
            Ip { dns: "1.1.1.1".into(), ..typed.clone() },
        ];
        for bad in bad {
            assert!(set_ip(&mut settings, &bad, true).is_err(), "{bad:?}");
        }
        set_ip(&mut settings, &Ip::default(), true).unwrap();
        assert_eq!(text(&settings["ipv6"]["method"]).as_deref(), Some("auto"));
    }

    const PRIVATE: &str = "yAnz5TF+lXXJte14tji3zlMNq+hd2rYUIgJBgB3fBmk=";
    const PUBLIC: &str = "xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=";

    #[test]
    fn wireguard_conf() {
        let conf = format!(
            "[Interface]\nPrivateKey = {PRIVATE}\nAddress = 10.0.0.2/24, fd00::2\nDNS = 1.1.1.1, example.lan, 2606:4700:4700::1111\nPostUp = iptables …\n\n\
             [Peer] # the server\nPublicKey = {PUBLIC}\nPresharedKey = {PUBLIC}\nEndpoint = vpn.example.com:51820\n\
             AllowedIPs = 0.0.0.0/0, ::/0\nPersistentKeepalive = 25\n"
        );
        let wg = parse_wireguard("my vpn.conf-with-a-long-name", &conf).unwrap();
        let settings = wireguard_settings(&wg);
        let text = |v: &Value| String::try_from(v.try_clone().unwrap()).unwrap();
        assert_eq!(text(&settings["connection"]["type"]), "wireguard");
        assert_eq!(text(&settings["connection"]["interface-name"]), "myvpnconf-with-");
        assert_eq!(text(&settings["wireguard"]["private-key"]), PRIVATE);
        let peers = Vec::<HashMap<String, OwnedValue>>::try_from(settings["wireguard"]["peers"].try_clone().unwrap()).unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(text(&peers[0]["endpoint"].try_clone().unwrap().into()), "vpn.example.com:51820");
        assert_eq!(Vec::<String>::try_from(peers[0]["allowed-ips"].try_clone().unwrap()).unwrap(), ["0.0.0.0/0", "::/0"]);
        assert_eq!(u32::try_from(&peers[0]["persistent-keepalive"]).unwrap(), 25);
        let address = |v4: &str| dicts(Some(&OwnedValue::try_from(settings[v4]["address-data"].try_clone().unwrap()).unwrap()));
        assert_eq!(
            (text(&address("ipv4")[0]["address"].try_clone().unwrap().into()), u32::try_from(&address("ipv4")[0]["prefix"]).unwrap()),
            ("10.0.0.2".into(), 24)
        );
        assert_eq!(u32::try_from(&address("ipv6")[0]["prefix"]).unwrap(), 128);
        assert_eq!(Vec::<u32>::try_from(settings["ipv4"]["dns"].try_clone().unwrap()).unwrap(), [u32::from_ne_bytes([1, 1, 1, 1])]);
        let v6 = Vec::<Vec<u8>>::try_from(settings["ipv6"]["dns"].try_clone().unwrap()).unwrap();
        assert_eq!(v6, ["2606:4700:4700::1111".parse::<std::net::Ipv6Addr>().unwrap().octets().to_vec()]);
        assert_eq!(text(&address("ipv6")[0]["address"].try_clone().unwrap().into()), "fd00::2");
        assert_eq!(Vec::<String>::try_from(settings["ipv4"]["dns-search"].try_clone().unwrap()).unwrap(), ["example.lan"]);
        assert!(!settings["ipv6"].contains_key("dns-search"));
        // Only IPv6 addresses: the search domains go there; none without any address.
        let v6_only = parse_wireguard("wg0", &conf.replace("10.0.0.2/24, ", "")).unwrap();
        let settings = wireguard_settings(&v6_only);
        assert!(!settings["ipv4"].contains_key("dns-search"));
        assert_eq!(Vec::<String>::try_from(settings["ipv6"]["dns-search"].try_clone().unwrap()).unwrap(), ["example.lan"]);
        let none = parse_wireguard("wg0", &conf.replace("Address = 10.0.0.2/24, fd00::2\n", "")).unwrap();
        assert!(wireguard_settings(&none).values().all(|s| !s.contains_key("dns-search")));
    }

    #[test]
    fn wireguard_rejects_bad_keys() {
        let conf = |private: &str, public: &str| format!("[Interface]\nPrivateKey = {private}\n[Peer]\nPublicKey = {public}\nAllowedIPs = 0.0.0.0/0\n");
        assert!(parse_wireguard("wg0", &conf(PRIVATE, PUBLIC)).is_ok());
        for private in ["short=", &PRIVATE.replace('k', "!"), &PRIVATE.replace("Bmk=", "Bml="), &PRIVATE[..43]] {
            let err = parse_wireguard("wg0", &conf(private, PUBLIC)).err().unwrap();
            // The error says which key, never what it is.
            assert!(err.contains("privatekey") && !err.contains(private), "{err}");
        }
        assert!(parse_wireguard("wg0", &conf(PRIVATE, "nope")).is_err());
        assert!(parse_wireguard("wg0", &format!("[Interface]\nPrivateKey = {PRIVATE}\n")).is_err());
        assert!(parse_wireguard("wg0", &format!("PrivateKey = {PRIVATE}\n")).is_err());
        assert!(parse_wireguard("wg0", "[Interface]\nAddress = nonsense\n").is_err());
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
        let known = [Known { path: "/s/1".into(), ssid: "Home".into(), wifi: true, auto_join: true, ip4: Ip::default(), ip6: Ip::default() }];
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
