//! The screen reader's key monitor (E8.6): `org.freedesktop.a11y.KeyboardMonitor` on the session bus. The screen
//! reader sees keys only through this on Wayland: it watches every key to speak it, and grabs its own shortcuts
//! (Insert or Caps Lock plus a key), which apps then never get. Grabs are matched on the input path from the shared
//! `Monitor`; the events go out here, each only to the client that asked (never broadcast).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use futures_lite::StreamExt;
use zbus::{
    Connection, fdo,
    fdo::{RequestNameFlags, RequestNameReply},
    message::Header,
};

const NAME: &str = "org.freedesktop.a11y.Manager";
const PATH: &str = "/org/freedesktop/a11y/Manager";
const INTERFACE: &str = "org.freedesktop.a11y.KeyboardMonitor";
/// Only the owner of the screen reader's own bus name may monitor keys: anyone else would be a key logger.
const READER: &str = "org.gnome.Orca.KeyboardMonitor";
/// A grabbed modifier pressed twice within this goes through the second time (Caps Lock twice toggles Caps Lock).
const DOUBLE_PRESS: Duration = Duration::from_millis(400);

/// One key event as the interface sends it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Key {
    pub released: bool,
    /// XKB modifier mask (depressed, latched and locked).
    pub state: u32,
    pub keysym: u32,
    /// The character it types, or 0.
    pub unichar: u32,
    /// XKB keycode.
    pub keycode: u16,
}

#[derive(Debug, Default)]
struct Client {
    grab_all: bool,
    watch_all: bool,
    /// Keysyms grabbed themselves, and every key pressed while one is down.
    modifiers: Vec<u32>,
    /// (keysym, exact modifier mask).
    keystrokes: Vec<(u32, u32)>,
}

impl Client {
    fn grabs(&self, key: &Key, down: &[u32]) -> bool {
        self.grab_all || self.modifiers.iter().any(|m| *m == key.keysym || down.contains(m)) || self.keystrokes.contains(&(key.keysym, key.state))
    }
}

/// Clients by unique bus name, and the keys held under a grab.
#[derive(Debug, Default)]
pub struct Monitor {
    clients: HashMap<String, Client>,
    /// Grabbed modifiers held down.
    down: Vec<u32>,
    /// Keycodes whose press was swallowed; their release is too.
    held: Vec<u16>,
    /// The last swallowed modifier press, for `DOUBLE_PRESS`.
    last: Option<(u32, Instant)>,
}

impl Monitor {
    /// The clients `key` goes to, and whether apps and shortcuts never see it.
    pub fn key(&mut self, key: &Key, now: Instant) -> (Vec<String>, bool) {
        let to = self.clients.iter().filter(|(_, c)| c.watch_all || c.grabs(key, &self.down)).map(|(n, _)| n.clone()).collect();
        if key.released {
            self.down.retain(|m| *m != key.keysym);
            let held = self.held.iter().position(|c| *c == key.keycode).map(|i| self.held.swap_remove(i));
            return (to, held.is_some());
        }
        let modifier = self.clients.values().any(|c| c.modifiers.contains(&key.keysym));
        if modifier && self.last.take().is_some_and(|(sym, t)| sym == key.keysym && now.duration_since(t) < DOUBLE_PRESS) {
            return (to, false);
        }
        if modifier {
            self.last = Some((key.keysym, now));
            self.down.push(key.keysym);
        }
        let grabbed = self.clients.values().any(|c| c.grabs(key, &self.down));
        if grabbed && !self.held.contains(&key.keycode) {
            self.held.push(key.keycode);
        }
        (to, grabbed)
    }

    fn edit(&mut self, client: String, edit: impl FnOnce(&mut Client)) {
        edit(self.clients.entry(client).or_default());
    }
}

/// Matches keys against the clients' grabs; sends their events to the services thread.
pub struct Keys {
    pub monitor: Arc<Mutex<Monitor>>,
    pub events: async_channel::Sender<(Vec<String>, Key)>,
}

impl Keys {
    /// Hands `key` to the clients that want it; true when apps and shortcuts must not see it.
    pub fn key(&self, key: Key) -> bool {
        let Ok(mut monitor) = self.monitor.lock() else { return false };
        if monitor.clients.is_empty() {
            return false;
        }
        let (to, swallow) = monitor.key(&key, Instant::now());
        if !to.is_empty() {
            let _ = self.events.try_send((to, key));
        }
        swallow
    }
}

struct Server(Arc<Mutex<Monitor>>);

impl Server {
    /// The caller's unique name, when it owns `READER`.
    async fn caller(bus: &Connection, header: &Header<'_>) -> fdo::Result<String> {
        let sender = header.sender().ok_or_else(|| fdo::Error::AccessDenied("no sender".into()))?;
        let owner = fdo::DBusProxy::new(bus).await?.get_name_owner(READER.try_into().map_err(zbus::Error::from)?).await.ok();
        match owner {
            Some(owner) if owner.as_str() == sender.as_str() => Ok(sender.to_string()),
            _ => Err(fdo::Error::AccessDenied(format!("only the owner of {READER} may monitor keys"))),
        }
    }

    async fn edit(&self, bus: &Connection, header: &Header<'_>, edit: impl FnOnce(&mut Client)) -> fdo::Result<()> {
        let client = Self::caller(bus, header).await?;
        if let Ok(mut monitor) = self.0.lock() {
            monitor.edit(client, edit);
        }
        Ok(())
    }
}

#[zbus::interface(name = "org.freedesktop.a11y.KeyboardMonitor")]
impl Server {
    async fn grab_keyboard(&self, #[zbus(header)] header: Header<'_>, #[zbus(connection)] bus: &Connection) -> fdo::Result<()> {
        self.edit(bus, &header, |c| c.grab_all = true).await
    }

    async fn ungrab_keyboard(&self, #[zbus(header)] header: Header<'_>, #[zbus(connection)] bus: &Connection) -> fdo::Result<()> {
        self.edit(bus, &header, |c| c.grab_all = false).await
    }

    async fn watch_keyboard(&self, #[zbus(header)] header: Header<'_>, #[zbus(connection)] bus: &Connection) -> fdo::Result<()> {
        self.edit(bus, &header, |c| c.watch_all = true).await
    }

    async fn unwatch_keyboard(&self, #[zbus(header)] header: Header<'_>, #[zbus(connection)] bus: &Connection) -> fdo::Result<()> {
        self.edit(bus, &header, |c| c.watch_all = false).await
    }

    async fn set_key_grabs(
        &self,
        modifiers: Vec<u32>,
        keystrokes: Vec<(u32, u32)>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] bus: &Connection,
    ) -> fdo::Result<()> {
        self.edit(bus, &header, |c| (c.modifiers, c.keystrokes) = (modifiers, keystrokes)).await
    }
}

/// Serves the interface (a session that already has one keeps it), drops clients that leave the bus, and sends the
/// key events.
pub async fn serve(bus: &Connection, monitor: Arc<Mutex<Monitor>>, events: async_channel::Receiver<(Vec<String>, Key)>) {
    if let Err(err) = bus.object_server().at(PATH, Server(monitor.clone())).await {
        return tracing::warn!("key monitor off: {err}");
    }
    match bus.request_name_with_flags(NAME, RequestNameFlags::DoNotQueue.into()).await {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {}
        Ok(reply) => return tracing::info!("another key monitor owns {NAME} ({reply:?}); ours stays off"),
        Err(err) => return tracing::warn!("key monitor off: {err}"),
    }
    let gone = async {
        let Ok(proxy) = fdo::DBusProxy::new(bus).await else { return };
        // arg2 (the new owner) empty: a connection left.
        let Ok(mut left) = proxy.receive_name_owner_changed_with_args(&[(2, "")]).await else { return };
        while let Some(signal) = left.next().await {
            if let (Ok(args), Ok(mut monitor)) = (signal.args(), monitor.lock()) {
                monitor.clients.remove(args.name().as_str());
            }
        }
    };
    let send = async {
        while let Ok((to, key)) = events.recv().await {
            let body = (key.released, key.state, key.keysym, key.unichar, key.keycode);
            for client in to {
                if let Err(err) = bus.emit_signal(Some(client.as_str()), PATH, INTERFACE, "KeyEvent", &body).await {
                    tracing::debug!("KeyEvent to {client}: {err}");
                }
            }
        }
    };
    futures_lite::future::zip(gone, send).await;
}

#[zbus::proxy(interface = "org.a11y.Status", default_service = "org.a11y.Bus", default_path = "/org/a11y/bus")]
trait Status {
    #[zbus(property)]
    fn set_is_enabled(&self, on: bool) -> zbus::Result<()>;
    #[zbus(property)]
    fn set_screen_reader_enabled(&self, on: bool) -> zbus::Result<()>;
}

/// Tells the accessibility bus whether a screen reader runs, which turns apps' accessibility trees on or off.
pub async fn set_status(bus: &Connection, on: bool) {
    let set = async {
        let status = StatusProxy::new(bus).await?;
        // Off leaves IsEnabled alone: another assistive tool may still need it.
        if on {
            status.set_is_enabled(true).await?;
        }
        status.set_screen_reader_enabled(on).await
    };
    if let Err(err) = set.await {
        tracing::warn!("accessibility bus: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INSERT: u32 = 0xff63;
    const A: u32 = 0x61;
    const CTRL: u32 = 4;

    fn key(keysym: u32, keycode: u16, state: u32, released: bool) -> Key {
        Key { released, state, keysym, unichar: 0, keycode }
    }

    fn reader(client: Client) -> Monitor {
        let mut m = Monitor::default();
        m.edit(":1.5".into(), |c| *c = client);
        m
    }

    #[test]
    fn grabbed_modifier_and_keys_under_it_are_swallowed() {
        let mut m = reader(Client { modifiers: vec![INSERT], ..Default::default() });
        let t = Instant::now();
        assert_eq!(m.key(&key(INSERT, 118, 0, false), t), (vec![":1.5".into()], true));
        assert_eq!(m.key(&key(A, 38, 0, false), t), (vec![":1.5".into()], true));
        assert!(m.key(&key(A, 38, 0, true), t).1);
        assert!(m.key(&key(INSERT, 118, 0, true), t).1);
        // Plain keys afterwards reach the app and aren't sent to a client that doesn't watch.
        assert_eq!(m.key(&key(A, 38, 0, false), t), (vec![], false));
        assert!(!m.key(&key(A, 38, 0, true), t).1);
    }

    #[test]
    fn double_press_of_a_grabbed_modifier_goes_through() {
        let mut m = reader(Client { modifiers: vec![INSERT], ..Default::default() });
        let t = Instant::now();
        assert!(m.key(&key(INSERT, 118, 0, false), t).1);
        assert!(m.key(&key(INSERT, 118, 0, true), t).1);
        assert!(!m.key(&key(INSERT, 118, 0, false), t + Duration::from_millis(200)).1);
        assert!(!m.key(&key(INSERT, 118, 0, true), t + Duration::from_millis(250)).1);
        // Too slow: grabbed again.
        assert!(m.key(&key(INSERT, 118, 0, false), t + Duration::from_secs(2)).1);
    }

    #[test]
    fn keystrokes_match_keysym_and_exact_mask() {
        let mut m = reader(Client { keystrokes: vec![(A, CTRL)], ..Default::default() });
        let t = Instant::now();
        assert!(!m.key(&key(A, 38, 0, false), t).1);
        assert!(!m.key(&key(A, 38, CTRL | 1, false), t).1);
        assert!(m.key(&key(A, 38, CTRL, false), t).1);
        // The release is swallowed even with Ctrl already up.
        assert!(m.key(&key(A, 38, 0, true), t).1);
    }

    #[test]
    fn watching_sends_everything_and_swallows_nothing() {
        let mut m = reader(Client { watch_all: true, ..Default::default() });
        assert_eq!(m.key(&key(A, 38, 0, false), Instant::now()), (vec![":1.5".into()], false));
        m.edit(":1.5".into(), |c| c.grab_all = true);
        assert!(m.key(&key(A, 38, 0, false), Instant::now()).1);
    }
}
