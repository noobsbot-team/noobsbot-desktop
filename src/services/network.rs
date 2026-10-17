//! Network state from NetworkManager (E2.3): the primary connection and, on Wi-Fi, its network name and signal bars.

use smithay::reexports::calloop::channel::Sender;
use zbus::{Connection, proxy::CacheProperties, zvariant::OwnedObjectPath};

use super::Update;

const SERVICE: &str = "org.freedesktop.NetworkManager";
const ROOT: &str = "/org/freedesktop/NetworkManager";

#[derive(Clone, Debug, PartialEq)]
pub struct Network {
    pub wifi_enabled: bool,
    pub link: Link,
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
    fn primary_connection(&self) -> zbus::Result<OwnedObjectPath>;
}

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

/// `None` when NetworkManager doesn't answer.
async fn read(bus: &Connection) -> Option<Network> {
    let manager = ManagerProxy::builder(bus).cache_properties(CacheProperties::No).build().await.ok()?;
    let wifi_enabled = manager.wireless_enabled().await.ok()?;
    let primary = manager.primary_connection().await.ok()?;
    // A connection that vanishes while we read it is simply gone; the signal that follows corrects the rest.
    let link = if primary.as_str() == "/" { Link::None } else { link(bus, primary).await.unwrap_or(Link::None) };
    Some(Network { wifi_enabled, link })
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
    fn strength_to_bars() {
        assert_eq!([0, 33, 34, 66, 67, 100].map(bars), [1, 1, 2, 2, 3, 3]);
    }
}
