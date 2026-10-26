//! `ext-workspace-v1` (E5.1, E6.11): Spaces as workspaces, for third-party bars and pagers. A group per set of
//! Spaces (one per display, or one shared, `[spaces] per_display`), its Spaces in order with their names and which
//! is shown; `activate` switches to one. Unsandboxed clients only (perms.rs).
// ponytail: checked once per event-loop turn, and any change sends every group and workspace anew (the old ones
// removed). No create/remove/assign; a wl_output bound after the groups were sent gets no output_enter.

use std::sync::Mutex;

use smithay::{
    output::Output,
    reexports::{
        wayland_protocols::ext::workspace::v1::server::{
            ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1, GroupCapabilities},
            ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1, WorkspaceCapabilities},
            ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
            backend::{ClientId, GlobalId},
        },
    },
};

use crate::state::State;

/// One set of Spaces: its key in `spaces.outputs`, the displays showing it, its Spaces' names and the shown one.
#[derive(Clone, PartialEq)]
struct Group {
    key: String,
    outputs: Vec<Output>,
    names: Vec<String>,
    current: usize,
}

/// A group sent to a manager, with its workspaces.
type Sent = (ExtWorkspaceGroupHandleV1, Vec<ExtWorkspaceHandleV1>);

pub struct Workspaces {
    _global: GlobalId,
    /// Each bound manager and what it was sent (`None`: nothing yet).
    managers: Vec<(ExtWorkspaceManagerV1, Option<Vec<Sent>>)>,
    groups: Vec<Group>,
}

impl Workspaces {
    pub fn new(dh: &DisplayHandle) -> Self {
        Self { _global: dh.create_global::<State, ExtWorkspaceManagerV1, _>(1, ()), managers: Vec::new(), groups: Vec::new() }
    }
}

/// The keys of the Spaces the displays named `names` show, in display order, each once.
fn keys(names: &[String], per_display: bool) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for key in names.iter().map(|n| crate::spaces::desk_key(per_display, n)) {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

impl State {
    /// Sends every manager the Spaces when they changed.
    pub fn sync_workspaces(&mut self) {
        if self.protocols.workspaces.managers.is_empty() {
            return;
        }
        let outputs: Vec<Output> = self.space.outputs().cloned().collect();
        let names: Vec<String> = outputs.iter().map(Output::name).collect();
        let groups: Vec<Group> = keys(&names, self.config.spaces.per_display)
            .into_iter()
            .map(|key| {
                let (names, current) = self.desk_names_at(&key);
                let outputs = outputs.iter().filter(|o| self.output_key(o) == key).cloned().collect();
                Group { key, outputs, names, current }
            })
            .collect();
        let ws = &mut self.protocols.workspaces;
        let changed = ws.groups != groups;
        ws.groups = groups;
        let dh = self.display_handle.clone();
        for (manager, sent) in &mut ws.managers {
            if sent.is_some() && !changed {
                continue;
            }
            for (group, workspaces) in sent.take().unwrap_or_default() {
                for w in workspaces {
                    group.workspace_leave(&w);
                    w.removed();
                }
                group.removed();
            }
            *sent = Some(send_groups(&dh, manager, &ws.groups));
        }
    }
}

/// Sends `groups` to `manager` as fresh groups and workspaces, then `done`.
fn send_groups(dh: &DisplayHandle, manager: &ExtWorkspaceManagerV1, groups: &[Group]) -> Vec<Sent> {
    let Some(client) = manager.client() else { return Vec::new() };
    let mut sent = Vec::new();
    for g in groups {
        let Ok(group) = client.create_resource::<ExtWorkspaceGroupHandleV1, _, State>(dh, manager.version(), ()) else { continue };
        manager.workspace_group(&group);
        group.capabilities(GroupCapabilities::empty());
        for output in &g.outputs {
            output.client_outputs(&client).for_each(|o| group.output_enter(&o));
        }
        let mut workspaces = Vec::new();
        for (i, name) in g.names.iter().enumerate() {
            let Ok(w) = client.create_resource::<ExtWorkspaceHandleV1, _, State>(dh, manager.version(), (g.key.clone(), i)) else { continue };
            manager.workspace(&w);
            w.name(name.clone());
            w.coordinates((i as u32).to_ne_bytes().to_vec());
            w.state(if i == g.current { ext_workspace_handle_v1::State::Active } else { ext_workspace_handle_v1::State::empty() });
            w.capabilities(WorkspaceCapabilities::Activate);
            group.workspace_enter(&w);
            workspaces.push(w);
        }
        sent.push((group, workspaces));
    }
    manager.done();
    sent
}

impl GlobalDispatch<ExtWorkspaceManagerV1, ()> for State {
    /// The groups go out on this loop turn's sync.
    fn bind(state: &mut Self, _: &DisplayHandle, _: &Client, resource: New<ExtWorkspaceManagerV1>, _: &(), init: &mut DataInit<'_, Self>) {
        let manager = init.init(resource, Mutex::default());
        state.protocols.workspaces.managers.push((manager, None));
    }

    fn can_view(client: Client, _: &()) -> bool {
        super::unsandboxed(&client)
    }
}

/// Spaces asked to be shown, until `commit`.
type Pending = Mutex<Vec<(String, usize)>>;

impl Dispatch<ExtWorkspaceManagerV1, Pending> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        manager: &ExtWorkspaceManagerV1,
        request: ext_workspace_manager_v1::Request,
        pending: &Pending,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_workspace_manager_v1::Request::Commit => {
                for (key, i) in std::mem::take(&mut *pending.lock().unwrap()) {
                    state.show_space_at(&key, i);
                }
            }
            ext_workspace_manager_v1::Request::Stop => {
                state.protocols.workspaces.managers.retain(|(m, _)| m != manager);
                manager.finished();
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, manager: &ExtWorkspaceManagerV1, _: &Pending) {
        state.protocols.workspaces.managers.retain(|(m, _)| m != manager);
    }
}

impl Dispatch<ExtWorkspaceGroupHandleV1, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ExtWorkspaceGroupHandleV1,
        _: ext_workspace_group_handle_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

impl Dispatch<ExtWorkspaceHandleV1, (String, usize)> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        _: &ExtWorkspaceHandleV1,
        request: ext_workspace_handle_v1::Request,
        (key, i): &(String, usize),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let ext_workspace_handle_v1::Request::Activate = request {
            // Queued on the client's manager(s) until its commit.
            for (manager, _) in state.protocols.workspaces.managers.iter().filter(|(m, _)| m.client().as_ref() == Some(client)) {
                if let Some(pending) = manager.data::<Pending>() {
                    pending.lock().unwrap().push((key.clone(), *i));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_group_per_display_or_one_shared() {
        let names = ["eDP-1".to_owned(), "HDMI-A-1".to_owned()];
        assert_eq!(keys(&names, true), ["eDP-1", "HDMI-A-1"]);
        assert_eq!(keys(&names, false), [""]);
    }
}
