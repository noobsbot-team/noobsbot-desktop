//! The desktop's own bus name (`org.noobsbot.Desktop`): `Shortcuts.Record` lets Settings capture keys the desktop
//! would otherwise take (E8.7); `Displays` lets the Displays page set the brightness and identify the screens;
//! `Update` is Settings' Software Update (Check Now, Update Now, Revert).

use smithay::reexports::calloop::channel::Sender;
use zbus::{
    Connection,
    fdo::{RequestNameFlags, RequestNameReply},
};

use super::Update;

const NAME: &str = "org.noobsbot.Desktop";
const PATH: &str = "/org/noobsbot/Desktop";

struct Server {
    updates: Sender<Update>,
}

#[zbus::interface(name = "org.noobsbot.Desktop.Shortcuts")]
impl Server {
    /// On: no shortcut matches and every key goes to the focused window, until focus moves or 15 s pass.
    fn record(&self, on: bool) {
        let _ = self.updates.send(Update::Record(on));
    }
}

struct Displays {
    updates: Sender<Update>,
}

#[zbus::interface(name = "org.noobsbot.Desktop.Displays")]
impl Displays {
    /// The built-in panel's brightness, in percent (through logind, as the brightness keys do).
    fn set_brightness(&self, percent: u8) {
        let _ = self.updates.send(Update::SetBrightness(percent));
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
    fn install(&self) {
        let _ = self.updates.send(Update::InstallNow);
    }

    /// Points the installed version back at the previous one (within 30 days of an update), then the log-out alert.
    fn revert(&self) {
        let _ = self.updates.send(Update::RevertVersion);
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
    {
        return tracing::warn!("shortcut recording off: {err}");
    }
    match bus.request_name_with_flags(NAME, RequestNameFlags::DoNotQueue.into()).await {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {}
        Ok(reply) => tracing::info!("another desktop owns {NAME} ({reply:?}); shortcut recording off"),
        Err(err) => tracing::warn!("shortcut recording off: {err}"),
    }
}
