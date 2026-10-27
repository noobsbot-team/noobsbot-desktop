//! `ext-foreign-toplevel-list-v1` (E6.11): every open window (all Spaces, minimised too) with its title and app id,
//! for third-party bars, docks and switchers. Unsandboxed clients only (perms.rs).
// ponytail: reconciled once per event-loop turn while a client has the list (a title read per window, O(n²) matching);
// hook map/title/close if it ever shows up.

use smithay::{
    desktop::Window,
    reexports::{
        wayland_protocols::ext::foreign_toplevel_list::v1::server::{
            ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
            ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
        },
        wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, backend::ClientId},
    },
    wayland::foreign_toplevel_list::{ForeignToplevelHandle, ForeignToplevelListGlobalData, ForeignToplevelListHandler, ForeignToplevelListState},
};

use crate::state::State;

pub struct ToplevelList {
    state: ForeignToplevelListState,
    handles: Vec<(Window, ForeignToplevelHandle)>,
    /// Clients' list objects: with none, nobody reads the list and it isn't kept.
    bound: usize,
}

impl ToplevelList {
    pub fn new(dh: &DisplayHandle) -> Self {
        Self { state: ForeignToplevelListState::new_with_filter::<State>(dh, super::unsandboxed), handles: Vec::new(), bound: 0 }
    }
}

impl State {
    /// Announces new windows, sends title and app id changes, and closes the handles of windows gone. Only while a
    /// client has the list: a bind brings it up to date first.
    pub fn sync_toplevel_list(&mut self) {
        if self.protocols.toplevel_list.bound == 0 {
            return;
        }
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

// smithay's own handling, with the list's binds counted (`bound`).
smithay::reexports::wayland_server::delegate_dispatch!(State: [ExtForeignToplevelHandleV1: ForeignToplevelHandle] => ForeignToplevelListState);

impl GlobalDispatch<ExtForeignToplevelListV1, ForeignToplevelListGlobalData> for State {
    fn bind(
        state: &mut Self,
        dh: &DisplayHandle,
        client: &Client,
        resource: New<ExtForeignToplevelListV1>,
        data: &ForeignToplevelListGlobalData,
        init: &mut DataInit<'_, Self>,
    ) {
        state.protocols.toplevel_list.bound += 1;
        // Not kept while nobody had it: the new client gets today's windows, not stale ones.
        state.sync_toplevel_list();
        <ForeignToplevelListState as GlobalDispatch<ExtForeignToplevelListV1, _, Self>>::bind(state, dh, client, resource, data, init);
    }

    fn can_view(client: Client, data: &ForeignToplevelListGlobalData) -> bool {
        <ForeignToplevelListState as GlobalDispatch<ExtForeignToplevelListV1, _, Self>>::can_view(client, data)
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        list: &ExtForeignToplevelListV1,
        request: ext_foreign_toplevel_list_v1::Request,
        data: &(),
        dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        <ForeignToplevelListState as Dispatch<ExtForeignToplevelListV1, (), Self>>::request(state, client, list, request, data, dh, init);
    }

    fn destroyed(state: &mut Self, client: ClientId, list: &ExtForeignToplevelListV1, data: &()) {
        <ForeignToplevelListState as Dispatch<ExtForeignToplevelListV1, (), Self>>::destroyed(state, client, list, data);
        let list = &mut state.protocols.toplevel_list;
        list.bound = list.bound.saturating_sub(1);
        // The last one gone: the handles (and the windows they hold) go too; the next bind makes them anew.
        if list.bound == 0 {
            for (_, handle) in list.handles.drain(..) {
                list.state.remove_toplevel(&handle);
            }
        }
    }
}
