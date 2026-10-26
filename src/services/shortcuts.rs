//! The desktop's own bus name (`org.noobsbot.Desktop`): `Shortcuts.Record` lets Settings capture keys the desktop
//! would otherwise take (E8.7).

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

/// Serves the interface; a second desktop on the same bus (nested in a session of ours) leaves the name to the first.
pub async fn serve(bus: &Connection, updates: &Sender<Update>) {
    if let Err(err) = bus.object_server().at(PATH, Server { updates: updates.clone() }).await {
        return tracing::warn!("shortcut recording off: {err}");
    }
    match bus.request_name_with_flags(NAME, RequestNameFlags::DoNotQueue.into()).await {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {}
        Ok(reply) => tracing::info!("another desktop owns {NAME} ({reply:?}); shortcut recording off"),
        Err(err) => tracing::warn!("shortcut recording off: {err}"),
    }
}
