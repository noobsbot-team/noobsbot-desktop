//! System services over D-Bus (C9, E2.3). One `services` thread runs zbus and every client; the frame loop only sees
//! two channels, so a stuck service can never freeze the screen (E1.11). A missing service is a state, not an error:
//! its client reports `None` and picks it up again when it appears.

pub mod a11y;
pub mod bluetooth;
pub mod bolt;
pub mod brightness;
pub mod disks;
pub mod fcitx;
pub mod launcher;
pub mod login;
pub mod media;
pub mod network;
pub mod notifications;
pub mod polkit;
pub mod power;
pub mod profiles;
pub mod shortcuts;
pub mod sound;
pub mod tray;
pub mod update;
pub mod verify;
pub mod weather;
pub mod widgets;

use std::{
    fmt::Debug,
    future::Future,
    os::unix::net::UnixStream,
    sync::{Arc, Mutex},
    time::Duration,
};

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
    /// Peripherals' batteries; `None`: UPower isn't running.
    Peripherals(Option<Vec<power::Peripheral>>),
    /// `None`: NetworkManager isn't running.
    Network(Option<network::Network>),
    /// `None`: no adapter, or BlueZ isn't running.
    Bluetooth(Option<bluetooth::Bluetooth>),
    /// `None`: power-profiles-daemon isn't running.
    Profiles(Option<profiles::Profiles>),
    /// Percent; `None` without a backlight or DDC/CI monitor.
    Brightness(Option<u8>),
    /// `None`: no output device, or PipeWire isn't running.
    Sound(Option<sound::Sound>),
    /// `None`: nothing playing or paused.
    Media(Option<media::Media>),
    /// A device asks to pair; `None`: BlueZ gave up.
    Pairing(Option<bluetooth::Pairing>),
    /// NetworkManager asks for this network's password (or WEP key, or login); `None`: it gave up.
    WifiSecret(Option<(String, network::Secret)>),
    /// A pairing device needs a PIN (or, `passkey`, six digits) typed for it.
    PinEntry { device: String, passkey: bool },
    /// Our polkit agent asks for, rejects or is done with an admin password.
    Polkit(polkit::Event),
    /// Shown tray items, left to right.
    Tray(Vec<tray::TrayItem>),
    /// The rows of a tray item's menu, after `Command::TrayMenu`.
    TrayMenu { service: String, menu: String, parent: i32, rows: Vec<Option<tray::MenuRow>> },
    /// A new notification, or one replacing the notification with its id.
    Notify(Box<notifications::Notification>),
    /// An app's `CloseNotification`.
    CloseNotification(u32),
    /// The saved history, once at start (oldest first).
    History(Vec<notifications::Notification>),
    /// Plug-in filesystems (E4.6); empty without UDisks2.
    Disks(Vec<disks::Volume>),
    /// Mounting or ejecting `name` failed.
    DiskFailed { name: String, error: String },
    /// An app's dock badge, progress and urgency (LauncherEntry), by desktop-file id.
    Launcher { app: String, entry: launcher::Entry },
    /// logind is about to sleep: lock, then answer `Command::SleepReady` (E6.5).
    PrepareForSleep,
    /// Awake again.
    Resumed,
    /// fcitx5's (or ibus's) input methods; `None`: it isn't running.
    Fcitx(Option<fcitx::Fcitx>),
    /// Settings starts or stops recording a shortcut (`org.noobsbot.Desktop.Shortcuts.Record`).
    Record(bool),
    /// Settings' Displays page moved the brightness slider (`org.noobsbot.Desktop.Displays.SetBrightness`), in percent.
    SetBrightness(u8),
    /// Settings' Software Update ▸ Check Now (`org.noobsbot.Desktop.Update.Check`).
    CheckNow,
    /// Settings' Software Update ▸ Update Now (`org.noobsbot.Desktop.Update.Install`).
    InstallNow,
    /// Settings' Software Update ▸ Revert to (`org.noobsbot.Desktop.Update.Revert`).
    RevertVersion,
    /// Settings' Displays ▸ Identify (`org.noobsbot.Desktop.Displays.Identify`).
    Identify,
    /// The keyboard backlight after a key changed it, in percent.
    KbdBacklight(u8),
    /// Thunderbolt devices waiting to be allowed; `None`: boltd isn't running.
    Thunderbolt(Option<Vec<bolt::Accessory>>),
    /// logind's sleep blockers, after `Command::Inhibitors`.
    Inhibitors(Vec<login::Inhibitor>),
    /// Every widget other processes set (`org.noobsbot.Desktop.Widgets`), in the order they came.
    Widgets(Vec<widgets::Widget>),
}

/// Requests from the frame loop.
#[derive(Debug)]
pub enum Command {
    /// An IANA zone such as `Asia/Kathmandu` (E2.6).
    Timezone(String),
    PowerProfile(String),
    Wifi(bool),
    Bluetooth(bool),
    /// Connect (`true`) or disconnect a device by its BlueZ path.
    BluetoothDevice {
        path: String,
        connect: bool,
    },
    /// Ask the Wi-Fi devices for a fresh list of networks.
    WifiScan,
    /// Join a network: its saved connection, or a new one (with the password, when it needs one).
    WifiJoin {
        ssid: String,
        ap: String,
        device: String,
        password: Option<String>,
    },
    /// Bring a saved VPN up, or its active connection (`active`) down.
    Vpn {
        path: String,
        active: Option<String>,
    },
    /// Percent.
    Brightness(u8),
    /// The keyboard backlight one step up, down, or (`None`) off and on.
    KbdBacklight(Option<bool>),
    /// Idle: the keyboard backlight off (`true`), or back to the level it had.
    KbdBacklightIdle(bool),
    /// Percent of the default output (`true`) or input.
    Volume {
        output: bool,
        percent: u8,
    },
    /// An app's stream, by PipeWire node id.
    StreamVolume {
        id: u32,
        percent: u8,
    },
    Mute {
        output: bool,
        muted: bool,
    },
    /// The sound picker: a `sound::Output` id.
    Output(u32),
    /// To the player `Update::Media` named.
    Media {
        player: String,
        action: media::Action,
    },
    /// The pairing alert's answer.
    PairingAnswer(bool),
    /// The password, key or user name NetworkManager asked for, or `None`: cancelled.
    WifiSecret(Option<String>),
    /// The PIN or passkey typed for a pairing device, or `None`: cancelled.
    PinEntry(Option<String>),
    /// The polkit alert's (user, password), or `None`: cancelled.
    PolkitAnswer(polkit::Reply),
    /// A click on a tray item without a menu, at screen `x`, `y`.
    TrayActivate {
        service: String,
        path: String,
        x: i32,
        y: i32,
    },
    /// Fetch a tray item's dbusmenu, or (`parent` ≠ 0) one of its submenus.
    TrayMenu {
        service: String,
        menu: String,
        parent: i32,
    },
    /// A row of that menu was clicked.
    TrayClicked {
        service: String,
        menu: String,
        id: i32,
    },
    /// Logo menu ▸ Sleep.
    Suspend,
    /// The lock screen is shown: release the sleep inhibitor.
    SleepReady,
    /// After the Restart alert.
    Reboot,
    /// After the Shut Down alert.
    PowerOff,
    /// A notification went; tells its app.
    NotificationClosed {
        id: u32,
        reason: notifications::Reason,
    },
    /// A notification's button (or the click on it, key "default").
    NotificationAction {
        id: u32,
        key: String,
    },
    /// The history file's new text.
    SaveNotifications(String),
    /// Mount a `disks::Volume` (by its block path); `name` for the error notification.
    Mount {
        block: String,
        name: String,
    },
    /// Unmount `blocks`, then power `drive` off.
    Eject {
        drive: String,
        blocks: Vec<String>,
        name: String,
    },
    /// The input menu opened: read fcitx5's (or ibus's) methods afresh.
    ImeRead {
        ibus: bool,
    },
    /// Switch fcitx5 (or ibus) to this method (unique name).
    InputMethod {
        name: String,
        ibus: bool,
    },
    /// The Thunderbolt alert's Allow: enroll this boltd uid.
    Thunderbolt(String),
    /// Control Center opened: ask logind who blocks sleep.
    Inhibitors,
    /// A screen reader started or stopped: apps expose their accessibility trees while one runs.
    ScreenReader(bool),
    /// A widget was clicked: tell its owner.
    WidgetActivated {
        owner: String,
        id: String,
    },
}

pub struct Services {
    commands: async_channel::Sender<Command>,
    /// The screen reader's key monitor.
    pub keys: a11y::Keys,
}

impl Services {
    pub fn send(&self, command: Command) {
        // Unbounded; fails only once the thread has gone, which it has already logged.
        let _ = self.commands.try_send(command);
    }
}

/// `agents`: answer for the system (BlueZ pairing, Wi-Fi passwords, polkit). Off when nested, so the host desktop keeps its own agent.
pub fn spawn(updates: Sender<Update>, agents: bool) -> Services {
    let (commands, rx) = async_channel::unbounded();
    let (events, key_rx) = async_channel::unbounded();
    let keys = a11y::Keys { monitor: Arc::default(), events };
    let monitor = keys.monitor.clone();
    sound::spawn(updates.clone());
    let thread = std::thread::Builder::new().name("services".into()).spawn(move || future::block_on(run(updates, rx, agents, (monitor, key_rx))));
    if let Err(err) = thread {
        tracing::warn!("system services off: {err}");
    }
    Services { commands, keys }
}

type KeyMonitor = (Arc<Mutex<a11y::Monitor>>, async_channel::Receiver<(Vec<String>, a11y::Key)>);

async fn run(updates: Sender<Update>, commands: async_channel::Receiver<Command>, agents: bool, (monitor, keys): KeyMonitor) {
    // A missing bus leaves its states at `None` (hidden) in the frame loop, so there is nothing to send.
    let system = connect("DBUS_SYSTEM_BUS_ADDRESS", "/run/dbus/system_bus_socket".into())
        .await
        .inspect_err(|err| tracing::warn!("system bus unavailable, system status hidden: {err}"))
        .ok();
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let session = connect("DBUS_SESSION_BUS_ADDRESS", format!("{runtime}/bus"))
        .await
        .inspect_err(|err| tracing::warn!("session bus unavailable, Now Playing and tray hidden: {err}"))
        .ok();
    brightness::start(&updates);
    let (pairing, secret, entry, admin) = (Answer::default(), Answer::default(), Answer::default(), Answer::default());
    if let (true, Some(bus)) = (agents, &system)
        && !polkit::serve(bus, &updates, &admin).await
    {
        polkit::fallback();
    }
    // Inhibitors, Thunderbolt and the pairing and password agents only in a real session: nested, the host keeps its
    // own sleep, power key, docks and agents.
    let logind = async {
        if let (true, Some(bus)) = (agents, &system) {
            let agents = future::zip(bluetooth::serve_agent(bus, &updates, &pairing, &entry), network::serve_agent(bus, &updates, &secret));
            future::zip(future::zip(login::watch(bus, &updates), bolt::watch(bus, &updates)), agents).await;
        }
    };
    let system_clients = async {
        if let Some(bus) = &system {
            future::zip(
                future::zip(future::zip(power::watch(bus, &updates), power::watch_peripherals(bus, &updates)), profiles::watch(bus, &updates)),
                future::zip(future::zip(network::watch(bus, &updates), bluetooth::watch(bus, &updates)), disks::watch(bus, &updates)),
            )
            .await;
        }
    };
    let session_clients = async {
        if let Some(bus) = &session {
            let ours = future::zip(future::zip(notifications::serve(bus, &updates), launcher::watch(bus, &updates)), fcitx::watch(bus, &updates));
            let ours = future::zip(ours, future::zip(widgets::serve(bus, &updates), shortcuts::serve(bus, &updates)));
            let ours = future::zip(ours, a11y::serve(bus, monitor, keys));
            future::zip(future::zip(media::watch(bus, &updates), tray::watch(bus, &updates)), ours).await;
        }
    };
    let clients = future::zip(future::zip(system_clients, session_clients), logind);
    future::zip(clients, handle(system.as_ref(), session.as_ref(), commands, &updates, &pairing, &secret, &entry, &admin)).await;
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
type Interfaces = std::collections::HashMap<zbus::names::OwnedInterfaceName, std::collections::HashMap<String, zbus::zvariant::OwnedValue>>;

/// A property from a `GetManagedObjects` entry.
fn get<'a>(interfaces: &'a Interfaces, interface: &str, property: &str) -> Option<&'a zbus::zvariant::OwnedValue> {
    interfaces.iter().find(|(name, _)| name.as_str() == interface)?.1.get(property)
}

async fn follow<'a, T, F>(
    bus: &'a Connection,
    service: &str,
    path: &str,
    updates: &Sender<Update>,
    wrap: fn(Option<T>) -> Update,
    read: impl Fn(&'a Connection) -> F,
) where
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

/// Runs `register` now and again whenever `service` comes back (it restarted and forgot our agent), for good.
async fn keep_registered<F: Future<Output = zbus::Result<()>>>(bus: &Connection, service: &str, what: &str, register: impl Fn() -> F) {
    // Subscribed before the first registration, so a restart in between isn't missed.
    let owners = async { fdo::DBusProxy::new(bus).await?.receive_name_owner_changed_with_args(&[(0, service)]).await };
    let owners = owners.await.inspect_err(|err| tracing::warn!("{what}: not re-registered after a restart: {err}")).ok();
    if let Err(err) = register().await {
        tracing::warn!("{what} off: {err}");
    }
    let Some(owners) = owners else { return };
    let mut back = owners.filter(|change| change.args().is_ok_and(|a| a.new_owner.is_some()));
    while back.next().await.is_some() {
        match register().await {
            Ok(()) => tracing::info!("{what} registered again"),
            Err(err) => tracing::warn!("{what} off: {err}"),
        }
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

#[expect(clippy::too_many_arguments, reason = "one answer slot per agent")]
async fn handle(
    system: Option<&Connection>,
    session: Option<&Connection>,
    commands: async_channel::Receiver<Command>,
    updates: &Sender<Update>,
    pairing: &Answer<bool>,
    secret: &Answer<Option<String>>,
    entry: &Answer<Option<String>>,
    admin: &Answer<polkit::Reply>,
) {
    // The keyboard backlight's level before idleness turned it off.
    let mut kbd_saved = None;
    while let Ok(command) = commands.recv().await {
        let name = command_name(&command);
        match (command, system, session) {
            (Command::Timezone(zone), Some(bus), _) => set_timezone(bus, &zone).await,
            (Command::PowerProfile(profile), Some(bus), _) => profiles::set(bus, &profile).await,
            (Command::Brightness(percent), bus, _) => brightness::set(bus, percent, updates).await,
            (Command::KbdBacklight(change), Some(bus), _) => power::kbd_backlight(bus, change, updates).await,
            (Command::KbdBacklightIdle(off), Some(bus), _) => power::kbd_idle(bus, off, &mut kbd_saved).await,
            (Command::Media { player, action }, _, Some(bus)) => media::control(bus, &player, action).await,
            (Command::TrayActivate { service, path, x, y }, _, Some(bus)) => tray::activate(bus, &service, &path, x, y).await,
            (Command::TrayMenu { service, menu, parent }, _, Some(bus)) => tray::menu(bus, service, menu, parent, updates).await,
            (Command::TrayClicked { service, menu, id }, _, Some(bus)) => tray::clicked(bus, &service, &menu, id).await,
            (Command::Suspend | Command::Reboot | Command::PowerOff, Some(bus), _) => power(bus, &name).await,
            (Command::Wifi(on), Some(bus), _) => network::set_wifi(bus, on).await,
            (Command::Bluetooth(on), Some(bus), _) => bluetooth::set_powered(bus, on).await,
            (Command::BluetoothDevice { path, connect }, Some(bus), _) => bluetooth::connect(bus, &path, connect).await,
            (Command::WifiScan, Some(bus), _) => network::scan(bus).await,
            (Command::Vpn { path, active }, Some(bus), _) => network::vpn(bus, &path, active.as_deref()).await,
            (Command::Thunderbolt(uid), Some(bus), _) => bolt::enroll(bus, &uid).await,
            (Command::PairingAnswer(yes), ..) => pairing.give(yes),
            (Command::SleepReady, ..) => login::release_sleep(),
            (Command::WifiSecret(psk), ..) => secret.give(psk),
            (Command::PinEntry(typed), ..) => entry.give(typed),
            (Command::PolkitAnswer(reply), ..) => admin.give(reply),
            (Command::WifiJoin { ssid, ap, device, password }, Some(bus), _) => network::join(bus, &ssid, &ap, &device, password.as_deref()).await,
            (Command::Volume { output, percent }, ..) => sound::set(sound::Set::Volume { output, percent }),
            (Command::StreamVolume { id, percent }, ..) => sound::set(sound::Set::Stream { id, percent }),
            (Command::Mute { output, muted }, ..) => sound::set(sound::Set::Mute { output, muted }),
            (Command::Output(id), ..) => sound::set(sound::Set::Output(id)),
            (Command::NotificationClosed { id, reason }, _, Some(bus)) => notifications::closed(bus, id, reason).await,
            (Command::NotificationAction { id, key }, _, Some(bus)) => notifications::invoked(bus, id, &key).await,
            (Command::SaveNotifications(text), ..) => notifications::save_history(&text),
            (Command::Mount { block, name }, Some(bus), _) => disks::mount(bus, &block, name, updates).await,
            (Command::Eject { drive, blocks, name }, Some(bus), _) => disks::eject(bus, &drive, &blocks, name, updates).await,
            // The `ibus` CLI, blocking this thread (it never blocks the frame loop).
            (Command::ImeRead { ibus: true }, ..) => {
                let _ = updates.send(Update::Fcitx(fcitx::read_ibus()));
            }
            (Command::InputMethod { name, ibus: true }, ..) => {
                crate::ibus::run("ibus", &["engine", &name]);
                let _ = updates.send(Update::Fcitx(fcitx::read_ibus()));
            }
            (Command::ImeRead { ibus: false }, _, Some(bus)) => {
                let _ = updates.send(Update::Fcitx(fcitx::read(bus).await));
            }
            (Command::InputMethod { name, ibus: false }, _, Some(bus)) => fcitx::set(bus, &name, updates).await,
            (Command::Inhibitors, Some(bus), _) => {
                let _ = updates.send(Update::Inhibitors(login::inhibitors(bus).await));
            }
            (Command::ScreenReader(on), _, Some(bus)) => a11y::set_status(bus, on).await,
            (Command::WidgetActivated { owner, id }, _, Some(bus)) => widgets::activated(bus, &owner, &id).await,
            // Its bus is down, which `run` has logged.
            _ => {}
        }
    }
}

#[zbus::proxy(interface = "org.freedesktop.timedate1", default_service = "org.freedesktop.timedate1", default_path = "/org/freedesktop/timedate1")]
trait Timedate {
    fn set_timezone(&self, timezone: &str, interactive: bool) -> zbus::Result<()>;
}

fn command_name(command: &Command) -> String {
    match command {
        Command::Reboot => "reboot",
        Command::PowerOff => "power-off",
        _ => "suspend",
    }
    .into()
}

#[zbus::proxy(interface = "org.freedesktop.login1.Manager", default_service = "org.freedesktop.login1", default_path = "/org/freedesktop/login1")]
trait Login {
    fn suspend(&self, interactive: bool) -> zbus::Result<()>;
    fn reboot(&self, interactive: bool) -> zbus::Result<()>;
    fn power_off(&self, interactive: bool) -> zbus::Result<()>;
}

/// Sleep, restart or power off through logind ("interactive": polkit may ask for a password).
async fn power(bus: &Connection, what: &str) {
    let result = match LoginProxy::new(bus).await {
        Ok(login) => match what {
            "reboot" => login.reboot(true).await,
            "power-off" => login.power_off(true).await,
            _ => login.suspend(true).await,
        },
        Err(err) => Err(err),
    };
    if let Err(err) = result {
        tracing::warn!("{what}: {err}");
    }
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

/// Where an alert's answer goes: the agent call (BlueZ's, NetworkManager's) waiting on it.
pub struct Answer<T>(Arc<Mutex<Option<async_channel::Sender<T>>>>);

impl<T> Default for Answer<T> {
    fn default() -> Self {
        Self(Arc::default())
    }
}

impl<T> Clone for Answer<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Answer<T> {
    pub fn give(&self, value: T) {
        if let Some(waiting) = self.0.lock().ok().and_then(|mut w| w.take()) {
            let _ = waiting.try_send(value);
        }
    }

    /// A new question: the next `give` answers it. One still waiting gets an error (its sender is dropped).
    pub fn wait(&self) -> async_channel::Receiver<T> {
        let (tx, rx) = async_channel::bounded(1);
        if let Ok(mut waiting) = self.0.lock() {
            *waiting = Some(tx);
        }
        rx
    }

    /// The caller gave up: drop the question.
    pub fn cancel(&self) {
        if let Ok(mut waiting) = self.0.lock() {
            waiting.take();
        }
    }
}
