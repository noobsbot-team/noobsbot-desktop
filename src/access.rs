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
    /// The window highlighted in Mission Control, by title.
    pub mission: Option<String>,
    /// Open Spotlight.
    pub spotlight: Option<Search>,
}

/// A text field: its label, text, and whether it is a password (read as bullets).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Field {
    pub label: String,
    pub text: String,
    pub secure: bool,
}

impl Field {
    pub fn new(label: &str, field: &crate::ui::field::TextField) -> Self {
        Self { label: label.into(), text: field.text.clone(), secure: field.secure }
    }

    fn node(&self, role: Role) -> Node {
        let mut node = Node::new(if self.secure { Role::PasswordInput } else { role });
        node.set_label(self.label.as_str());
        node.set_value(if self.secure { "•".repeat(self.text.chars().count()) } else { self.text.clone() });
        node
    }
}

/// Spotlight: its field, result rows (name, kind) and the selected row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Search {
    pub field: Field,
    pub rows: Vec<(String, String)>,
    pub selected: usize,
}

const ROOT: NodeId = NodeId(1);
const BAR: NodeId = NodeId(2);
const DOCK: NodeId = NodeId(3);
const CONTROL: NodeId = NodeId(4);
const MISSION: NodeId = NodeId(5);
const SPOTLIGHT: NodeId = NodeId(6);
const SPOTLIGHT_FIELD: NodeId = NodeId(7);
const SPOTLIGHT_LIST: NodeId = NodeId(8);
const MISSION_ITEM: NodeId = NodeId(3000);
const BAR_ITEM: u64 = 100;
const DOCK_ITEM: u64 = 1000;
const CONTROL_ITEM: u64 = 2000;
const SPOTLIGHT_ITEM: u64 = 4000;

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
    if let Some(title) = &shell.mission {
        let mut item = Node::new(Role::ListItem);
        item.set_label(title.as_str());
        group(MISSION, Role::Dialog, "Mission Control", vec![(MISSION_ITEM, item)], &mut nodes);
        children.push(MISSION);
    }
    if let Some(search) = &shell.spotlight {
        let rows: Vec<_> = search
            .rows
            .iter()
            .enumerate()
            .map(|(i, (name, kind))| {
                let mut node = Node::new(Role::ListBoxOption);
                node.set_label(format!("{name}, {kind}"));
                node.set_position_in_set(i);
                node.set_selected(i == search.selected);
                (NodeId(SPOTLIGHT_ITEM + i as u64), node)
            })
            .collect();
        let mut field = search.field.node(Role::SearchInput);
        field.set_controls(vec![SPOTLIGHT_LIST]);
        // The field keeps the keyboard; the selected row is read as it moves.
        if search.selected < rows.len() {
            field.set_active_descendant(NodeId(SPOTLIGHT_ITEM + search.selected as u64));
        }
        let mut list = Node::new(Role::ListBox);
        list.set_label("Results");
        list.set_size_of_set(rows.len());
        list.set_children(rows.iter().map(|(id, _)| *id).collect::<Vec<_>>());
        group(SPOTLIGHT, Role::Dialog, "Spotlight", vec![(SPOTLIGHT_FIELD, field), (SPOTLIGHT_LIST, list)], &mut nodes);
        nodes.extend(rows);
        children.push(SPOTLIGHT);
    }
    let mut root = Node::new(Role::Window);
    root.set_label("Desktop");
    root.set_children(children);
    nodes.push((ROOT, root));
    TreeUpdate { nodes, tree: Some(TreeInfo::new(ROOT)), tree_id: TreeId::ROOT, focus: focus(shell) }
}

/// What has the keyboard: the topmost open surface, else the desktop.
fn focus(shell: &Shell) -> NodeId {
    if shell.spotlight.is_some() {
        SPOTLIGHT_FIELD
    } else if shell.mission.is_some() {
        MISSION_ITEM
    } else if shell.controls.is_some() {
        CONTROL
    } else {
        ROOT
    }
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
        let mission = self.mission.as_ref().filter(|_| self.mission_open()).and_then(|m| m.hover.as_ref()).map(crate::render::window_title);
        let spotlight = self.spotlight.as_ref().map(|s| {
            let hits = self.spotlight_results();
            // Only the rows that fit are drawn.
            let shown = self.spotlight_layout(hits.len()).map_or(0, |l| l.rows.len());
            let rows = hits.iter().take(shown).map(|h| (h.title(), h.kind().to_owned())).collect();
            Search { field: Field::new("Spotlight Search", &s.field), rows, selected: s.selected }
        });
        Shell { bar, dock, controls, mission, spotlight }
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
        let focused = focus(&shell) != ROOT;
        *last = shell.clone();
        drop(last);
        access.adapter.update_window_focus_state(focused);
        access.adapter.update_if_active(|| tree(&shell));
    }

    /// The screen reader pressed a dock icon or a Control Center toggle.
    fn access_press(&mut self, id: u64) {
        if (DOCK_ITEM..CONTROL_ITEM).contains(&id) {
            if let Some(item) = self.dock_items().into_iter().nth((id - DOCK_ITEM) as usize) {
                self.dock_click(item, false);
            }
        } else if let Some(i) = id.checked_sub(CONTROL_ITEM).filter(|i| *i < MISSION_ITEM.0 - CONTROL_ITEM) {
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
        let shell = Shell { bar: vec!["Clock".into()], dock: vec![("Files".into(), true)], ..Default::default() };
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

        let mission = tree(&Shell { mission: Some("Notes".into()), ..Default::default() });
        assert_eq!(mission.focus, MISSION_ITEM, "the highlighted window is read");
    }

    fn node(update: &TreeUpdate, id: NodeId) -> Node {
        update.nodes.iter().find(|(n, _)| *n == id).map(|(_, node)| node.clone()).unwrap_or_else(|| panic!("no node {id:?}"))
    }

    #[test]
    fn spotlight_reads_its_field_and_the_selected_row() {
        let field = Field { label: "Spotlight Search".into(), text: "fi".into(), secure: false };
        let rows = vec![("Files".into(), "Application".into()), ("Firefox".into(), "Application".into())];
        let update = tree(&Shell { spotlight: Some(Search { field, rows, selected: 1 }), ..Default::default() });
        assert_eq!(update.focus, SPOTLIGHT_FIELD);
        let field = node(&update, SPOTLIGHT_FIELD);
        assert_eq!((field.role(), field.value()), (Role::SearchInput, Some("fi")));
        assert_eq!(field.active_descendant(), Some(NodeId(SPOTLIGHT_ITEM + 1)), "the selected row is read");
        let row = node(&update, NodeId(SPOTLIGHT_ITEM + 1));
        assert_eq!((row.label(), row.is_selected()), (Some("Firefox, Application"), Some(true)));
        assert_eq!(node(&update, NodeId(SPOTLIGHT_ITEM)).is_selected(), Some(false));
        assert_eq!(node(&update, SPOTLIGHT_LIST).children().len(), 2);
    }

    #[test]
    fn a_password_field_reads_bullets() {
        let field = Field { label: "Password".into(), text: "hunter2".into(), secure: true }.node(Role::TextInput);
        assert_eq!((field.role(), field.value()), (Role::PasswordInput, Some("•••••••")));
    }
}
