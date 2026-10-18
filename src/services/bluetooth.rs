//! Bluetooth state from BlueZ (E2.3): whether it is on, and the paired devices with their battery level.

use std::collections::HashMap;

use smithay::reexports::calloop::channel::Sender;
use zbus::{
    Connection,
    fdo::{ManagedObjects, ObjectManagerProxy},
    names::OwnedInterfaceName,
    proxy::CacheProperties,
    zvariant::OwnedValue,
};

use super::Update;

const SERVICE: &str = "org.bluez";

#[derive(Clone, Debug, PartialEq)]
pub struct Bluetooth {
    pub powered: bool,
    /// Paired or connected devices: connected first, then by name.
    pub devices: Vec<Device>,
}

impl Bluetooth {
    pub fn connected(&self) -> impl Iterator<Item = &Device> {
        self.devices.iter().filter(|d| d.connected)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub name: String,
    /// From BlueZ's Battery1 interface, when the device reports one.
    pub battery: Option<u8>,
    pub connected: bool,
    /// BlueZ's object path, for connecting.
    pub path: String,
}

/// Sends the Bluetooth state now and after every change: adapters and devices appearing, connecting, powering.
pub async fn watch(bus: &Connection, updates: &Sender<Update>) {
    // "/" because BlueZ announces new objects (InterfacesAdded) from its root.
    super::follow(bus, SERVICE, "/", updates, Update::Bluetooth, read).await;
}

/// `None` when BlueZ doesn't answer or there is no adapter.
async fn read(bus: &Connection) -> Option<Bluetooth> {
    let manager = ObjectManagerProxy::builder(bus).destination(SERVICE).ok()?.path("/").ok()?.build().await.ok()?;
    summarize(&manager.get_managed_objects().await.ok()?)
}

#[zbus::proxy(interface = "org.bluez.Device1", default_service = "org.bluez")]
trait BluezDevice {
    fn connect(&self) -> zbus::Result<()>;
    fn disconnect(&self) -> zbus::Result<()>;
}

/// Control Center ▸ Bluetooth ▸ a device: connects or disconnects it.
pub async fn connect(bus: &Connection, path: &str, on: bool) {
    let result = async {
        let device = BluezDeviceProxy::builder(bus).path(path)?.build().await?;
        if on { device.connect().await } else { device.disconnect().await }
    };
    if let Err(err) = result.await {
        tracing::warn!("Bluetooth {} {path}: {err}", if on { "connect" } else { "disconnect" });
    }
}

#[zbus::proxy(interface = "org.bluez.Adapter1", default_service = "org.bluez")]
trait Adapter {
    #[zbus(property)]
    fn powered(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn set_powered(&self, on: bool) -> zbus::Result<()>;
}

/// Control Center ▸ Bluetooth: every adapter on or off.
pub async fn set_powered(bus: &Connection, on: bool) {
    let set = async {
        let manager = ObjectManagerProxy::builder(bus).destination(SERVICE)?.path("/")?.build().await?;
        for (path, interfaces) in manager.get_managed_objects().await? {
            if get(&interfaces, "org.bluez.Adapter1", "Address").is_some() {
                AdapterProxy::builder(bus).path(path)?.cache_properties(CacheProperties::No).build().await?.set_powered(on).await?;
            }
        }
        zbus::Result::Ok(())
    };
    if let Err(err) = set.await {
        tracing::warn!("Bluetooth {}: {err}", if on { "on" } else { "off" });
    }
}

type Interfaces = HashMap<OwnedInterfaceName, HashMap<String, OwnedValue>>;

fn get<'a>(interfaces: &'a Interfaces, interface: &str, property: &str) -> Option<&'a OwnedValue> {
    interfaces.iter().find(|(name, _)| name.as_str() == interface)?.1.get(property)
}

fn flag(interfaces: &Interfaces, interface: &str, property: &str) -> bool {
    get(interfaces, interface, property).and_then(|v| v.downcast_ref::<bool>().ok()).unwrap_or(false)
}

fn summarize(objects: &ManagedObjects) -> Option<Bluetooth> {
    let adapters: Vec<_> = objects.values().filter(|i| get(i, "org.bluez.Adapter1", "Address").is_some()).collect();
    if adapters.is_empty() {
        return None;
    }
    let mut devices: Vec<Device> = objects
        .iter()
        .filter(|(_, i)| flag(i, "org.bluez.Device1", "Connected") || flag(i, "org.bluez.Device1", "Paired"))
        .map(|(path, i)| Device {
            name: get(i, "org.bluez.Device1", "Alias").and_then(|v| v.downcast_ref::<&str>().ok()).unwrap_or_default().to_owned(),
            battery: get(i, "org.bluez.Battery1", "Percentage").and_then(|v| v.downcast_ref::<u8>().ok()),
            connected: flag(i, "org.bluez.Device1", "Connected"),
            path: path.to_string(),
        })
        .collect();
    devices.sort_by(|a, b| b.connected.cmp(&a.connected).then_with(|| a.name.cmp(&b.name)));
    Some(Bluetooth { powered: adapters.iter().any(|i| flag(i, "org.bluez.Adapter1", "Powered")), devices })
}

#[cfg(test)]
mod tests {
    use zbus::zvariant::OwnedObjectPath;

    use super::*;

    fn object(interfaces: &[(&str, &[(&str, OwnedValue)])]) -> Interfaces {
        interfaces
            .iter()
            .map(|(name, props)| {
                let props = props.iter().map(|(k, v)| (k.to_string(), v.try_clone().unwrap())).collect();
                (OwnedInterfaceName::try_from(*name).unwrap(), props)
            })
            .collect()
    }

    fn path(p: &str) -> OwnedObjectPath {
        OwnedObjectPath::try_from(p).unwrap()
    }

    fn value<T: Into<zbus::zvariant::Value<'static>>>(v: T) -> OwnedValue {
        OwnedValue::try_from(v.into()).unwrap()
    }

    #[test]
    fn no_adapter_is_none() {
        assert_eq!(summarize(&ManagedObjects::new()), None);
    }

    #[test]
    fn paired_and_connected_devices() {
        let objects: ManagedObjects = [
            (path("/org/bluez/hci0"), object(&[("org.bluez.Adapter1", &[("Address", value("AA:BB")), ("Powered", value(true))])])),
            (
                path("/org/bluez/hci0/dev_1"),
                object(&[
                    ("org.bluez.Device1", &[("Alias", value("AirPods Pro")), ("Connected", value(true))]),
                    ("org.bluez.Battery1", &[("Percentage", value(80u8))]),
                ]),
            ),
            (path("/org/bluez/hci0/dev_2"), object(&[("org.bluez.Device1", &[("Alias", value("Keyboard")), ("Connected", value(false)), ("Paired", value(true))])])),
            (path("/org/bluez/hci0/dev_4"), object(&[("org.bluez.Device1", &[("Alias", value("Stranger")), ("Connected", value(false))])])),
            (path("/org/bluez/hci0/dev_3"), object(&[("org.bluez.Device1", &[("Alias", value("Mouse")), ("Connected", value(true))])])),
        ]
        .into_iter()
        .collect();
        let bluetooth = summarize(&objects).unwrap();
        assert!(bluetooth.powered);
        let device = |name: &str, battery, connected, dev: &str| Device { name: name.into(), battery, connected, path: format!("/org/bluez/hci0/{dev}") };
        // Connected first; an unpaired, unconnected device is left out.
        assert_eq!(
            bluetooth.devices,
            [device("AirPods Pro", Some(80), true, "dev_1"), device("Mouse", None, true, "dev_3"), device("Keyboard", None, false, "dev_2")]
        );
        assert_eq!(bluetooth.connected().count(), 2);
    }

    #[test]
    fn adapter_off() {
        let objects: ManagedObjects =
            [(path("/org/bluez/hci0"), object(&[("org.bluez.Adapter1", &[("Address", value("AA:BB")), ("Powered", value(false))])]))].into_iter().collect();
        assert_eq!(summarize(&objects), Some(Bluetooth { powered: false, devices: vec![] }));
    }
}
