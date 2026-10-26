//! `ext-foreign-toplevel-list-v1` (E6.11): every open window (all Spaces, minimised too) with its title and app id,
//! for third-party bars, docks and switchers. Unsandboxed clients only (perms.rs).
// ponytail: reconciled once per event-loop turn (a title read per window); hook map/title/close if it ever shows up.

use smithay::{
    delegate_foreign_toplevel_list,
    desktop::Window,
    reexports::wayland_server::DisplayHandle,
    wayland::foreign_toplevel_list::{ForeignToplevelHandle, ForeignToplevelListHandler, ForeignToplevelListState},
};

use crate::state::State;

pub struct ToplevelList {
    state: ForeignToplevelListState,
    handles: Vec<(Window, ForeignToplevelHandle)>,
}

impl ToplevelList {
    pub fn new(dh: &DisplayHandle) -> Self {
        Self { state: ForeignToplevelListState::new_with_filter::<State>(dh, super::unsandboxed), handles: Vec::new() }
    }
}

impl State {
    /// Announces new windows, sends title and app id changes, and closes the handles of windows gone.
    pub fn sync_toplevel_list(&mut self) {
        let now: Vec<(Window, String, String)> = self
            .space
            .elements()
            .chain(self.hidden.iter().map(|(w, _)| w))
            .filter(|w| w.toplevel().is_some())
            .map(|w| (w.clone(), crate::render::window_title(w), self.app_id(w).unwrap_or_default()))
            .collect();
        let list = &mut self.protocols.toplevel_list;
        list.handles.retain(|(w, h)| {
            let open = now.iter().any(|(n, ..)| n == w);
            if !open {
                list.state.remove_toplevel(h);
            }
            open
        });
        for (window, title, app_id) in now {
            match list.handles.iter().find(|(w, _)| *w == window) {
                Some((_, h)) if h.title() != title || h.app_id() != app_id => {
                    h.send_title(&title);
                    h.send_app_id(&app_id);
                    h.send_done();
                }
                Some(_) => {}
                None => {
                    let handle = list.state.new_toplevel::<State>(title, app_id);
                    list.handles.push((window, handle));
                }
            }
        }
    }
}

impl ForeignToplevelListHandler for State {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.protocols.toplevel_list.state
    }
}

delegate_foreign_toplevel_list!(State);
