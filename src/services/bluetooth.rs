//! Bluetooth state from BlueZ (E2.3): whether it is on, and the connected devices with their battery level.

use std::collections::HashMap;

use smithay::reexports::calloop::channel::Sender;
use zbus::{
    Connection,
    fdo::{ManagedObjects, ObjectManagerProxy},
    names::OwnedInterfaceName,
    zvariant::OwnedValue,
};

use super::Update;

const SERVICE: &str = "org.bluez";

#[derive(Clone, Debug, PartialEq)]
pub struct Bluetooth {
    pub powered: bool,
    /// Connected devices, sorted by name.
    pub devices: Vec<Device>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub name: String,
    /// From BlueZ's Battery1 interface, when the device reports one.
    pub battery: Option<u8>,
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
        .values()
        .filter(|i| flag(i, "org.bluez.Device1", "Connected"))
        .map(|i| Device {
            name: get(i, "org.bluez.Device1", "Alias").and_then(|v| v.downcast_ref::<&str>().ok()).unwrap_or_default().to_owned(),
            battery: get(i, "org.bluez.Battery1", "Percentage").and_then(|v| v.downcast_ref::<u8>().ok()),
        })
        .collect();
    devices.sort_by(|a, b| a.name.cmp(&b.name));
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
    fn connected_devices_with_battery() {
        let objects: ManagedObjects = [
            (path("/org/bluez/hci0"), object(&[("org.bluez.Adapter1", &[("Address", value("AA:BB")), ("Powered", value(true))])])),
            (
                path("/org/bluez/hci0/dev_1"),
                object(&[
                    ("org.bluez.Device1", &[("Alias", value("AirPods Pro")), ("Connected", value(true))]),
                    ("org.bluez.Battery1", &[("Percentage", value(80u8))]),
                ]),
            ),
            (path("/org/bluez/hci0/dev_2"), object(&[("org.bluez.Device1", &[("Alias", value("Keyboard")), ("Connected", value(false))])])),
            (path("/org/bluez/hci0/dev_3"), object(&[("org.bluez.Device1", &[("Alias", value("Mouse")), ("Connected", value(true))])])),
        ]
        .into_iter()
        .collect();
        let bluetooth = summarize(&objects).unwrap();
        assert!(bluetooth.powered);
        assert_eq!(
            bluetooth.devices,
            [Device { name: "AirPods Pro".into(), battery: Some(80) }, Device { name: "Mouse".into(), battery: None }]
        );
    }

    #[test]
    fn adapter_off() {
        let objects: ManagedObjects =
            [(path("/org/bluez/hci0"), object(&[("org.bluez.Adapter1", &[("Address", value("AA:BB")), ("Powered", value(false))])]))].into_iter().collect();
        assert_eq!(summarize(&objects), Some(Bluetooth { powered: false, devices: vec![] }));
    }
}
