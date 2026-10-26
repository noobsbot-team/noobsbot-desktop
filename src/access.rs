//! Our shell widgets for the screen reader over AT-SPI (E8.6): the menu bar, the dock and the open Control Center
//! as one accessible app, with names, roles and on/off states; dock icons and Control Center toggles can be pressed.
//! Built only while the screen reader is on: the adapter keeps a bus connection and a thread.

use std::sync::{Arc, Mutex};

use accesskit::{Action, ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, Node, NodeId, Role, Toggled, TreeId, TreeInfo, TreeUpdate};
use smithay::reexports::calloop::channel::{self, Sender};

use crate::{shell::control::Control, state::State};

/// What the screen reader can read, as plain data.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Shell {
    /// The menu bar's items, left to right.
    pub bar: Vec<String>,
    /// Dock icons: name, running.
    pub dock: Vec<(String, bool)>,
    /// The open Control Center's toggles: name, on.
    pub controls: Option<Vec<(String, bool)>>,
}

const ROOT: NodeId = NodeId(1);
const BAR: NodeId = NodeId(2);
const DOCK: NodeId = NodeId(3);
const CONTROL: NodeId = NodeId(4);
const BAR_ITEM: u64 = 100;
const DOCK_ITEM: u64 = 1000;
const CONTROL_ITEM: u64 = 2000;

/// The whole tree; every update replaces it.
pub fn tree(shell: &Shell) -> TreeUpdate {
    let mut nodes = Vec::new();
    let group = |id: NodeId, role, label: &str, items: Vec<(NodeId, Node)>, nodes: &mut Vec<(NodeId, Node)>| {
        let mut node = Node::new(role);
        node.set_label(label);
        node.set_children(items.iter().map(|(id, _)| *id).collect::<Vec<_>>());
        nodes.push((id, node));
        nodes.extend(items);
    };
    let bar = shell.bar.iter().enumerate().map(|(i, name)| {
        let mut node = Node::new(Role::MenuItem);
        node.set_label(name.as_str());
        (NodeId(BAR_ITEM + i as u64), node)
    });
    group(BAR, Role::MenuBar, "Menu Bar", bar.collect(), &mut nodes);
    let dock = shell.dock.iter().enumerate().map(|(i, (name, running))| {
        let mut node = Node::new(Role::Button);
        node.set_label(if *running { format!("{name}, running") } else { name.clone() });
        node.add_action(Action::Click);
        (NodeId(DOCK_ITEM + i as u64), node)
    });
    group(DOCK, Role::Toolbar, "Dock", dock.collect(), &mut nodes);
    let mut children = vec![BAR, DOCK];
    if let Some(controls) = &shell.controls {
        let toggles = controls.iter().enumerate().map(|(i, (name, on))| {
            let mut node = Node::new(Role::Switch);
            node.set_label(name.as_str());
            node.set_toggled(if *on { Toggled::True } else { Toggled::False });
            node.add_action(Action::Click);
            (NodeId(CONTROL_ITEM + i as u64), node)
        });
        group(CONTROL, Role::Dialog, "Control Center", toggles.collect(), &mut nodes);
        children.push(CONTROL);
    }
    let mut root = Node::new(Role::Window);
    root.set_label("Desktop");
    root.set_children(children);
    nodes.push((ROOT, root));
    let focus = if shell.controls.is_some() { CONTROL } else { ROOT };
    TreeUpdate { nodes, tree: Some(TreeInfo::new(ROOT)), tree_id: TreeId::ROOT, focus }
}

/// The adapter and the last tree it got.
pub struct Access {
    adapter: accesskit_unix::Adapter,
    shell: Arc<Mutex<Shell>>,
}

struct Initial(Arc<Mutex<Shell>>);

impl ActivationHandler for Initial {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        self.0.lock().ok().map(|shell| tree(&shell))
    }
}

struct Pressed(Sender<u64>);

impl ActionHandler for Pressed {
    fn do_action(&mut self, request: ActionRequest) {
        if request.action == Action::Click {
            let _ = self.0.send(request.target_node.0);
        }
    }
}

struct Quiet;

impl DeactivationHandler for Quiet {
    fn deactivate_accessibility(&mut self) {}
}

/// Control Center's toggles, in reading order.
const TOGGLES: [(Control, &str); 6] = [
    (Control::Wifi, "Wi-Fi"),
    (Control::Bluetooth, "Bluetooth"),
    (Control::Dnd, "Do Not Disturb"),
    (Control::Dark, "Dark Mode"),
    (Control::NightLight, "Night Light"),
    (Control::KeepAwake, "Keep Awake"),
];

impl State {
    /// The screen reader came or went: our widgets appear to it or leave.
    pub fn access_enable(&mut self, on: bool) {
        if !on {
            self.reader_tree = None;
            return;
        }
        if self.reader_tree.is_some() {
            return;
        }
        let (tx, rx) = channel::channel();
        let inserted = self.loop_handle.insert_source(rx, |event, _, state| {
            if let channel::Event::Msg(id) = event {
                state.access_press(id);
            }
        });
        if let Err(err) = inserted {
            return tracing::warn!("accessibility: {err}");
        }
        let shell = Arc::new(Mutex::new(self.access_shell()));
        // ponytail: the adapter's bus thread stays after the screen reader is turned off, until we exit.
        let adapter = accesskit_unix::Adapter::new(Initial(shell.clone()), Pressed(tx), Quiet);
        self.reader_tree = Some(Access { adapter, shell });
    }

    fn access_shell(&self) -> Shell {
        let status = self.control_status();
        let mut bar = vec!["Noobsbot menu".to_owned()];
        if let Some(content) = self.menu_bar() {
            bar.extend(content.app.map(|id| crate::render::menubar::app_name(&id)));
            bar.extend(status.wifi.as_ref().map(|(_, name)| format!("Wi-Fi, {name}")));
            bar.extend(status.bluetooth.as_ref().map(|(_, name)| format!("Bluetooth, {name}")));
            bar.extend(status.battery.map(|(p, charging)| format!("Battery {p}%{}", if charging { ", charging" } else { "" })));
            bar.extend(status.volume.map(|(v, muted)| if muted { "Volume, muted".to_owned() } else { format!("Volume {v}%") }));
            bar.push("Control Center".into());
            bar.push(content.clock);
        }
        let dock = self.dock_items().iter().map(|i| (i.title().to_owned(), !i.windows.is_empty())).collect();
        let controls = matches!(self.popup, Some(crate::shell::Popup::Control)).then(|| {
            let on = [
                status.wifi.as_ref().map(|w| w.0),
                status.bluetooth.as_ref().map(|b| b.0),
                Some(status.dnd),
                Some(status.dark),
                Some(status.night),
                Some(status.awake),
            ];
            TOGGLES.iter().zip(on).filter_map(|((_, name), on)| Some(((*name).to_owned(), on?))).collect()
        });
        Shell { bar, dock, controls }
    }

    /// Once per loop turn: a changed shell reaches the screen reader.
    // ponytail: rebuilds the snapshot every turn while the reader is on; diff per widget if that shows in a profile.
    pub fn access_frame(&mut self) {
        if self.reader_tree.is_none() {
            return;
        }
        let shell = self.access_shell();
        let Some(access) = self.reader_tree.as_mut() else { return };
        let Ok(mut last) = access.shell.lock() else { return };
        if *last == shell {
            return;
        }
        let focused = shell.controls.is_some();
        *last = shell.clone();
        drop(last);
        access.adapter.update_window_focus_state(focused);
        access.adapter.update_if_active(|| tree(&shell));
    }

    /// The screen reader pressed a dock icon or a Control Center toggle.
    fn access_press(&mut self, id: u64) {
        if let Some(i) = id.checked_sub(DOCK_ITEM).filter(|i| *i < CONTROL_ITEM - DOCK_ITEM) {
            if let Some(item) = self.dock_items().into_iter().nth(i as usize) {
                self.dock_click(item, false);
            }
        } else if let Some(i) = id.checked_sub(CONTROL_ITEM) {
            // The shown toggles skip a missing service: count only those.
            let status = self.control_status();
            let present = [status.wifi.is_some(), status.bluetooth.is_some(), true, true, true, true];
            let control = TOGGLES.iter().zip(present).filter(|(_, p)| *p).nth(i as usize).map(|((c, _), _)| *c);
            if let Some(rect) = control.and_then(|c| self.control_layout().rect_of(c)) {
                self.control_press(rect.to_f64().loc + rect.to_f64().size.downscale(2.0));
            }
        }
        self.schedule_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_names_every_widget_and_focuses_an_open_control_center() {
        let shell = Shell { bar: vec!["Clock".into()], dock: vec![("Files".into(), true)], controls: None };
        let update = tree(&shell);
        let node = |id: u64| update.nodes.iter().find(|(n, _)| n.0 == id).map(|(_, node)| node.clone());
        assert_eq!(node(DOCK_ITEM).and_then(|n| n.label().map(str::to_owned)), Some("Files, running".into()));
        assert_eq!(node(BAR_ITEM).map(|n| n.role()), Some(Role::MenuItem));
        assert_eq!(update.focus, ROOT);
        assert!(node(CONTROL.0).is_none());

        let open = tree(&Shell { controls: Some(vec![("Wi-Fi".into(), true)]), ..shell });
        assert_eq!(open.focus, CONTROL);
        let wifi = open.nodes.iter().find(|(n, _)| n.0 == CONTROL_ITEM).map(|(_, n)| n.clone());
        assert_eq!(wifi.and_then(|n| n.toggled()), Some(Toggled::True));
        // Every node but the root is some node's child.
        let children: Vec<NodeId> = open.nodes.iter().flat_map(|(_, n)| n.children().to_vec()).collect();
        assert!(open.nodes.iter().all(|(id, _)| *id == ROOT || children.contains(id)));
    }
}
