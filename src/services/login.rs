//! logind (E6.5): a `delay` sleep inhibitor so the screen locks before the machine sleeps (waking never shows the
//! desktop for a frame), and a `block` on the power key so its press reaches us and asks first. Lid and idle sleep go
//! through logind too, so every sleep passes `PrepareForSleep`. Only in a real session: nested, the host's logind is
//! never touched.

use std::{os::fd::OwnedFd, sync::Mutex};

use futures_lite::StreamExt;
use smithay::reexports::calloop::channel::Sender;
use zbus::Connection;

use super::Update;

/// A `ListInhibitors` row: what, who, why, mode, uid, pid.
type Row = (String, String, String, String, u32, u32);

/// The sleep inhibitor's fd; closing it lets the sleep go ahead.
static SLEEP: Mutex<Option<OwnedFd>> = Mutex::new(None);

#[zbus::proxy(interface = "org.freedesktop.login1.Manager", default_service = "org.freedesktop.login1", default_path = "/org/freedesktop/login1")]
trait Manager {
    fn inhibit(&self, what: &str, who: &str, why: &str, mode: &str) -> zbus::Result<zbus::zvariant::OwnedFd>;
    fn list_inhibitors(&self) -> zbus::Result<Vec<Row>>;
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

async fn inhibit(login: &ManagerProxy<'_>, what: &str, why: &str, mode: &str) -> Option<OwnedFd> {
    login.inhibit(what, "Noobsbot Desktop", why, mode).await.inspect_err(|err| tracing::warn!("logind {what} inhibitor: {err}")).ok().map(OwnedFd::from)
}

async fn take_sleep(login: &ManagerProxy<'_>) {
    let fd = inhibit(login, "sleep", "Lock the screen first", "delay").await;
    if let Ok(mut held) = SLEEP.lock() {
        *held = fd;
    }
}

/// The lock frame is on screen: sleep may go ahead.
pub fn release_sleep() {
    if let Ok(mut held) = SLEEP.lock() {
        held.take();
    }
}

/// An app keeping the machine awake: who, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Inhibitor {
    pub who: String,
    pub why: String,
}

/// logind's inhibitors that block sleep or idle; `delay` ones (ours, NetworkManager's) only postpone it.
fn blocking(list: Vec<Row>) -> Vec<Inhibitor> {
    list.into_iter()
        .filter(|(what, who, _, mode, _, _)| mode == "block" && who != "Noobsbot Desktop" && what.split(':').any(|w| w == "sleep" || w == "idle"))
        .map(|(_, who, why, ..)| Inhibitor { who, why })
        .collect()
}

/// Who blocks sleep now; empty without logind.
pub async fn inhibitors(bus: &Connection) -> Vec<Inhibitor> {
    let list = async { ManagerProxy::new(bus).await?.list_inhibitors().await };
    list.await.inspect_err(|err| tracing::debug!("logind inhibitors: {err}")).map(blocking).unwrap_or_default()
}

/// Holds the inhibitors and relays `PrepareForSleep`: `start` → `Update::PrepareForSleep`, back awake →
/// `Update::Resumed` (and the sleep inhibitor again). For good: after a bus hiccup it starts again, or the screen would
/// stop locking before sleep.
pub async fn watch(bus: &Connection, updates: &Sender<Update>) {
    super::again("logind", || watch_once(bus, updates)).await;
}

async fn watch_once(bus: &Connection, updates: &Sender<Update>) {
    let Ok(login) = ManagerProxy::new(bus).await.inspect_err(|err| tracing::warn!("logind off: {err}")) else { return };
    let Ok(mut signals) = login.receive_prepare_for_sleep().await.inspect_err(|err| tracing::warn!("logind off: {err}")) else { return };
    take_sleep(&login).await;
    let _power_key = inhibit(&login, "handle-power-key", "Asks before shutting down", "block").await;
    while let Some(signal) = signals.next().await {
        let Ok(start) = signal.args().map(|a| a.start) else { continue };
        if start {
            // ponytail: logind gives up waiting after InhibitDelayMaxSec (5 s) even if no frame came.
            let _ = updates.send(Update::PrepareForSleep);
        } else {
            take_sleep(&login).await;
            let _ = updates.send(Update::Resumed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_sleep_and_idle_blockers_count() {
        let row = |what: &str, who: &str, mode: &str| (what.into(), who.into(), "because".into(), mode.into(), 1000, 42);
        let list = vec![
            row("sleep", "NetworkManager", "delay"),
            row("handle-power-key", "Noobsbot Desktop", "block"),
            row("sleep", "Noobsbot Desktop", "block"),
            row("shutdown:sleep", "Transmission", "block"),
            row("idle", "Videos", "block"),
            row("handle-lid-switch", "Docked", "block"),
        ];
        let who: Vec<_> = blocking(list).into_iter().map(|i| i.who).collect();
        assert_eq!(who, ["Transmission", "Videos"]);
    }
}
