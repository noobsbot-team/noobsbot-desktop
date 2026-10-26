//! Thunderbolt accessories from boltd (E8.8, G2): a newly plugged device waits, "connected" but not authorized,
//! until the user allows it. Allow enrolls it with policy "auto", so boltd lets it in by itself next time.

use std::collections::HashMap;

use smithay::reexports::calloop::channel::Sender;
use zbus::{
    Connection,
    fdo::PropertiesProxy,
    names::InterfaceName,
    zvariant::{OwnedObjectPath, OwnedValue},
};

use super::Update;

const SERVICE: &str = "org.freedesktop.bolt";
const DEVICE: InterfaceName<'static> = InterfaceName::from_static_str_unchecked("org.freedesktop.bolt1.Device");

/// A device waiting to be allowed.
#[derive(Clone, Debug, PartialEq)]
pub struct Accessory {
    /// boltd's unique id, for enrolling.
    pub uid: String,
    /// Vendor and name, as the alert shows it.
    pub name: String,
}

#[zbus::proxy(interface = "org.freedesktop.bolt1.Manager", default_service = "org.freedesktop.bolt", default_path = "/org/freedesktop/bolt")]
trait Manager {
    fn list_devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
    fn enroll_device(&self, uid: &str, policy: &str, flags: &str) -> zbus::Result<OwnedObjectPath>;
}

/// Sends the waiting devices now and after every boltd signal (`DeviceAdded`, a status change); boltd starting later
/// is picked up too. Without boltd nothing is sent.
pub async fn watch(bus: &Connection, updates: &Sender<Update>) {
    super::follow(bus, SERVICE, "/org/freedesktop/bolt", updates, Update::Thunderbolt, read).await;
}

/// `None` when boltd doesn't answer.
async fn read(bus: &Connection) -> Option<Vec<Accessory>> {
    let paths = ManagerProxy::new(bus).await.ok()?.list_devices().await.ok()?;
    let mut waiting = Vec::new();
    for path in paths {
        let props = async { PropertiesProxy::builder(bus).destination(SERVICE)?.path(path)?.build().await?.get_all(DEVICE).await };
        waiting.extend(props.await.ok().and_then(|p| accessory(&p)));
    }
    Some(waiting)
}

/// The alert's Allow: store the device with policy "auto", which also authorizes it now.
pub async fn enroll(bus: &Connection, uid: &str) {
    let result = async { ManagerProxy::new(bus).await?.enroll_device(uid, "auto", "").await };
    if let Err(err) = result.await {
        tracing::warn!("Thunderbolt enroll {uid}: {err}");
    }
}

/// A device from its properties, when it waits for the user: status "connected" (plugged, not authorized). Every
/// other status ("authorized", "connecting", "authorizing", "auth-error", "disconnected") needs no answer.
fn accessory(props: &HashMap<String, OwnedValue>) -> Option<Accessory> {
    let text = |key: &str| props.get(key).and_then(|v| v.downcast_ref::<&str>().ok()).unwrap_or_default().trim().to_owned();
    if text("Status") != "connected" {
        return None;
    }
    let (vendor, name) = (text("Vendor"), text("Name"));
    // Names often repeat the vendor ("Dell" + "Dell WD19TB").
    let name = if name.starts_with(&vendor) { name } else { format!("{vendor} {name}").trim().to_owned() };
    Some(Accessory { uid: text("Uid"), name: if name.is_empty() { "Thunderbolt accessory".into() } else { name } })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(pairs: &[(&str, &str)]) -> HashMap<String, OwnedValue> {
        pairs.iter().map(|(k, v)| (k.to_string(), OwnedValue::from(zbus::zvariant::Str::from(*v).into_owned()))).collect()
    }

    #[test]
    fn only_connected_waits() {
        for status in ["authorized", "connecting", "authorizing", "auth-error", "disconnected", "unknown", ""] {
            assert_eq!(accessory(&props(&[("Status", status), ("Uid", "u")])), None, "{status}");
        }
        let dock = accessory(&props(&[("Status", "connected"), ("Uid", "u1"), ("Vendor", "Dell"), ("Name", "WD19TB")]));
        assert_eq!(dock, Some(Accessory { uid: "u1".into(), name: "Dell WD19TB".into() }));
    }

    #[test]
    fn names() {
        let name = |vendor, name| accessory(&props(&[("Status", "connected"), ("Vendor", vendor), ("Name", name)])).unwrap().name;
        assert_eq!(name("Dell", "Dell WD19TB"), "Dell WD19TB");
        assert_eq!(name("", "Dock"), "Dock");
        assert_eq!(name("Acme", ""), "Acme");
        assert_eq!(name("", ""), "Thunderbolt accessory");
    }
}
