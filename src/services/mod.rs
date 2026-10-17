//! System services over D-Bus (C9, E2.3). One `services` thread runs zbus and every client; the frame loop only sees
//! two channels, so a stuck service can never freeze the screen (E1.11). A missing service is a state, not an error:
//! its client reports `None` and picks it up again when it appears.

pub mod network;
pub mod power;

use std::{os::unix::net::UnixStream, time::Duration};

use futures_lite::future;
use smithay::reexports::calloop::channel::Sender;
use zbus::Connection;

/// Every D-Bus call gives up after this long (E1.11).
const TIMEOUT: Duration = Duration::from_secs(2);

/// Service state for the frame loop.
#[derive(Debug)]
pub enum Update {
    /// `None`: no battery, or UPower isn't running.
    Battery(Option<power::Battery>),
    /// `None`: NetworkManager isn't running.
    Network(Option<network::Network>),
}

/// Requests from the frame loop.
#[derive(Debug)]
pub enum Command {
    /// An IANA zone such as `Asia/Kathmandu` (E2.6).
    SetTimezone(String),
}

pub struct Services {
    commands: async_channel::Sender<Command>,
}

impl Services {
    pub fn send(&self, command: Command) {
        // Unbounded; fails only once the thread has gone, which it has already logged.
        let _ = self.commands.try_send(command);
    }
}

pub fn spawn(updates: Sender<Update>) -> Services {
    let (commands, rx) = async_channel::unbounded();
    let thread = std::thread::Builder::new().name("services".into()).spawn(move || future::block_on(run(updates, rx)));
    if let Err(err) = thread {
        tracing::warn!("system services off: {err}");
    }
    Services { commands }
}

async fn run(updates: Sender<Update>, commands: async_channel::Receiver<Command>) {
    let bus = match system_socket() {
        Ok(socket) => zbus::connection::Builder::async_io_unix_stream(socket).method_timeout(TIMEOUT).build().await,
        Err(err) => Err(err.into()),
    };
    let bus = match bus {
        Ok(bus) => bus,
        Err(err) => {
            tracing::warn!("system bus unavailable, system status hidden: {err}");
            let _ = updates.send(Update::Battery(None));
            let _ = updates.send(Update::Network(None));
            return;
        }
    };
    future::zip(future::zip(power::watch(&bus, &updates), network::watch(&bus, &updates)), handle(&bus, commands)).await;
}

/// Connects on this thread. zbus's own connect runs on the `blocking` crate's pool, whose idle thread (1.7.0) wakes
/// every 500 ms for the life of the process, which would break the zero-idle-wakeup budget (Part I).
fn system_socket() -> std::io::Result<UnixStream> {
    let address = std::env::var("DBUS_SYSTEM_BUS_ADDRESS").unwrap_or_default();
    let path = address.strip_prefix("unix:path=").map_or("/run/dbus/system_bus_socket", |rest| rest.split(',').next().unwrap_or(rest));
    UnixStream::connect(path)
}

async fn handle(bus: &Connection, commands: async_channel::Receiver<Command>) {
    while let Ok(command) = commands.recv().await {
        match command {
            Command::SetTimezone(zone) => set_timezone(bus, &zone).await,
        }
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.timedate1",
    default_service = "org.freedesktop.timedate1",
    default_path = "/org/freedesktop/timedate1"
)]
trait Timedate {
    fn set_timezone(&self, timezone: &str, interactive: bool) -> zbus::Result<()>;
}

/// systemd-timedated replaces `/etc/localtime`; chrono's `Local` notices the change, so the clock follows by itself.
/// Setting the zone it already has is a no-op there, before any permission check.
async fn set_timezone(bus: &Connection, zone: &str) {
    let result = match TimedateProxy::new(bus).await {
        Ok(timedate) => timedate.set_timezone(zone, true).await,
        Err(err) => Err(err),
    };
    match result {
        Ok(()) => tracing::info!("time zone set to {zone}"),
        // An authentication prompt outlives the timeout; timedated still applies the zone once it is answered.
        Err(err) => tracing::warn!("setting the time zone to {zone}: {err}"),
    }
}
