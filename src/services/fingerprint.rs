//! Fingerprint unlock (E7.2): fprintd's verify runs beside the lock screen's password field, for the user's enrolled
//! fingers. Each lock gets its own system bus connection, so stopping is dropping it: fprintd then ends the verify and
//! releases the reader by itself, whatever it was in the middle of. sudo and other PAM users are the system's own PAM
//! config (`pam_fprintd`), untouched here.

use std::time::Duration;

use futures_lite::{StreamExt, future};
use smithay::reexports::calloop::channel::Sender;
use zbus::zvariant::OwnedObjectPath;

use super::Update;

/// No-matches before only the password unlocks: pam_fprintd's default `max-tries`.
const TRIES: usize = 3;

/// fprintd may be started for the first call, and claiming opens the reader: both can outlast `super::TIMEOUT`.
const FPRINT_TIMEOUT: Duration = Duration::from_secs(25);

/// What the lock screen shows under the password field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Verify {
    /// No reader, no enrolled finger, or stopped: nothing.
    #[default]
    Off,
    /// "Touch the fingerprint sensor".
    Waiting,
    /// "Not recognised, try again", until the next touch.
    NoMatch,
    /// Unlocks, as the right password does.
    Match,
}

/// A `VerifyStatus` result as the lock screen shows it; `None` ends the verify (the reader went, or failed).
pub fn status(result: &str) -> Option<Verify> {
    match result {
        "verify-match" => Some(Verify::Match),
        "verify-no-match" => Some(Verify::NoMatch),
        // A touch too short, off centre or lifted early: fprintd keeps waiting.
        "verify-retry-scan" | "verify-swipe-too-short" | "verify-finger-not-centered" | "verify-remove-and-retry" => Some(Verify::Waiting),
        // "verify-disconnected", "verify-unknown-error".
        _ => None,
    }
}

#[zbus::proxy(interface = "net.reactivated.Fprint.Manager", default_service = "net.reactivated.Fprint", default_path = "/net/reactivated/Fprint/Manager")]
trait Manager {
    fn get_default_device(&self) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(interface = "net.reactivated.Fprint.Device", default_service = "net.reactivated.Fprint")]
trait Device {
    fn list_enrolled_fingers(&self, username: &str) -> zbus::Result<Vec<String>>;
    fn claim(&self, username: &str) -> zbus::Result<()>;
    fn verify_start(&self, finger_name: &str) -> zbus::Result<()>;
    fn verify_stop(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    fn verify_status(&self, result: &str, done: bool) -> zbus::Result<()>;
}

/// For good: each `Some(user)` on `wanted` verifies that user's finger until a match, `TRIES` no-matches, or the
/// next message (`None`: unlocked or asleep).
pub async fn serve(updates: &Sender<Update>, wanted: async_channel::Receiver<Option<String>>) {
    let mut next = wanted.recv().await;
    while let Ok(user) = next {
        next = match user {
            Some(user) => {
                let session = async {
                    if let Err(err) = verify(&user, updates).await {
                        tracing::debug!("fingerprint: {err}");
                    }
                    let _ = updates.send(Update::Fingerprint(Verify::Off));
                    future::pending().await
                };
                future::or(session, wanted.recv()).await
            }
            None => wanted.recv().await,
        };
    }
}

/// One lock's verify. Without fprintd, a reader or an enrolled finger, it ends at once.
async fn verify(user: &str, updates: &Sender<Update>) -> zbus::Result<()> {
    let bus = super::connect("DBUS_SYSTEM_BUS_ADDRESS", "/run/dbus/system_bus_socket".into(), FPRINT_TIMEOUT).await?;
    let path = ManagerProxy::new(&bus).await?.get_default_device().await?;
    let device = DeviceProxy::builder(&bus).path(path)?.build().await?;
    // None enrolled is an error too (NoEnrolledPrints).
    if device.list_enrolled_fingers(user).await?.is_empty() {
        return Ok(());
    }
    device.claim(user).await?;
    // Only fprintd's own signals: the stream checks the sender.
    let mut statuses = device.receive_verify_status().await?;
    let _ = updates.send(Update::Fingerprint(Verify::Waiting));
    for _ in 0..TRIES {
        device.verify_start("any").await?;
        loop {
            let Some(signal) = statuses.next().await else { return Ok(()) };
            let args = signal.args()?;
            let Some(state) = status(args.result) else { return Ok(()) };
            let _ = updates.send(Update::Fingerprint(state));
            if state == Verify::Match {
                return Ok(());
            }
            if args.done {
                break;
            }
        }
        // fprintd wants a stop after a finished verify before the next start.
        device.verify_stop().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_status_shown() {
        assert_eq!(status("verify-match"), Some(Verify::Match));
        assert_eq!(status("verify-no-match"), Some(Verify::NoMatch));
        for retry in ["verify-retry-scan", "verify-swipe-too-short", "verify-finger-not-centered", "verify-remove-and-retry"] {
            assert_eq!(status(retry), Some(Verify::Waiting), "{retry}: still waiting");
        }
        assert_eq!(status("verify-disconnected"), None, "the reader went: stop");
        assert_eq!(status("verify-unknown-error"), None);
    }
}
