//! The desktop's own bus name (`org.noobsbot.Desktop`): `Shortcuts.Record` lets Settings capture keys the desktop
//! would otherwise take (E8.7); `Displays` lets the Displays page set each display's brightness and identify the
//! screens; `Update` is Settings' Software Update (Check Now, Update Now, Revert); `Calendar.Fetch` fetches CalDAV now.

use std::{
    ffi::OsStr,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use smithay::reexports::calloop::channel::Sender;
use zbus::{
    Connection, fdo,
    fdo::{RequestNameFlags, RequestNameReply},
    message::Header,
};

use super::Update;

const NAME: &str = "org.noobsbot.Desktop";
const PATH: &str = "/org/noobsbot/Desktop";

/// The caller is our Settings app, the binary beside ours: Record turns every shortcut off (Lock among them), and
/// Install and Revert run the root helper, which asks no password again for a while.
// ponytail: by the caller's executable path; a same-user process can still start Settings with LD_PRELOAD.
async fn settings_only(bus: &Connection, header: &Header<'_>) -> fdo::Result<()> {
    let denied = || fdo::Error::AccessDenied("only System Settings may call this".into());
    let sender = header.sender().ok_or_else(denied)?;
    let pid = fdo::DBusProxy::new(bus).await?.get_connection_unix_process_id(sender.clone().into()).await?;
    let caller = std::fs::read_link(format!("/proc/{pid}/exe")).map_err(|_| denied())?;
    let ours = std::env::current_exe().map_err(|_| denied())?;
    is_settings(&caller, &ours).then_some(()).ok_or_else(denied)
}

/// Whether `caller` is the Settings binary beside `ours`, either maybe replaced on disk since (`/proc`'s " (deleted)").
fn is_settings(caller: &Path, ours: &Path) -> bool {
    let live = |p: &Path| -> PathBuf {
        let bytes = p.as_os_str().as_bytes();
        Path::new(OsStr::from_bytes(bytes.strip_suffix(b" (deleted)").unwrap_or(bytes))).to_owned()
    };
    live(caller) == live(ours).with_file_name("noobsbot-desktop-settings")
}

struct Server {
    updates: Sender<Update>,
}

#[zbus::interface(name = "org.noobsbot.Desktop.Shortcuts")]
impl Server {
    /// On: no shortcut matches and every key goes to the focused window, until focus moves or 15 s pass.
    async fn record(&self, on: bool, #[zbus(header)] header: Header<'_>, #[zbus(connection)] bus: &Connection) -> fdo::Result<()> {
        settings_only(bus, &header).await?;
        let _ = self.updates.send(Update::Record(on));
        Ok(())
    }
}

struct Displays {
    updates: Sender<Update>,
}

#[zbus::interface(name = "org.noobsbot.Desktop.Displays")]
impl Displays {
    /// Every display's brightness, in percent, as the brightness keys set it.
    fn set_brightness(&self, percent: u8) {
        let _ = self.updates.send(Update::SetBrightness(None, percent));
    }

    /// One display's brightness, in percent, by connector (`eDP-1`, `DP-1`): the built-in panel's backlight or a
    /// DDC/CI monitor. Each display's level is in `displays.json`.
    fn set_output_brightness(&self, connector: String, percent: u8) {
        let _ = self.updates.send(Update::SetBrightness(Some(connector), percent));
    }

    /// Every display shows its name for 2 s.
    fn identify(&self) {
        let _ = self.updates.send(Update::Identify);
    }
}

struct Updates {
    updates: Sender<Update>,
}

#[zbus::interface(name = "org.noobsbot.Desktop.Update")]
impl Updates {
    /// Checks for a newer release now; the result goes to `$XDG_STATE_HOME/noobsbot-desktop/update.json`.
    fn check(&self) {
        let _ = self.updates.send(Update::CheckNow);
    }

    /// "Restart to Update": installs the verified download (the admin password is asked), then the log-out alert.
    async fn install(&self, #[zbus(header)] header: Header<'_>, #[zbus(connection)] bus: &Connection) -> fdo::Result<()> {
        settings_only(bus, &header).await?;
        let _ = self.updates.send(Update::InstallNow);
        Ok(())
    }

    /// Points the installed version back at the previous one (within 30 days of an update), then the log-out alert.
    async fn revert(&self, #[zbus(header)] header: Header<'_>, #[zbus(connection)] bus: &Connection) -> fdo::Result<()> {
        settings_only(bus, &header).await?;
        let _ = self.updates.send(Update::RevertVersion);
        Ok(())
    }
}

struct Calendar {
    updates: Sender<Update>,
}

#[zbus::interface(name = "org.noobsbot.Desktop.Calendar")]
impl Calendar {
    /// Fetches the CalDAV calendars now (an account's password changed).
    fn fetch(&self) {
        let _ = self.updates.send(Update::CalDavNow);
    }
}

/// Serves the interfaces; a second desktop on the same bus (nested in a session of ours) leaves the name to the first.
pub async fn serve(bus: &Connection, updates: &Sender<Update>) {
    let server = bus.object_server();
    if let Err(err) = server
        .at(PATH, Server { updates: updates.clone() })
        .await
        .and(server.at(PATH, Displays { updates: updates.clone() }).await)
        .and(server.at(PATH, Updates { updates: updates.clone() }).await)
        .and(server.at(PATH, Calendar { updates: updates.clone() }).await)
    {
        return tracing::warn!("shortcut recording off: {err}");
    }
    match bus.request_name_with_flags(NAME, RequestNameFlags::DoNotQueue.into()).await {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {}
        Ok(reply) => tracing::info!("another desktop owns {NAME} ({reply:?}); shortcut recording off"),
        Err(err) => tracing::warn!("shortcut recording off: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_settings_binary_beside_ours_counts() {
        let ours = Path::new("/usr/lib/noobsbot-desktop/versions/1.0.0/noobsbot-desktop");
        assert!(is_settings(Path::new("/usr/lib/noobsbot-desktop/versions/1.0.0/noobsbot-desktop-settings"), ours));
        assert!(is_settings(Path::new("/x/target/debug/noobsbot-desktop-settings (deleted)"), Path::new("/x/target/debug/noobsbot-desktop (deleted)")));
        assert!(!is_settings(Path::new("/usr/lib/noobsbot-desktop/versions/0.9.0/noobsbot-desktop-settings"), ours));
        assert!(!is_settings(Path::new("/tmp/noobsbot-desktop-settings"), ours));
        assert!(!is_settings(Path::new("/usr/bin/busctl"), ours));
    }
}
