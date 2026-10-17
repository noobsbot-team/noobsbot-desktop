//! Clipboard persistence (E1.1): Wayland drops the clipboard when the app that copied quits; macOS keeps it.
//! Every new clipboard is copied into memory (up to 10 MB); when its app is gone we serve the copy ourselves.

use std::{
    io::{Read, Write},
    os::fd::OwnedFd,
    sync::Arc,
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
            // ponytail: one blocking thread per mime type; an app that never closes the pipe parks a thread.
            std::thread::spawn(move || {
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

    /// An app pastes from our copy.
    pub fn send(&self, mime: &str, fd: OwnedFd) {
        let Some((_, bytes)) = self.data.iter().find(|(m, _)| m == mime) else { return };
        let bytes = bytes.clone();
        // A worker thread: a slow reader must never block the frame loop (E1.11).
        std::thread::spawn(move || {
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
}
