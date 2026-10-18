//! USB drives and SD cards from UDisks2 (E4.6). UDisks2 doesn't automount; the frame loop decides (see
//! `shell::disks`) and this module mounts, unmounts and powers drives off through polkit's session rules.

use std::collections::HashMap;

use smithay::reexports::calloop::channel::Sender;
use zbus::{
    Connection,
    fdo::{ManagedObjects, ObjectManagerProxy},
    zvariant::{OwnedValue, Value},
};

use super::{Interfaces, Update, get};

const SERVICE: &str = "org.freedesktop.UDisks2";
const ROOT: &str = "/org/freedesktop/UDisks2";
const BLOCK: &str = "org.freedesktop.UDisks2.Block";
const FILESYSTEM: &str = "org.freedesktop.UDisks2.Filesystem";
const DRIVE: &str = "org.freedesktop.UDisks2.Drive";

/// A filesystem a person may plug in: never one UDisks2 marks as the system's own or as hidden.
#[derive(Clone, Debug, PartialEq)]
pub struct Volume {
    /// The block device's object path, for mounting.
    pub block: String,
    /// The drive's object path ("/" for none), for ejecting.
    pub drive: String,
    /// The filesystem label, else the drive's vendor and model.
    pub name: String,
    /// Mount points; empty when not mounted.
    pub mounts: Vec<String>,
    /// UDisks2's `HintAuto`: removable media that should mount by itself.
    pub auto: bool,
}

/// Sends the volumes now and after every change.
pub async fn watch(bus: &Connection, updates: &Sender<Update>) {
    super::follow(bus, SERVICE, ROOT, updates, |v| Update::Disks(v.unwrap_or_default()), read).await;
}

async fn read(bus: &Connection) -> Option<Vec<Volume>> {
    let manager = ObjectManagerProxy::builder(bus).destination(SERVICE).ok()?.path(ROOT).ok()?.build().await.ok()?;
    Some(summarize(&manager.get_managed_objects().await.ok()?))
}

fn text(interfaces: &Interfaces, interface: &str, property: &str) -> String {
    get(interfaces, interface, property).and_then(|v| v.downcast_ref::<&str>().ok()).unwrap_or_default().trim().to_owned()
}

fn flag(interfaces: &Interfaces, interface: &str, property: &str) -> bool {
    get(interfaces, interface, property).and_then(|v| v.downcast_ref::<bool>().ok()).unwrap_or(false)
}

/// `aay`: NUL-terminated paths.
fn mounts(value: Option<&OwnedValue>) -> Vec<String> {
    let paths = value.and_then(|v| Vec::<Vec<u8>>::try_from(v.try_clone().ok()?).ok()).unwrap_or_default();
    paths.iter().map(|p| String::from_utf8_lossy(p.strip_suffix(&[0]).unwrap_or(p)).into_owned()).collect()
}

fn summarize(objects: &ManagedObjects) -> Vec<Volume> {
    let mut volumes: Vec<Volume> = objects
        .iter()
        .filter(|(_, i)| i.keys().any(|n| n.as_str() == FILESYSTEM) && !flag(i, BLOCK, "HintSystem") && !flag(i, BLOCK, "HintIgnore"))
        .map(|(path, i)| {
            let drive = get(i, BLOCK, "Drive").and_then(|v| v.downcast_ref::<zbus::zvariant::ObjectPath>().ok()).map_or("/".into(), |p| p.to_string());
            let model = objects.iter().find(|(p, _)| p.as_str() == drive).map(|(_, d)| format!("{} {}", text(d, DRIVE, "Vendor"), text(d, DRIVE, "Model")));
            let name = [text(i, BLOCK, "HintName"), text(i, BLOCK, "IdLabel"), model.unwrap_or_default().trim().to_owned()].into_iter().find(|n| !n.is_empty());
            Volume {
                block: path.to_string(),
                drive,
                name: name.unwrap_or_else(|| "Untitled".into()),
                mounts: mounts(get(i, FILESYSTEM, "MountPoints")),
                auto: flag(i, BLOCK, "HintAuto"),
            }
        })
        .collect();
    volumes.sort_by(|a, b| a.block.cmp(&b.block));
    volumes
}

#[zbus::proxy(interface = "org.freedesktop.UDisks2.Filesystem", default_service = "org.freedesktop.UDisks2")]
trait Filesystem {
    fn mount(&self, options: HashMap<&str, Value<'_>>) -> zbus::Result<String>;
    fn unmount(&self, options: HashMap<&str, Value<'_>>) -> zbus::Result<()>;
}

#[zbus::proxy(interface = "org.freedesktop.UDisks2.Drive", default_service = "org.freedesktop.UDisks2")]
trait Drive {
    fn power_off(&self, options: HashMap<&str, Value<'_>>) -> zbus::Result<()>;
    fn eject(&self, options: HashMap<&str, Value<'_>>) -> zbus::Result<()>;
    #[zbus(property)]
    fn can_power_off(&self) -> zbus::Result<bool>;
}

/// Mounts `block` under /run/media/$USER; a failure reaches the frame loop as `Update::DiskFailed`.
pub async fn mount(bus: &Connection, block: &str, name: String, updates: &Sender<Update>) {
    let result = async { FilesystemProxy::builder(bus).path(block)?.build().await?.mount(HashMap::new()).await };
    if let Err(err) = result.await {
        tracing::warn!("mounting {block}: {err}");
        let _ = updates.send(Update::DiskFailed { name, error: reason(&err) });
    }
}

/// Unmounts `blocks`, then powers `drive` off (or ejects its media) so it can be pulled out.
pub async fn eject(bus: &Connection, drive: &str, blocks: &[String], name: String, updates: &Sender<Update>) {
    let result = async {
        for block in blocks {
            FilesystemProxy::builder(bus).path(block.as_str())?.build().await?.unmount(HashMap::new()).await?;
        }
        if drive == "/" {
            return Ok(());
        }
        let drive = DriveProxy::builder(bus).path(drive)?.build().await?;
        // Card readers can't power off; ejecting the card is the best they do.
        if drive.can_power_off().await.unwrap_or(false) { drive.power_off(HashMap::new()).await } else { drive.eject(HashMap::new()).await }
    };
    if let Err(err) = result.await {
        tracing::warn!("ejecting {drive}: {err}");
        let _ = updates.send(Update::DiskFailed { name, error: reason(&err) });
    }
}

/// UDisks2's message without its D-Bus error name.
fn reason(err: &zbus::Error) -> String {
    match err {
        zbus::Error::MethodError(_, Some(message), _) => message.clone(),
        err => err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use zbus::{
        names::OwnedInterfaceName,
        zvariant::{ObjectPath, OwnedObjectPath},
    };

    use super::*;

    fn object(interfaces: &[(&str, Vec<(&str, OwnedValue)>)]) -> Interfaces {
        interfaces
            .iter()
            .map(|(name, props)| (OwnedInterfaceName::try_from(*name).unwrap(), props.iter().map(|(k, v)| (k.to_string(), v.try_clone().unwrap())).collect()))
            .collect()
    }

    fn value<T: Into<Value<'static>>>(v: T) -> OwnedValue {
        OwnedValue::try_from(v.into()).unwrap()
    }

    fn path(p: &str) -> OwnedObjectPath {
        OwnedObjectPath::try_from(p).unwrap()
    }

    const USB: &str = "/org/freedesktop/UDisks2/drives/SanDisk_Ultra_1";

    fn objects(mounted: bool) -> ManagedObjects {
        let points: Vec<Vec<u8>> = if mounted { vec![b"/run/media/me/USB\0".to_vec()] } else { vec![] };
        [
            (path(USB), object(&[(DRIVE, vec![("Vendor", value("SanDisk ")), ("Model", value("Ultra")), ("Removable", value(true))])])),
            (
                path("/org/freedesktop/UDisks2/block_devices/sda1"),
                object(&[
                    (BLOCK, vec![("Drive", value(ObjectPath::try_from(USB).unwrap())), ("IdLabel", value("")), ("HintAuto", value(true)), ("HintSystem", value(false))]),
                    (FILESYSTEM, vec![("MountPoints", value(points))]),
                ]),
            ),
            // The system disk: hidden.
            (
                path("/org/freedesktop/UDisks2/block_devices/nvme0n1p2"),
                object(&[(BLOCK, vec![("HintSystem", value(true))]), (FILESYSTEM, vec![("MountPoints", value(vec![b"/\0".to_vec()]))])]),
            ),
            // A partition table, no filesystem: hidden.
            (path("/org/freedesktop/UDisks2/block_devices/sda"), object(&[(BLOCK, vec![("Drive", value(ObjectPath::try_from(USB).unwrap()))])])),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn removable_filesystems_only() {
        let volume = |mounts: Vec<String>| Volume {
            block: "/org/freedesktop/UDisks2/block_devices/sda1".into(),
            drive: USB.into(),
            name: "SanDisk Ultra".into(),
            mounts,
            auto: true,
        };
        assert_eq!(summarize(&objects(false)), [volume(vec![])]);
        assert_eq!(summarize(&objects(true)), [volume(vec!["/run/media/me/USB".into()])]);
    }

    #[test]
    fn label_wins() {
        let mut objects = objects(false);
        let block = objects.get_mut(&path("/org/freedesktop/UDisks2/block_devices/sda1")).unwrap();
        block.get_mut(&OwnedInterfaceName::try_from(BLOCK).unwrap()).unwrap().insert("IdLabel".into(), value("PHOTOS"));
        assert_eq!(summarize(&objects)[0].name, "PHOTOS");
    }
}
