//! Clipboard persistence (E1.1): Wayland drops the clipboard when the app that copied quits; macOS keeps it.
//! Every new clipboard is copied into memory (up to 10 MB); when its app is gone we serve the copy ourselves.

use std::{
    io::{Read, Write},
    os::fd::OwnedFd,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use smithay::{
    input::Seat,
    reexports::{
        calloop::channel::Sender,
        wayland_server::{DisplayHandle, Resource, backend::ClientId},
    },
    wayland::selection::data_device::{request_data_device_client_selection, set_data_device_selection},
};

use crate::state::State;

pub const MAX_BYTES: usize = 10 * 1024 * 1024;
/// Transfer threads in flight at once (E1.11): an app that never closes or never reads its pipe parks one, so a
/// misbehaving app can't pile up threads. Past the cap the pipe is dropped, which reads as an empty transfer.
const MAX_TRANSFERS: usize = 32;
static TRANSFERS: AtomicUsize = AtomicUsize::new(0);

/// Runs `work` on a worker thread, unless `MAX_TRANSFERS` are already running.
pub fn transfer(work: impl FnOnce() + Send + 'static) {
    if TRANSFERS.fetch_add(1, Ordering::Relaxed) >= MAX_TRANSFERS {
        TRANSFERS.fetch_sub(1, Ordering::Relaxed);
        tracing::warn!("{MAX_TRANSFERS} clipboard transfers in flight, dropping one");
        return;
    }
    let spawned = std::thread::Builder::new().name("clipboard".into()).spawn(move || {
        work();
        TRANSFERS.fetch_sub(1, Ordering::Relaxed);
    });
    if spawned.is_err() {
        TRANSFERS.fetch_sub(1, Ordering::Relaxed);
    }
}

/// One mime type's contents, read from the app that copied.
pub struct Fetched {
    generation: u64,
    mime: String,
    bytes: Vec<u8>,
}

pub struct Clipboard {
    generation: u64,
    /// The app that copied, while it still serves the clipboard itself.
    owner: Option<ClientId>,
    data: Vec<(String, Arc<[u8]>)>,
    tx: Sender<Fetched>,
}

impl Clipboard {
    pub fn new(tx: Sender<Fetched>) -> Self {
        Self { generation: 0, owner: None, data: Vec::new(), tx }
    }

    /// A client set (or cleared) the clipboard. Returns the generation to fetch, if any.
    pub fn replaced(&mut self, owner: Option<ClientId>) -> u64 {
        self.generation += 1;
        self.owner = owner;
        self.data.clear();
        self.generation
    }

    pub fn store(&mut self, fetched: Fetched) {
        let total: usize = self.data.iter().map(|(_, b)| b.len()).sum();
        if fetched.generation != self.generation || total + fetched.bytes.len() > MAX_BYTES {
            return;
        }
        self.data.push((fetched.mime, fetched.bytes.into()));
    }

    /// Starts reading every mime type of the current clipboard on worker threads.
    pub fn fetch(&self, seat: &Seat<State>, generation: u64, mimes: Vec<String>) {
        if generation != self.generation {
            return;
        }
        for mime in mimes {
            let Ok((mut reader, writer)) = std::io::pipe() else { return };
            if request_data_device_client_selection(seat, mime.clone(), writer.into()).is_err() {
                continue;
            }
            let tx = self.tx.clone();
            transfer(move || {
                let mut bytes = Vec::new();
                if reader.by_ref().take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes).is_ok() && bytes.len() <= MAX_BYTES {
                    let _ = tx.send(Fetched { generation, mime, bytes });
                }
            });
        }
    }

    /// If the app that copied has quit, the compositor takes over the clipboard with its copy.
    pub fn adopt_if_orphaned(&mut self, dh: &DisplayHandle, seat: &Seat<State>) {
        let Some(owner) = &self.owner else { return };
        if dh.backend_handle().get_client_data(owner.clone()).is_ok() || self.data.is_empty() {
            return;
        }
        self.owner = None;
        set_data_device_selection(dh, seat, self.data.iter().map(|(m, _)| m.clone()).collect(), ());
    }

    /// The desktop copies `text` itself (Spotlight's calculator); apps paste it from us.
    pub fn set_text(&mut self, dh: &DisplayHandle, seat: &Seat<State>, text: &str) {
        self.set_bytes(dh, seat, &["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "STRING", "TEXT"], text.as_bytes().to_vec());
    }

    /// The desktop copies `bytes` (a screenshot's PNG), offered as each of `mimes`.
    pub fn set_bytes(&mut self, dh: &DisplayHandle, seat: &Seat<State>, mimes: &[&str], bytes: Vec<u8>) {
        self.replaced(None);
        let bytes: Arc<[u8]> = bytes.into();
        self.data = mimes.iter().map(|m| ((*m).to_owned(), bytes.clone())).collect();
        set_data_device_selection(dh, seat, self.data.iter().map(|(m, _)| m.clone()).collect(), ());
    }

    /// An app pastes from our copy.
    pub fn send(&self, mime: &str, fd: OwnedFd) {
        let Some((_, bytes)) = self.data.iter().find(|(m, _)| m == mime) else { return };
        let bytes = bytes.clone();
        // A worker thread: a slow reader must never block the frame loop (E1.11).
        transfer(move || {
            let _ = std::fs::File::from(fd).write_all(&bytes);
        });
    }
}

/// The client owning the keyboard focus: only it may set the clipboard.
pub fn focused_client(dh: &DisplayHandle, seat: &Seat<State>) -> Option<ClientId> {
    let surface = seat.get_keyboard()?.current_focus()?;
    dh.get_client(surface.id()).ok().map(|c| c.id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_current_generation_within_limit() {
        let (tx, _rx) = smithay::reexports::calloop::channel::channel();
        let mut clip = Clipboard::new(tx);
        let old = clip.replaced(None);
        let current = clip.replaced(None);

        clip.store(Fetched { generation: old, mime: "text/plain".into(), bytes: b"stale".to_vec() });
        assert!(clip.data.is_empty(), "stale fetch must be dropped");

        clip.store(Fetched { generation: current, mime: "text/plain".into(), bytes: b"hi".to_vec() });
        clip.store(Fetched { generation: current, mime: "image/png".into(), bytes: vec![0; MAX_BYTES] });
        assert_eq!(clip.data.len(), 1, "total size is capped at MAX_BYTES");

        clip.replaced(None);
        assert!(clip.data.is_empty(), "a new clipboard clears the copy");
    }

    #[test]
    fn transfers_are_capped() {
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let wait = Arc::new(std::sync::Mutex::new(wait));
        let (started_tx, started) = std::sync::mpsc::channel();
        for _ in 0..MAX_TRANSFERS + 5 {
            let (wait, started_tx) = (wait.clone(), started_tx.clone());
            transfer(move || {
                let _ = started_tx.send(());
                let _ = wait.lock().unwrap().recv();
            });
        }
        drop(started_tx);
        // Every thread that got a slot reports in; the extra 5 were dropped.
        assert_eq!(started.iter().take(MAX_TRANSFERS).count(), MAX_TRANSFERS);
        assert_eq!(TRANSFERS.load(Ordering::Relaxed), MAX_TRANSFERS);
        drop(release);
        while TRANSFERS.load(Ordering::Relaxed) > 0 {
            std::thread::yield_now();
        }
    }
}
