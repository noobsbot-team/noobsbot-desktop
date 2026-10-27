//! Buffer synchronisation (E1.1, E1.10): a commit waits, off the frame thread, until the app's GPU has finished its
//! buffer — on the explicit acquire point (linux-drm-syncobj-v1, required on NVIDIA 555+ to avoid flicker) or on the
//! dmabuf's implicit fence. Drawing an unfinished buffer would stall the whole frame.

use std::sync::{Arc, Mutex};

use smithay::{
    backend::drm::{DrmDeviceFd, DrmNode},
    delegate_drm_syncobj,
    reexports::{
        calloop::{Interest, RegistrationToken},
        wayland_server::{Client, Resource, protocol::wl_surface::WlSurface},
    },
    utils::DeviceFd,
    wayland::{
        compositor::{BufferAssignment, CompositorHandler, SurfaceAttributes, add_blocker, add_destruction_hook, add_pre_commit_hook, with_states},
        dmabuf::get_dmabuf,
        drm_syncobj::{DrmSyncobjCachedState, DrmSyncobjHandler, DrmSyncobjState, supports_syncobj_eventfd},
    },
};

use crate::state::State;

/// Offers linux-drm-syncobj-v1 when the GPU's render node supports syncobj eventfds.
pub fn init(state: &mut State, node: DrmNode) {
    let Some(path) = node.dev_path() else { return };
    let file = match std::fs::OpenOptions::new().read(true).write(true).open(&path) {
        Ok(file) => file,
        Err(err) => {
            tracing::warn!("opening {}: {err}, no explicit sync", path.display());
            return;
        }
    };
    let device = DrmDeviceFd::new(DeviceFd::from(std::os::fd::OwnedFd::from(file)));
    if supports_syncobj_eventfd(&device) {
        state.syncobj = Some(DrmSyncobjState::new::<State>(&state.display_handle, device));
    } else {
        tracing::info!("{} lacks syncobj eventfd, no explicit sync", path.display());
    }
}

/// Acquire points one surface may have waiting at once. Its app can leave them unsignalled for good, and each wait
/// holds an eventfd and a loop source; past this, a commit waits on the buffer's implicit fence instead.
const ACQUIRE_WAITS: usize = 8;

/// A surface's acquire-point waits, each by a number of its own: one leaves when its point signals, the rest with the
/// surface (its client's end included).
#[derive(Default)]
struct Waits {
    next: u64,
    tokens: Vec<(u64, RegistrationToken)>,
}

/// Holds each commit of `surface` with a new dmabuf until the buffer is ready to read.
pub fn add_hook(surface: &WlSurface) {
    let waits = Arc::new(Mutex::new(Waits::default()));
    let gone = waits.clone();
    add_destruction_hook::<State, _>(surface, move |state, _| {
        let tokens = std::mem::take(&mut gone.lock().unwrap().tokens);
        for (_, token) in tokens {
            state.loop_handle.remove(token);
        }
    });
    add_pre_commit_hook::<State, _>(surface, move |state, _, surface| {
        let (acquire, dmabuf) = with_states(surface, |states| {
            let acquire = states.cached_state.get::<DrmSyncobjCachedState>().pending().acquire_point.clone();
            let dmabuf = match &states.cached_state.get::<SurfaceAttributes>().pending().buffer {
                Some(BufferAssignment::NewBuffer(buffer)) => get_dmabuf(buffer).cloned().ok(),
                _ => None,
            };
            (acquire, dmabuf)
        });
        let Some(dmabuf) = dmabuf else { return };
        let Some(client) = surface.client() else { return };
        if let Some(acquire) = acquire.filter(|_| waits.lock().unwrap().tokens.len() < ACQUIRE_WAITS)
            && let Ok((blocker, source)) = acquire.generate_blocker()
        {
            let id = {
                let mut w = waits.lock().unwrap();
                w.next += 1;
                w.next
            };
            if let Ok(token) = state.loop_handle.insert_source(source, cleared(client, Some((waits.clone(), id)))) {
                waits.lock().unwrap().tokens.push((id, token));
                add_blocker(surface, blocker);
            }
        } else if let Ok((blocker, source)) = dmabuf.generate_blocker(Interest::READ)
            && state.loop_handle.insert_source(source, cleared(client, None)).is_ok()
        {
            add_blocker(surface, blocker);
        }
    });
}

/// When a wait ends, applies the commits it held back; an acquire point's wait (`wait`) leaves its surface's list.
fn cleared<M>(client: Client, wait: Option<(Arc<Mutex<Waits>>, u64)>) -> impl FnMut((), &mut M, &mut State) -> std::io::Result<()> {
    move |_, _, state| {
        if let Some((waits, id)) = &wait {
            waits.lock().unwrap().tokens.retain(|(n, _)| n != id);
        }
        let dh = state.display_handle.clone();
        state.client_compositor_state(&client).blocker_cleared(state, &dh);
        Ok(())
    }
}

impl DrmSyncobjHandler for State {
    fn drm_syncobj_state(&mut self) -> Option<&mut DrmSyncobjState> {
        self.syncobj.as_mut()
    }
}

delegate_drm_syncobj!(State);
