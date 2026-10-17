//! System services over D-Bus (C9, E2.3). One `services` thread runs zbus and every client; the frame loop only sees
//! two channels, so a stuck service can never freeze the screen (E1.11). A missing service is a state, not an error:
//! its client reports `None` and picks it up again when it appears.

pub mod bluetooth;
pub mod network;
pub mod power;
pub mod profiles;

use std::{fmt::Debug, future::Future, os::unix::net::UnixStream, time::Duration};

use futures_lite::{StreamExt, future};
use smithay::reexports::calloop::channel::Sender;
use zbus::{Connection, MatchRule, MessageStream, fdo, message};

/// Every D-Bus call gives up after this long (E1.11).
const TIMEOUT: Duration = Duration::from_secs(2);

/// Service state for the frame loop.
#[derive(Debug)]
pub enum Update {
    /// `None`: no battery, or UPower isn't running.
    Battery(Option<power::Battery>),
    /// `None`: NetworkManager isn't running.
    Network(Option<network::Network>),
    /// `None`: no adapter, or BlueZ isn't running.
    Bluetooth(Option<bluetooth::Bluetooth>),
    /// `None`: power-profiles-daemon isn't running.
    Profiles(Option<profiles::Profiles>),
}

/// Requests from the frame loop.
#[derive(Debug)]
pub enum Command {
    /// An IANA zone such as `Asia/Kathmandu` (E2.6).
    SetTimezone(String),
    #[expect(dead_code, reason = "the Control Center (E2.3) waits for the M2 mockup approval")]
    SetPowerProfile(String),
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
            // Every service state starts as `None` (hidden) in the frame loop, so there is nothing to send.
            return tracing::warn!("system bus unavailable, system status hidden: {err}");
        }
    };
    let clients = future::zip(
        future::zip(power::watch(&bus, &updates), profiles::watch(&bus, &updates)),
        future::zip(network::watch(&bus, &updates), bluetooth::watch(&bus, &updates)),
    );
    future::zip(clients, handle(&bus, commands)).await;
}

/// Connects on this thread. zbus's own connect runs on the `blocking` crate's pool, whose idle thread (1.7.0) wakes
/// every 500 ms for the life of the process, which would break the zero-idle-wakeup budget (Part I).
fn system_socket() -> std::io::Result<UnixStream> {
    let address = std::env::var("DBUS_SYSTEM_BUS_ADDRESS").unwrap_or_default();
    let path = address.strip_prefix("unix:path=").map_or("/run/dbus/system_bus_socket", |rest| rest.split(',').next().unwrap_or(rest));
    UnixStream::connect(path)
}

/// A client's whole life: `read` now, and again after every signal from `service` under `path` (properties, objects
/// added or removed) and every change of its owner, so a service that starts later or restarts is picked up. Only a
/// changed result reaches the frame loop.
async fn follow<'a, T, F>(bus: &'a Connection, service: &str, path: &str, updates: &Sender<Update>, wrap: fn(Option<T>) -> Update, read: impl Fn(&'a Connection) -> F)
where
    T: Clone + Debug + PartialEq,
    F: Future<Output = Option<T>>,
{
    let rule = MatchRule::builder().msg_type(message::Type::Signal).sender(service).and_then(|r| r.path_namespace(path)).map(|r| r.build());
    let streams = async {
        let changes = MessageStream::for_match_rule(rule?, bus, Some(16)).await?;
        let owners = fdo::DBusProxy::new(bus).await?.receive_name_owner_changed_with_args(&[(0, service)]).await?;
        zbus::Result::Ok(owners.map(drop).or(changes.map(drop)))
    };
    let mut events = match streams.await {
        Ok(events) => events,
        Err(err) => {
            tracing::warn!("{service} off: {err}");
            let _ = updates.send(wrap(None));
            return;
        }
    };
    let mut last = None;
    loop {
        let state = read(bus).await;
        if state != last {
            tracing::debug!(?state, "{service}");
            if updates.send(wrap(state.clone())).is_err() {
                return;
            }
            last = state;
        }
        if events.next().await.is_none() {
            return;
        }
    }
}

async fn handle(bus: &Connection, commands: async_channel::Receiver<Command>) {
    while let Ok(command) = commands.recv().await {
        match command {
            Command::SetTimezone(zone) => set_timezone(bus, &zone).await,
            Command::SetPowerProfile(profile) => profiles::set(bus, &profile).await,
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
