//! logind (E6.5): a `delay` sleep inhibitor so the screen locks before the machine sleeps (waking never shows the
//! desktop for a frame), and a `block` on the power key so its press reaches us and asks first. Lid and idle sleep go
//! through logind too, so every sleep passes `PrepareForSleep`. Only in a real session: nested, the host's logind is
//! never touched.

use std::{os::fd::OwnedFd, sync::Mutex};

use futures_lite::StreamExt;
use smithay::reexports::calloop::channel::Sender;
use zbus::Connection;

use super::Update;

/// The sleep inhibitor's fd; closing it lets the sleep go ahead.
static SLEEP: Mutex<Option<OwnedFd>> = Mutex::new(None);

#[zbus::proxy(interface = "org.freedesktop.login1.Manager", default_service = "org.freedesktop.login1", default_path = "/org/freedesktop/login1")]
trait Manager {
    fn inhibit(&self, what: &str, who: &str, why: &str, mode: &str) -> zbus::Result<zbus::zvariant::OwnedFd>;
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

/// Holds the inhibitors and relays `PrepareForSleep`: `start` → `Update::PrepareForSleep`, back awake →
/// `Update::Resumed` (and the sleep inhibitor again).
pub async fn watch(bus: &Connection, updates: &Sender<Update>) {
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
