//! USB drives (E4.6): plugged-in filesystems mount by themselves, a notification offers Open (Dolphin) and Eject,
//! and another says when the drive can be pulled out. Dolphin lists mounted drives in its sidebar itself.

use crate::{
    services::{Command, disks::Volume, notifications::Notification},
    state::State,
};

/// Our notifications' ids, far above the notification server's counter.
const FIRST_ID: u32 = 0xFFFF_0000;

/// The frame loop's view of the drives.
#[derive(Default)]
pub struct Disks {
    /// `None` until UDisks2 first answers: what is plugged in at start raises nothing.
    volumes: Option<Vec<Volume>>,
    /// Drives we asked to eject, so one that vanishes at once still gets its "can be removed".
    ejecting: Vec<String>,
    /// Shown notifications' volumes, for their buttons.
    shown: Vec<(u32, Volume)>,
    next: u32,
}

#[derive(Debug, PartialEq)]
enum Event<'a> {
    Mount(&'a Volume),
    Connected(&'a Volume),
    /// A drive's name.
    Removable(String),
}

/// What changed from `old` to `new`.
// ponytail: a drive pulled out still mounted, whose objects UDisks2 drops after first unmounting, reads as
// "can be removed"; tell the two apart by the drive's own InterfacesRemoved if that ever shows.
fn events<'a>(old: &[Volume], new: &'a [Volume], ejecting: &[String]) -> Vec<Event<'a>> {
    let before = |block: &str| old.iter().find(|v| v.block == block);
    let mut events = Vec::new();
    for v in new {
        match before(&v.block) {
            None if v.auto && v.mounts.is_empty() => events.push(Event::Mount(v)),
            prior if !v.mounts.is_empty() && prior.is_none_or(|p| p.mounts.is_empty()) => events.push(Event::Connected(v)),
            _ => {}
        }
    }
    let mut drives: Vec<&Volume> = old.iter().filter(|v| v.drive != "/" && !v.mounts.is_empty()).collect();
    drives.sort_by(|a, b| a.drive.cmp(&b.drive));
    drives.dedup_by(|a, b| a.drive == b.drive);
    for v in drives {
        let now: Vec<_> = new.iter().filter(|n| n.drive == v.drive).collect();
        let unmounted = now.iter().all(|n| n.mounts.is_empty());
        if (!now.is_empty() && unmounted) || (now.is_empty() && ejecting.contains(&v.drive)) {
            events.push(Event::Removable(v.name.clone()));
        }
    }
    events
}

impl State {
    /// UDisks2's new list. Mounting is left to the host desktop when nested, so testing never touches real drives.
    pub fn disks_changed(&mut self, new: Vec<Volume>) {
        let Some(old) = self.disks.volumes.take() else {
            self.disks.volumes = Some(new);
            return;
        };
        for event in events(&old, &new, &self.disks.ejecting) {
            match event {
                Event::Mount(v) if !self.nested => self.services.send(Command::Mount { block: v.block.clone(), name: v.name.clone() }),
                Event::Mount(_) => {}
                Event::Connected(v) => {
                    let actions = [("default", "Open"), ("open", "Open"), ("eject", "Eject")].map(|(k, l)| (k.to_owned(), l.to_owned()));
                    self.disk_notice(format!("{} connected", v.name), String::new(), actions.to_vec(), Some(v.clone()));
                }
                Event::Removable(name) => self.disk_notice(format!("{name} can be removed"), "It's safe to unplug it now.".into(), vec![], None),
            }
        }
        self.disks.ejecting.retain(|d| new.iter().any(|v| v.drive == *d && !v.mounts.is_empty()));
        self.disks.shown.retain(|(id, _)| self.notifications.get(*id).is_some());
        self.disks.volumes = Some(new);
    }

    pub fn disk_failed(&mut self, name: String, error: String) {
        self.disk_notice(format!("Couldn't use {name}"), error, vec![], None);
    }

    fn disk_notice(&mut self, summary: String, body: String, actions: Vec<(String, String)>, volume: Option<Volume>) {
        let id = FIRST_ID + self.disks.next % 0xFFFF;
        self.disks.next += 1;
        tracing::debug!(id, summary, "disk notice");
        if let Some(v) = volume {
            self.disks.shown.push((id, v));
        }
        self.notify(notice(id, summary, body, actions));
    }

    /// A button on one of our notifications. `false`: not ours.
    pub fn disk_action(&mut self, id: u32, key: &str) -> bool {
        if id < FIRST_ID {
            return false;
        }
        let Some((_, v)) = self.disks.shown.iter().find(|(i, _)| *i == id).cloned() else { return true };
        // The volume as it is now: it may have been unmounted meanwhile.
        let now = self.disks.volumes.iter().flatten().find(|n| n.block == v.block).cloned().unwrap_or(v);
        match key {
            "eject" => {
                let blocks = self.disks.volumes.iter().flatten().filter(|n| n.drive == now.drive && !n.mounts.is_empty()).map(|n| n.block.clone()).collect();
                self.disks.ejecting.push(now.drive.clone());
                self.services.send(Command::Eject { drive: now.drive, blocks, name: now.name });
            }
            _ => {
                if let Some(mount) = now.mounts.first() {
                    self.spawn(&["dolphin".into(), mount.clone()]);
                }
            }
        }
        true
    }
}

fn notice(id: u32, summary: String, body: String, actions: Vec<(String, String)>) -> Notification {
    let icon = "drive-removable-media-usb";
    Notification {
        id,
        app: "Files".into(),
        icon_name: icon.into(),
        icon: crate::services::notifications::load_icon(icon).map(std::sync::Arc::new),
        summary,
        body,
        actions,
        urgency: 1,
        expire: -1,
        transient: true,
        time: chrono::Local::now().timestamp(),
        ..Default::default()
    }
}

/// `--show usb`: the "connected" banner, hovered so its buttons show.
pub fn sample(state: &mut State) {
    let v = Volume { block: "/sample".into(), drive: "/sample".into(), name: "SanDisk Ultra".into(), mounts: vec!["/run/media/sample".into()], auto: true };
    state.disks.volumes = Some(vec![]);
    state.disks_changed(vec![v]);
    // Kept up for the screenshot.
    if let Some(mut n) = state.notifications.banners.first().and_then(|id| state.notifications.get(*id)).cloned() {
        n.expire = 0;
        state.notifications.hovered = Some(n.id);
        state.notify(n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(block: &str, drive: &str, mounted: bool) -> Volume {
        Volume {
            block: block.into(),
            drive: drive.into(),
            name: drive.into(),
            mounts: if mounted { vec![format!("/run/media/{block}")] } else { vec![] },
            auto: true,
        }
    }

    #[test]
    fn plug_mount_eject() {
        let plugged = [volume("sdb1", "usb", false)];
        assert_eq!(events(&[], &plugged, &[]), [Event::Mount(&plugged[0])]);
        let mounted = [volume("sdb1", "usb", true)];
        assert_eq!(events(&plugged, &mounted, &[]), [Event::Connected(&mounted[0])]);
        // Unmounted (by Dolphin, say), the drive still there.
        assert_eq!(events(&mounted, &plugged, &[]), [Event::Removable("usb".into())]);
        // Unmounted and powered off before we looked: only when we ejected it.
        assert_eq!(events(&mounted, &[], &["usb".into()]), [Event::Removable("usb".into())]);
        assert_eq!(events(&mounted, &[], &[]), []);
    }

    #[test]
    fn quiet_cases() {
        // Not meant to mount itself (UDisks2's HintAuto off).
        let manual = [Volume { auto: false, ..volume("sdb1", "usb", false) }];
        assert_eq!(events(&[], &manual, &[]), []);
        // One of two partitions unmounted: the drive isn't free yet.
        let both = [volume("sdb1", "usb", true), volume("sdb2", "usb", true)];
        let one = [volume("sdb1", "usb", false), volume("sdb2", "usb", true)];
        assert_eq!(events(&both, &one, &[]), []);
        assert_eq!(events(&both, &both, &[]), []);
    }
}
