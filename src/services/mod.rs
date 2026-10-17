//! System services over D-Bus (C9, E2.3). One `services` thread runs zbus and every client; the frame loop only sees
//! two channels, so a stuck service can never freeze the screen (E1.11). A missing service is a state, not an error:
//! its client reports `None` and picks it up again when it appears.

pub mod bluetooth;
pub mod brightness;
pub mod media;
pub mod network;
pub mod power;
pub mod profiles;
pub mod sound;

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
    /// Percent; `None` without a backlight (desktop monitors).
    Brightness(Option<u8>),
    /// `None`: no output device, or PipeWire isn't running.
    Sound(Option<sound::Sound>),
    /// `None`: nothing playing or paused.
    Media(Option<media::Media>),
}

/// Requests from the frame loop.
#[derive(Debug)]
pub enum Command {
    /// An IANA zone such as `Asia/Kathmandu` (E2.6).
    Timezone(String),
    #[expect(dead_code, reason = "the Control Center (E2.3) waits for the M2 mockup approval")]
    PowerProfile(String),
    /// Percent.
    Brightness(u8),
    /// Percent of the default output (`true`) or input.
    Volume { output: bool, percent: u8 },
    Mute { output: bool, muted: bool },
    /// The sound picker: a `sound::Output` id.
    #[expect(dead_code, reason = "the Control Center (E2.3) waits for the M2 mockup approval")]
    Output(u32),
    /// To the player `Update::Media` named.
    Media { player: String, action: media::Action },
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
    sound::spawn(updates.clone());
    let thread = std::thread::Builder::new().name("services".into()).spawn(move || future::block_on(run(updates, rx)));
    if let Err(err) = thread {
        tracing::warn!("system services off: {err}");
    }
    Services { commands }
}

async fn run(updates: Sender<Update>, commands: async_channel::Receiver<Command>) {
    // A missing bus leaves its states at `None` (hidden) in the frame loop, so there is nothing to send.
    let system = connect("DBUS_SYSTEM_BUS_ADDRESS", "/run/dbus/system_bus_socket".into())
        .await
        .inspect_err(|err| tracing::warn!("system bus unavailable, system status hidden: {err}"))
        .ok();
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let session = connect("DBUS_SESSION_BUS_ADDRESS", format!("{runtime}/bus"))
        .await
        .inspect_err(|err| tracing::warn!("session bus unavailable, Now Playing hidden: {err}"))
        .ok();
    brightness::send_current(&updates);
    let system_clients = async {
        if let Some(bus) = &system {
            future::zip(
                future::zip(power::watch(bus, &updates), profiles::watch(bus, &updates)),
                future::zip(network::watch(bus, &updates), bluetooth::watch(bus, &updates)),
            )
            .await;
        }
    };
    let session_clients = async {
        if let Some(bus) = &session {
            media::watch(bus, &updates).await;
        }
    };
    let clients = future::zip(system_clients, session_clients);
    future::zip(clients, handle(system.as_ref(), session.as_ref(), commands, &updates)).await;
}

/// Connects on this thread. zbus's own connect runs on the `blocking` crate's pool, whose idle thread (1.7.0) wakes
/// every 500 ms for the life of the process, which would break the zero-idle-wakeup budget (Part I).
async fn connect(variable: &str, fallback: String) -> zbus::Result<Connection> {
    let address = std::env::var(variable).unwrap_or_default();
    let path = address.strip_prefix("unix:path=").map_or(fallback.as_str(), |rest| rest.split(',').next().unwrap_or(rest));
    let socket = UnixStream::connect(path)?;
    zbus::connection::Builder::async_io_unix_stream(socket).method_timeout(TIMEOUT).build().await
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
    match streams.await {
        Ok(events) => relay(bus, events, service, updates, wrap, read).await,
        Err(err) => tracing::warn!("{service} off: {err}"),
    }
}

/// `read` now and after every event; only a changed result reaches the frame loop.
async fn relay<'a, T, F>(
    bus: &'a Connection,
    mut events: impl futures_lite::Stream + Unpin,
    name: &str,
    updates: &Sender<Update>,
    wrap: fn(Option<T>) -> Update,
    read: impl Fn(&'a Connection) -> F,
) where
    T: Clone + Debug + PartialEq,
    F: Future<Output = Option<T>>,
{
    let mut last = None;
    loop {
        let state = read(bus).await;
        if state != last {
            tracing::debug!(?state, "{name}");
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

async fn handle(system: Option<&Connection>, session: Option<&Connection>, commands: async_channel::Receiver<Command>, updates: &Sender<Update>) {
    while let Ok(command) = commands.recv().await {
        match (command, system, session) {
            (Command::Timezone(zone), Some(bus), _) => set_timezone(bus, &zone).await,
            (Command::PowerProfile(profile), Some(bus), _) => profiles::set(bus, &profile).await,
            (Command::Brightness(percent), Some(bus), _) => brightness::set(bus, percent, updates).await,
            (Command::Media { player, action }, _, Some(bus)) => media::control(bus, &player, action).await,
            // wpctl takes ~10 ms; blocking this thread that long is fine (it never blocks the frame loop).
            (Command::Volume { output, percent }, ..) => sound::set_volume(output, percent),
            (Command::Mute { output, muted }, ..) => sound::set_muted(output, muted),
            (Command::Output(id), ..) => sound::set_output(id),
            // Its bus is down, which `run` has logged.
            _ => {}
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
