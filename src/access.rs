//! Our shell widgets for the screen reader over AT-SPI (E8.6), as one accessible app: the menu bar, dock, Control
//! Center, Mission Control, Spotlight, Launchpad, notifications, alerts and Force Quit, the app switcher and the lock
//! screen, with names, roles and states; their buttons can be pressed. Banners are live regions, read as they come.
//! Built only while the screen reader is on: the adapter keeps a bus connection and a thread.

use std::sync::{Arc, Mutex};

use accesskit::{
    Action, ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, Live, Node, NodeId, Role, Toggled, TreeId, TreeInfo, TreeUpdate,
};
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
    /// Open Launchpad.
    pub launchpad: Option<Apps>,
    /// Banners, newest first; each is read once as it appears.
    pub banners: Vec<Note>,
    /// Open Notification Center's cards, newest first.
    pub center: Option<Vec<Note>>,
    /// An alert sheet (polkit's too) or Force Quit.
    pub dialog: Option<Dialog>,
    /// The shown app switcher: its apps and the selected one.
    pub switcher: Option<(Vec<String>, usize)>,
    /// The lock screen; while it is up the rest of the shell is hidden from the reader, banners aside.
    pub lock: Option<Locked>,
}

/// The lock screen: the user, the password field, a wrong-password hint, and what plays (`None`: no Now Playing card).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Locked {
    pub user: String,
    pub field: Field,
    pub hint: Option<String>,
    /// "Title, Artist" and whether it plays.
    pub playing: Option<(String, bool)>,
}

/// An alert or Force Quit: title, message, a field, a list (Force Quit's apps) with its selected row, the buttons
/// and the one Return presses.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Dialog {
    pub title: String,
    pub message: String,
    /// The message counts down each second: not read again on every change.
    pub ticking: bool,
    pub field: Option<Field>,
    pub rows: Vec<String>,
    pub selected: Option<usize>,
    pub buttons: Vec<String>,
    pub default: usize,
}

/// A notification: its body is `None` while previews are hidden; actions are (key, label).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Note {
    pub id: u32,
    pub app: String,
    pub title: String,
    pub body: Option<String>,
    pub critical: bool,
    pub actions: Vec<(String, String)>,
}

impl Note {
    /// What is read: "App, Title, Body".
    pub fn spoken(&self) -> String {
        let body = self.body.as_deref().filter(|b| !b.is_empty()).map(|b| format!(", {b}")).unwrap_or_default();
        format!("{}, {}{body}", self.app, self.title)
    }

    /// The card at `base`, pressable, with its actions as buttons after it.
    fn nodes(&self, base: u64, nodes: &mut Vec<(NodeId, Node)>) -> NodeId {
        let id = NodeId(base + ((self.id as u64) << 8));
        let buttons: Vec<(NodeId, Node)> = self
            .actions
            .iter()
            .enumerate()
            .map(|(j, (_, label))| {
                let mut button = Node::new(Role::Button);
                button.set_label(label.as_str());
                button.add_action(Action::Click);
                (NodeId(id.0 + 1 + j as u64), button)
            })
            .collect();
        let mut card = Node::new(if base == BANNER_ITEM { Role::Status } else { Role::ListItem });
        card.set_label(self.spoken());
        card.add_action(Action::Click);
        card.set_children(buttons.iter().map(|(id, _)| *id).collect::<Vec<_>>());
        nodes.push((id, card));
        nodes.extend(buttons);
        id
    }
}

/// Launchpad: its field, the shown page's tiles (name; a folder's app count), "Page 2 of 3" when there are several,
/// and the open folder's name (`tiles` are then its apps).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Apps {
    pub field: Field,
    pub tiles: Vec<(String, Option<usize>)>,
    pub page: Option<String>,
    pub folder: Option<String>,
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
const LAUNCHPAD: NodeId = NodeId(9);
const LAUNCHPAD_FIELD: NodeId = NodeId(10);
const LAUNCHPAD_PAGE: NodeId = NodeId(11);
const LAUNCHPAD_GRID: NodeId = NodeId(12);
const BANNERS: NodeId = NodeId(13);
const CENTER: NodeId = NodeId(14);
const DIALOG: NodeId = NodeId(15);
const DIALOG_MESSAGE: NodeId = NodeId(16);
const DIALOG_FIELD: NodeId = NodeId(17);
const DIALOG_LIST: NodeId = NodeId(18);
const SWITCHER: NodeId = NodeId(19);
const SWITCHER_ITEM: u64 = 8000;
const LOCK: NodeId = NodeId(20);
const LOCK_USER: NodeId = NodeId(21);
const LOCK_FIELD: NodeId = NodeId(22);
const LOCK_HINT: NodeId = NodeId(23);
const LOCK_MEDIA: NodeId = NodeId(24);
/// Now Playing's Previous, Play/Pause and Next.
const MEDIA_BUTTON: u64 = 9000;
/// Sleep, Restart and Shut Down.
const LOCK_BUTTON: u64 = 9100;
const DIALOG_ROW: u64 = 6000;
const DIALOG_BUTTON: u64 = 7000;
const MISSION_ITEM: NodeId = NodeId(3000);
const BAR_ITEM: u64 = 100;
const DOCK_ITEM: u64 = 1000;
const CONTROL_ITEM: u64 = 2000;
const SPOTLIGHT_ITEM: u64 = 4000;
const LAUNCHPAD_ITEM: u64 = 5000;
/// A notification's node is this plus its id << 8; its actions follow it.
const BANNER_ITEM: u64 = 1 << 40;
const CENTER_ITEM: u64 = 2 << 40;

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
    let mut children = Vec::new();
    // Locked, they are out of sight.
    if shell.lock.is_none() {
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
        children.extend([BAR, DOCK]);
    }
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
    if let Some(apps) = &shell.launchpad {
        let tiles = apps.tiles.iter().enumerate().map(|(i, (name, folder))| {
            let mut node = Node::new(Role::Button);
            node.set_label(match folder {
                Some(n) => format!("{name}, folder, {n} apps"),
                None => name.clone(),
            });
            node.add_action(Action::Click);
            (NodeId(LAUNCHPAD_ITEM + i as u64), node)
        });
        let mut grid = Vec::new();
        group(LAUNCHPAD_GRID, Role::Group, apps.folder.as_deref().unwrap_or("Applications"), tiles.collect(), &mut grid);
        let mut parts = vec![(LAUNCHPAD_FIELD, apps.field.node(Role::SearchInput))];
        if let Some(page) = &apps.page {
            let mut label = Node::new(Role::Label);
            label.set_value(page.as_str());
            parts.push((LAUNCHPAD_PAGE, label));
        }
        let ids: Vec<NodeId> = parts.iter().map(|(id, _)| *id).chain([LAUNCHPAD_GRID]).collect();
        let mut node = Node::new(Role::Dialog);
        node.set_label("Launchpad");
        node.set_children(ids);
        nodes.push((LAUNCHPAD, node));
        nodes.extend(parts);
        nodes.extend(grid);
        children.push(LAUNCHPAD);
    }
    if !shell.banners.is_empty() {
        let mut banners = Vec::new();
        let ids: Vec<NodeId> = shell
            .banners
            .iter()
            .map(|n| {
                let id = n.nodes(BANNER_ITEM, &mut banners);
                // A new banner is read as it is added; a critical one interrupts.
                if let Some((_, card)) = banners.iter_mut().find(|(i, _)| *i == id) {
                    card.set_live(if n.critical { Live::Assertive } else { Live::Polite });
                }
                id
            })
            .collect();
        let mut node = Node::new(Role::Group);
        node.set_label("Notifications");
        node.set_children(ids);
        nodes.push((BANNERS, node));
        nodes.extend(banners);
        children.push(BANNERS);
    }
    if let Some(cards) = &shell.center {
        let mut list = Vec::new();
        let ids: Vec<NodeId> = cards.iter().map(|n| n.nodes(CENTER_ITEM, &mut list)).collect();
        let mut node = Node::new(Role::Dialog);
        node.set_label("Notification Center");
        node.set_children(ids);
        nodes.push((CENTER, node));
        nodes.extend(list);
        children.push(CENTER);
    }
    if let Some(d) = &shell.dialog {
        let mut parts = Vec::new();
        let mut message = Node::new(Role::Label);
        message.set_value(d.message.as_str());
        // A changed message ("The password is incorrect.") is read.
        if !d.ticking {
            message.set_live(Live::Polite);
        }
        parts.push((DIALOG_MESSAGE, message));
        if let Some(field) = &d.field {
            parts.push((DIALOG_FIELD, field.node(Role::TextInput)));
        }
        if !d.rows.is_empty() {
            let rows: Vec<_> = d
                .rows
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    let mut row = Node::new(Role::ListBoxOption);
                    row.set_label(name.as_str());
                    row.set_position_in_set(i);
                    row.set_selected(d.selected == Some(i));
                    row.add_action(Action::Click);
                    (NodeId(DIALOG_ROW + i as u64), row)
                })
                .collect();
            let mut list = Node::new(Role::ListBox);
            list.set_size_of_set(rows.len());
            list.set_children(rows.iter().map(|(id, _)| *id).collect::<Vec<_>>());
            if let Some(i) = d.selected {
                list.set_active_descendant(NodeId(DIALOG_ROW + i as u64));
            }
            parts.push((DIALOG_LIST, list));
            parts.extend(rows);
        }
        let buttons: Vec<_> = d
            .buttons
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let mut button = Node::new(if i == d.default { Role::DefaultButton } else { Role::Button });
                button.set_label(label.as_str());
                button.add_action(Action::Click);
                (NodeId(DIALOG_BUTTON + i as u64), button)
            })
            .collect();
        let top = [DIALOG_MESSAGE, DIALOG_FIELD, DIALOG_LIST];
        let mut node = Node::new(Role::AlertDialog);
        node.set_label(d.title.as_str());
        node.set_description(d.message.as_str());
        node.set_children(parts.iter().map(|(id, _)| *id).filter(|id| top.contains(id)).chain(buttons.iter().map(|(id, _)| *id)).collect::<Vec<_>>());
        nodes.push((DIALOG, node));
        nodes.extend(parts);
        nodes.extend(buttons);
        children.push(DIALOG);
    }
    if let Some((apps, selected)) = &shell.switcher {
        let items = apps.iter().enumerate().map(|(i, name)| {
            let mut item = Node::new(Role::ListBoxOption);
            item.set_label(name.as_str());
            item.set_position_in_set(i);
            item.set_selected(i == *selected);
            (NodeId(SWITCHER_ITEM + i as u64), item)
        });
        group(SWITCHER, Role::ListBox, "App Switcher", items.collect(), &mut nodes);
        children.push(SWITCHER);
    }
    if let Some(lock) = &shell.lock {
        let button = |id: u64, label: &str| {
            let mut node = Node::new(Role::Button);
            node.set_label(label);
            node.add_action(Action::Click);
            (NodeId(id), node)
        };
        let mut user = Node::new(Role::Label);
        user.set_value(lock.user.as_str());
        let mut parts = vec![(LOCK_USER, user), (LOCK_FIELD, lock.field.node(Role::TextInput))];
        if let Some(hint) = &lock.hint {
            let mut node = Node::new(Role::Label);
            node.set_value(hint.as_str());
            node.set_live(Live::Assertive);
            parts.push((LOCK_HINT, node));
        }
        let mut media = Vec::new();
        if let Some((what, playing)) = &lock.playing {
            let play = button(MEDIA_BUTTON + 1, if *playing { "Pause" } else { "Play" });
            let buttons = vec![button(MEDIA_BUTTON, "Previous"), play, button(MEDIA_BUTTON + 2, "Next")];
            group(LOCK_MEDIA, Role::Group, &format!("Now Playing, {what}"), buttons, &mut media);
            parts.push(media.remove(0));
        }
        let power = crate::shell::lock::Button::ALL.iter().enumerate().map(|(i, b)| button(LOCK_BUTTON + i as u64, b.label()));
        parts.extend(power);
        let mut node = Node::new(Role::Dialog);
        node.set_label("Lock Screen");
        node.set_children(parts.iter().map(|(id, _)| *id).collect::<Vec<_>>());
        nodes.push((LOCK, node));
        nodes.extend(parts);
        nodes.extend(media);
        children.push(LOCK);
    }
    let mut root = Node::new(Role::Window);
    root.set_label("Desktop");
    root.set_children(children);
    nodes.push((ROOT, root));
    TreeUpdate { nodes, tree: Some(TreeInfo::new(ROOT)), tree_id: TreeId::ROOT, focus: focus(shell) }
}

/// What has the keyboard: the topmost open surface, else the desktop.
fn focus(shell: &Shell) -> NodeId {
    if shell.lock.is_some() {
        LOCK_FIELD
    } else if let Some(d) = &shell.dialog {
        if d.field.is_some() {
            DIALOG_FIELD
        } else if d.selected.is_some() {
            DIALOG_LIST
        } else {
            NodeId(DIALOG_BUTTON + d.default as u64)
        }
    } else if let Some((_, selected)) = &shell.switcher {
        // Focus follows the selection, so each app is read as it is stepped to.
        NodeId(SWITCHER_ITEM + *selected as u64)
    } else if shell.spotlight.is_some() {
        SPOTLIGHT_FIELD
    } else if let Some(apps) = &shell.launchpad {
        // An open folder takes typing as its new name.
        if apps.folder.is_some() { LAUNCHPAD_GRID } else { LAUNCHPAD_FIELD }
    } else if shell.mission.is_some() {
        MISSION_ITEM
    } else if shell.controls.is_some() {
        CONTROL
    } else if shell.center.is_some() {
        CENTER
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

    fn access_shell(&mut self) -> Shell {
        if let Some(lock) = &self.lock {
            // A lock client's surface speaks for itself.
            let lock = lock.client_surface().is_none().then(|| self.access_lock(lock));
            return Shell { lock, banners: self.access_notes().0, ..Default::default() };
        }
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
        let launchpad = self.access_launchpad();
        let (banners, center) = self.access_notes();
        let dialog = self.access_dialog();
        let switcher = self.switcher.open.as_ref().filter(|o| o.visible).map(|o| (o.apps.iter().map(|a| a.name.clone()).collect(), o.selected));
        Shell { bar, dock, controls, mission, spotlight, launchpad, banners, center, dialog, switcher, lock: None }
    }

    fn access_lock(&self, lock: &crate::shell::lock::Lock) -> Locked {
        let user = match &lock.greeter {
            Some(g) => g.users.get(g.user).map(|u| u.name.clone()).unwrap_or_default(),
            None => crate::pam::user().1,
        };
        let error = lock.greeter.as_ref().and_then(|g| g.error.clone());
        let hint = error.or(lock.wrong.map(|_| "Incorrect password".into()));
        let playing = self.media.as_ref().filter(|_| lock.greeter.is_none() && self.lock_layout().is_some_and(|l| l.media.is_some()));
        let playing = playing.map(|m| (if m.artist.is_empty() { m.title.clone() } else { format!("{}, {}", m.title, m.artist) }, m.playing));
        Locked { user, field: Field::new("Password", &lock.field), hint, playing }
    }

    /// The alert, else Force Quit.
    fn access_dialog(&mut self) -> Option<Dialog> {
        use crate::shell::alert::Kind;
        if let Some(alert) = &self.alert {
            let field = alert.field().map(|f| {
                let label = match &alert.kind {
                    Kind::Pin { passkey: true, .. } => "Passkey",
                    Kind::Pin { .. } => "PIN",
                    _ => "Password",
                };
                Field::new(label, f)
            });
            let buttons: Vec<String> = alert.cancellable().then(|| alert.cancel().to_owned()).into_iter().chain([alert.confirm().to_owned()]).collect();
            let default = buttons.len() - 1;
            let message = alert.body(std::time::Instant::now());
            return Some(Dialog { title: alert.title(), message, ticking: alert.deadline.is_some(), field, buttons, default, ..Default::default() });
        }
        self.force_quit.as_ref()?;
        let entries = self.force_quit_entries();
        let pid = self.force_quit.as_ref().and_then(|f| f.selection(&entries));
        Some(Dialog {
            title: "Force Quit Applications".into(),
            message: "If an app doesn't respond for a while, select its name and click Force Quit.".into(),
            rows: entries.iter().map(|e| if e.frozen { format!("{} (not responding)", e.name) } else { e.name.clone() }).collect(),
            selected: entries.iter().position(|e| Some(e.pid) == pid),
            buttons: vec!["Force Quit".into()],
            ..Default::default()
        })
    }

    /// The banners and Notification Center's cards.
    fn access_notes(&self) -> (Vec<Note>, Option<Vec<Note>>) {
        let preview = self.previews();
        let note = |n: &crate::services::notifications::Notification| Note {
            id: n.id,
            app: n.app.clone(),
            title: n.summary.clone(),
            body: preview.then(|| n.body.clone()),
            critical: n.urgency >= 2,
            actions: crate::shell::notify::buttons(n).cloned().collect(),
        };
        let store = &self.notifications;
        let center = matches!(self.popup, Some(crate::shell::Popup::Notifications { .. })).then(|| store.list.iter().rev().map(note).collect());
        // Banners stay in the tree under an open Notification Center, so closing it doesn't read them again.
        (store.banners.iter().filter_map(|id| store.get(*id)).map(note).collect(), center)
    }

    fn access_launchpad(&self) -> Option<Apps> {
        use crate::shell::launchpad::Tile;
        let l = self.launchpad.as_ref().filter(|_| self.launchpad_open())?;
        let per_page = self.launchpad_grid()?.per_page();
        let tiles = self.launchpad_tiles();
        let pages = crate::shell::launchpad::pages(tiles.len(), per_page);
        let page = (pages > 1).then(|| format!("Page {} of {pages}", l.page + 1));
        let folder = l.folder.as_ref().and_then(|(i, _)| match tiles.get(*i) {
            Some(Tile::Folder(name, apps)) => Some((name.clone(), apps.iter().map(|a| (a.name.clone(), None)).collect())),
            _ => None,
        });
        let (folder, tiles) = match folder {
            Some((name, apps)) => (Some(name), apps),
            None => {
                let shown = tiles.iter().skip(l.page * per_page).take(per_page);
                let tiles = shown.map(|t| match t {
                    Tile::App(a) => (a.name.clone(), None),
                    Tile::Folder(name, apps) => (name.clone(), Some(apps.len())),
                });
                (None, tiles.collect())
            }
        };
        Some(Apps { field: Field::new("Search", &l.field), tiles, page, folder })
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
        if (MEDIA_BUTTON..MEDIA_BUTTON + 3).contains(&id) {
            use crate::services::media::Action;
            let action = [Action::Previous, Action::PlayPause, Action::Next][(id - MEDIA_BUTTON) as usize];
            if let Some(media) = self.media.as_ref().filter(|_| self.locked()) {
                self.services.send(crate::services::Command::Media { player: media.player.clone(), action });
            }
        } else if (LOCK_BUTTON..LOCK_BUTTON + 3).contains(&id) && self.locked() {
            use crate::shell::{alert::Power, lock::Button};
            match Button::ALL[(id - LOCK_BUTTON) as usize] {
                Button::Sleep => self.sleep(),
                Button::Restart => self.power_now(Power::Restart),
                Button::ShutDown => self.power_now(Power::ShutDown),
            }
        } else if (DOCK_ITEM..CONTROL_ITEM).contains(&id) {
            if let Some(item) = self.dock_items().into_iter().nth((id - DOCK_ITEM) as usize) {
                self.dock_click(item, false);
            }
        } else if (DIALOG_ROW..DIALOG_BUTTON + 1000).contains(&id) {
            if let Some(alert) = &self.alert {
                // The last button is the default one.
                let last = alert.cancellable() as u64;
                self.alert_answer(if id == DIALOG_BUTTON + last { crate::shell::alert::Button::Confirm } else { crate::shell::alert::Button::Cancel });
            } else if self.force_quit.is_some() {
                let entries = self.force_quit_entries();
                match id.checked_sub(DIALOG_BUTTON) {
                    Some(_) => self.force_quit_selected(&entries),
                    None => self.force_quit = Some(crate::shell::force_quit::ForceQuit { selected: entries.get((id - DIALOG_ROW) as usize).map(|e| e.pid) }),
                }
            }
        } else if id >= BANNER_ITEM {
            let (note, j) = ((id >> 8) as u32, (id & 0xff) as usize);
            let center = id >= CENTER_ITEM;
            let key =
                j.checked_sub(1).and_then(|j| self.notifications.get(note).and_then(|n| crate::shell::notify::buttons(n).nth(j)).map(|(key, _)| key.clone()));
            match key {
                Some(key) => self.invoke(note, key),
                None if j == 0 => {
                    self.activate(note);
                    if center {
                        self.popup = None;
                    }
                }
                None => {}
            }
        } else if (LAUNCHPAD_ITEM..LAUNCHPAD_ITEM + 1000).contains(&id) {
            let i = (id - LAUNCHPAD_ITEM) as usize;
            let Some(l) = self.launchpad.as_ref() else { return };
            match (&l.folder, self.launchpad_grid()) {
                (Some((folder, _)), _) => self.launchpad_activate(*folder, Some(i)),
                (None, Some(grid)) => self.launchpad_activate(l.page * grid.per_page() + i, None),
                (None, None) => {}
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
    fn launchpad_reads_apps_folders_and_the_page() {
        let tiles = vec![("Files".into(), None), ("Games".into(), Some(4))];
        let apps = Apps { field: Field { label: "Search".into(), ..Default::default() }, tiles, page: Some("Page 2 of 3".into()), folder: None };
        let update = tree(&Shell { launchpad: Some(apps.clone()), ..Default::default() });
        assert_eq!(update.focus, LAUNCHPAD_FIELD);
        let files = node(&update, NodeId(LAUNCHPAD_ITEM));
        assert_eq!((files.role(), files.label()), (Role::Button, Some("Files")));
        assert!(files.supports_action(Action::Click));
        assert_eq!(node(&update, NodeId(LAUNCHPAD_ITEM + 1)).label(), Some("Games, folder, 4 apps"));
        assert_eq!(node(&update, LAUNCHPAD_PAGE).value(), Some("Page 2 of 3"));
        assert_eq!(node(&update, LAUNCHPAD).children(), [LAUNCHPAD_FIELD, LAUNCHPAD_PAGE, LAUNCHPAD_GRID]);

        let open = tree(&Shell { launchpad: Some(Apps { folder: Some("Games".into()), page: None, ..apps }), ..Default::default() });
        assert_eq!(open.focus, LAUNCHPAD_GRID);
        assert_eq!(node(&open, LAUNCHPAD_GRID).label(), Some("Games"));
        assert!(open.nodes.iter().all(|(id, _)| *id != LAUNCHPAD_PAGE));
    }

    #[test]
    fn banners_are_read_as_they_come_and_critical_ones_interrupt() {
        let reply = Note {
            id: 7,
            app: "Chat".into(),
            title: "Ana".into(),
            body: Some("Lunch?".into()),
            actions: vec![("reply".into(), "Reply".into())],
            ..Default::default()
        };
        assert_eq!(reply.spoken(), "Chat, Ana, Lunch?");
        let hidden = Note { id: 8, app: "Mail".into(), title: "New mail".into(), body: None, critical: true, ..Default::default() };
        assert_eq!(hidden.spoken(), "Mail, New mail", "a hidden preview keeps the body out");
        let update = tree(&Shell { banners: vec![reply.clone(), hidden], ..Default::default() });
        let card = node(&update, NodeId(BANNER_ITEM + (7 << 8)));
        assert_eq!((card.live(), card.label()), (Some(Live::Polite), Some("Chat, Ana, Lunch?")));
        assert_eq!(node(&update, NodeId(BANNER_ITEM + (8 << 8))).live(), Some(Live::Assertive));
        let button = node(&update, NodeId(BANNER_ITEM + (7 << 8) + 1));
        assert_eq!((button.role(), button.label()), (Role::Button, Some("Reply")));
        assert_eq!(update.focus, ROOT, "a banner doesn't take the keyboard");

        let center = tree(&Shell { center: Some(vec![reply]), ..Default::default() });
        assert_eq!(center.focus, CENTER);
        let card = node(&center, NodeId(CENTER_ITEM + (7 << 8)));
        assert_eq!((card.role(), card.live(), card.children().len()), (Role::ListItem, None, 1));
    }

    #[test]
    fn a_dialog_reads_its_title_message_and_default_button() {
        let field = Some(Field { label: "Password".into(), text: "ab".into(), secure: true });
        let dialog = Dialog {
            title: "Authentication is needed".into(),
            message: "Enter the password for “anil” to allow this.".into(),
            field,
            buttons: vec!["Cancel".into(), "OK".into()],
            default: 1,
            ..Default::default()
        };
        let update = tree(&Shell { dialog: Some(dialog.clone()), spotlight: Some(Search::default()), ..Default::default() });
        assert_eq!(update.focus, DIALOG_FIELD, "the dialog is above everything else");
        let sheet = node(&update, DIALOG);
        assert_eq!((sheet.role(), sheet.label(), sheet.description()), (Role::AlertDialog, Some("Authentication is needed"), Some(dialog.message.as_str())));
        assert_eq!(node(&update, DIALOG_FIELD).role(), Role::PasswordInput);
        assert_eq!(node(&update, NodeId(DIALOG_BUTTON)).role(), Role::Button);
        let ok = node(&update, NodeId(DIALOG_BUTTON + 1));
        assert_eq!((ok.role(), ok.label()), (Role::DefaultButton, Some("OK")));
        assert_eq!(node(&update, DIALOG_MESSAGE).live(), Some(Live::Polite), "a wrong password is read");

        let power = tree(&Shell { dialog: Some(Dialog { field: None, ticking: true, ..dialog }), ..Default::default() });
        assert_eq!(power.focus, NodeId(DIALOG_BUTTON + 1), "no field: the default button");
        assert_eq!(node(&power, DIALOG_MESSAGE).live(), None, "a countdown isn't read every second");

        let rows = vec!["Files".into(), "Notes (not responding)".into()];
        let quit = Dialog { title: "Force Quit Applications".into(), rows, selected: Some(1), buttons: vec!["Force Quit".into()], ..Default::default() };
        let quit = tree(&Shell { dialog: Some(quit), ..Default::default() });
        assert_eq!(quit.focus, DIALOG_LIST);
        assert_eq!(node(&quit, DIALOG_LIST).active_descendant(), Some(NodeId(DIALOG_ROW + 1)));
        assert_eq!(node(&quit, NodeId(DIALOG_ROW + 1)).is_selected(), Some(true));
        assert_eq!(node(&quit, NodeId(DIALOG_BUTTON)).role(), Role::DefaultButton);
    }

    #[test]
    fn the_switcher_focuses_the_selected_app() {
        let shell = Shell { switcher: Some((vec!["Files".into(), "Notes".into()], 1)), ..Default::default() };
        let update = tree(&shell);
        assert_eq!(update.focus, NodeId(SWITCHER_ITEM + 1));
        let notes = node(&update, NodeId(SWITCHER_ITEM + 1));
        assert_eq!((notes.label(), notes.is_selected()), (Some("Notes"), Some(true)));
        assert_eq!(node(&update, SWITCHER).label(), Some("App Switcher"));
    }

    #[test]
    fn the_lock_screen_reads_the_user_field_and_now_playing() {
        let field = Field { label: "Password".into(), text: "x".into(), secure: true };
        let lock = Locked { user: "Anil".into(), field, hint: Some("Incorrect password".into()), playing: Some(("Song, Band".into(), true)) };
        let update = tree(&Shell { lock: Some(lock), banners: vec![Note::default()], ..Default::default() });
        assert_eq!(update.focus, LOCK_FIELD);
        assert_eq!(node(&update, LOCK_FIELD).role(), Role::PasswordInput);
        assert_eq!(node(&update, LOCK_USER).value(), Some("Anil"));
        assert_eq!(node(&update, LOCK_HINT).live(), Some(Live::Assertive));
        assert_eq!(node(&update, LOCK_MEDIA).label(), Some("Now Playing, Song, Band"));
        let labels: Vec<_> = (0..3).map(|i| node(&update, NodeId(MEDIA_BUTTON + i)).label().map(str::to_owned)).collect();
        assert_eq!(labels, [Some("Previous".into()), Some("Pause".into()), Some("Next".into())]);
        assert_eq!(node(&update, NodeId(LOCK_BUTTON + 2)).label(), Some("Shut Down"));
        assert_eq!(node(&update, ROOT).children(), [BANNERS, LOCK]);
        let children: Vec<NodeId> = update.nodes.iter().flat_map(|(_, n)| n.children().to_vec()).collect();
        assert!(update.nodes.iter().all(|(id, _)| *id == ROOT || children.contains(id)), "every node is reachable");
    }

    #[test]
    fn a_password_field_reads_bullets() {
        let field = Field { label: "Password".into(), text: "hunter2".into(), secure: true }.node(Role::TextInput);
        assert_eq!((field.role(), field.value()), (Role::PasswordInput, Some("•••••••")));
    }
}
