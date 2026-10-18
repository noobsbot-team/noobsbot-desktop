//! The dock (E3.1): the owner's Plank dock (Transparent-Tight theme) rebuilt in the shell. Pinned apps from
//! `[dock] items`, then running apps; a dot under each running one; click to launch, focus or minimise; Plank's
//! parabolic zoom under the pointer; intelligent hide. Layout is a pure function so drawing and clicks share it.

use std::{
    io::Read,
    os::fd::AsFd,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use smithay::{
    desktop::Window,
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{Resource, protocol::wl_data_source::WlDataSource},
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Size},
    wayland::{compositor::with_states, selection::data_device::with_source_metadata, shell::xdg::XdgToplevelSurfaceData},
};

use crate::{
    apps::App,
    config::{self, Dock, DockHide},
    shell::Row,
    shortcuts::Action,
    state::State,
    wm::Rect,
};

/// Icons never shrink below this to fit the screen (D13).
pub const MIN_ICON: i32 = 32;
/// Plank's theme units are tenths of the icon size: ItemPadding 0.5 → gap 0.05 × icon, BottomPadding 2 → 0.2 × icon.
const ITEM_PAD: f64 = 0.05;
const BOTTOM: f64 = 0.2;
/// TopPadding -12: the bar stands behind the lower half of the icons only; the top half rises above it (E3.1 design).
const BAR_COVER: f64 = 0.5;
/// How far the zoom reaches either side of the pointer, in icon steps (Plank-style parabola).
const ZOOM_REACH: f64 = 2.5;

/// Where everything is, output-relative.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// One icon square per item, zoomed.
    pub icons: Vec<Rectangle<f64, Logical>>,
    /// Each item's click column: its icon plus the gaps beside it.
    pub columns: Vec<(f64, f64)>,
    /// The Transparent-Tight background bar.
    pub bar: Rect,
    /// The unzoomed dock: where the pointer zooms and clicks.
    pub zone: Rect,
    /// The unzoomed icon size after fitting.
    pub icon: f64,
}

/// The icon size for `n` items on a `width`-wide screen: `icon`, shrunk until they fit (D13), never below MIN_ICON.
pub fn fit(n: usize, width: i32, icon: i32) -> f64 {
    let fits = f64::from(width) / (n.max(1) as f64 * (1.0 + ITEM_PAD));
    f64::from(icon).min(fits.floor()).max(f64::from(MIN_ICON))
}

/// What windows keep clear of at the bottom of the screen: the dock at its configured size.
pub fn reserve(dock: &Dock) -> i32 {
    (f64::from(dock.icon_size) * (1.0 + BOTTOM)).round() as i32
}

/// The dock of `n` items on an `output`-sized screen, zoomed to `zoom` (1.5 = 150%) around `pointer` when the pointer is
/// over it. The item under the pointer stays under it while the others grow, so the dock widens away from it.
pub fn layout(n: usize, output: Size<i32, Logical>, icon: i32, zoom: f64, pointer: Option<Point<f64, Logical>>) -> Layout {
    let s = fit(n, output.w, icon);
    let step = s * (1.0 + ITEM_PAD);
    let bottom = (s * BOTTOM).round();
    let (w, h) = (f64::from(output.w), f64::from(output.h));
    let left = ((w - step * n as f64) / 2.0).round();
    let zone = Rectangle::new((left as i32, (h - s - bottom) as i32).into(), ((step * n as f64).round() as i32, (s + bottom) as i32).into());
    let pointer = pointer.filter(|p| n > 0 && zoom > 1.0 && zone.to_f64().contains(*p));
    let scale = |i: usize| {
        let Some(p) = pointer else { return 1.0 };
        let d = (p.x - (left + step * (i as f64 + 0.5))).abs() / (ZOOM_REACH * step);
        1.0 + (zoom - 1.0) * (1.0 - d * d).max(0.0)
    };
    let widths: Vec<f64> = (0..n).map(|i| step * scale(i)).collect();
    // The first column's left edge: the pointer keeps its fraction of the item it is over.
    let start = match pointer {
        Some(p) => {
            let k = (((p.x - left) / step).floor() as usize).min(n - 1);
            let fraction = (p.x - left - step * k as f64) / step;
            p.x - fraction * widths[k] - widths[..k].iter().sum::<f64>()
        }
        None => left,
    };
    let mut x = start;
    let (mut icons, mut columns) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for (i, width) in widths.iter().enumerate() {
        let size = s * scale(i);
        icons.push(Rectangle::new((x + (width - size) / 2.0, h - bottom - size).into(), (size, size).into()));
        columns.push((x, x + width));
        x += width;
    }
    let bar_top = (h - bottom - s * BAR_COVER).round() as i32;
    let bar = Rectangle::new((start.round() as i32, bar_top).into(), ((x - start).round() as i32, output.h - bar_top).into());
    Layout { icons, columns, bar, zone, icon: s }
}

/// The item a press at `p` hits: inside the unzoomed dock, by column.
pub fn hit(layout: &Layout, p: Point<f64, Logical>) -> Option<usize> {
    if !layout.zone.to_f64().contains(p) {
        return None;
    }
    layout.columns.iter().position(|(l, r)| p.x >= *l && p.x < *r)
}

/// One dock icon.
#[derive(Debug, Clone)]
pub struct Item {
    /// The desktop-file id, or the window's app id when no entry matches.
    pub key: String,
    pub app: Option<App>,
    /// Shown and minimised, topmost shown first.
    pub windows: Vec<Window>,
}

impl Item {
    pub fn icon(&self) -> &str {
        self.app.as_ref().map_or(&self.key, |a| &a.icon)
    }
}

/// The order windows first showed up in the dock, so running apps keep their place when focus restacks windows.
struct Seen(u64);
static SEEN: AtomicU64 = AtomicU64::new(0);

fn seen(window: &Window) -> u64 {
    window.user_data().get_or_insert(|| Seen(SEEN.fetch_add(1, Ordering::Relaxed))).0
}

/// Linux input codes of the mouse buttons the dock answers.
const BTN_LEFT: u32 = 0x110;
const BTN_MIDDLE: u32 = 0x112;
const BTN_RIGHT: u32 = 0x111;

impl State {
    /// The app a window belongs to: its app id, else (noobsbot and others set none) its process name.
    pub fn app_key(&self, window: &Window) -> Option<String> {
        let toplevel = window.toplevel()?;
        let id = with_states(toplevel.wl_surface(), |s| s.data_map.get::<XdgToplevelSurfaceData>().and_then(|d| d.lock().ok()?.app_id.clone()));
        id.filter(|id| !id.is_empty()).or_else(|| {
            let pid = toplevel.wl_surface().client()?.get_credentials(&self.display_handle).ok()?.pid;
            Some(std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?.trim().to_owned())
        })
    }

    /// Pinned apps that are installed, in config order, then running apps in the order they started.
    pub fn dock_items(&self) -> Vec<Item> {
        let mut items: Vec<Item> = Vec::new();
        for name in &self.config.dock.items {
            if let Some(app) = self.apps.find(name).filter(|a| !items.iter().any(|i| i.key == a.id)) {
                items.push(Item { key: app.id.clone(), app: Some(app.clone()), windows: Vec::new() });
            }
        }
        let mut windows: Vec<Window> = self.space.elements().rev().chain(self.hidden.iter().map(|(w, _)| w)).cloned().collect();
        windows.sort_by_key(seen);
        for window in windows {
            let Some(raw) = self.app_key(&window) else { continue };
            let app = self.apps.find(&raw);
            let key = app.map_or(raw, |a| a.id.clone());
            match items.iter_mut().find(|i| i.key == key) {
                Some(item) => item.windows.push(window),
                None => items.push(Item { key, app: app.cloned(), windows: vec![window] }),
            }
        }
        // Topmost shown window first within each app.
        let stack: Vec<&Window> = self.space.elements().rev().collect();
        for item in &mut items {
            item.windows.sort_by_key(|w| stack.iter().position(|s| *s == w).unwrap_or(usize::MAX));
        }
        self.dock_ui.preview(items)
    }

    /// The dock's layout for `n` items, zoomed around the pointer (or `--show dock-zoom`'s fake one).
    pub fn dock_layout(&self, n: usize) -> Option<Layout> {
        let size = self.space.outputs().next().and_then(|o| self.space.output_geometry(o))?.size;
        let dock = &self.config.dock;
        let zoom = if dock.zoom { f64::from(dock.zoom_percent) / 100.0 } else { 1.0 };
        let pointer = self.dock_pointer.or_else(|| self.seat.get_pointer().map(|p| self.to_output(p.current_location())));
        Some(layout(n, size, i32::from(dock.icon_size), zoom, pointer))
    }

    /// Shown, unless a fullscreen window has focus, or (intelligent hide) the focused window overlaps it and the pointer
    /// hasn't called it up from the bottom edge.
    pub fn dock_visible(&self, layout: &Layout) -> bool {
        let focused = self.focused_window();
        let state = |s| focused.as_ref().and_then(|w| w.toplevel()).is_some_and(|t| t.current_state().states.contains(s));
        if state(xdg_toplevel::State::Fullscreen) {
            return false;
        }
        let origin = self.to_output((0.0, 0.0).into()).to_i32_round::<i32>();
        let overlaps = focused.and_then(|w| self.frame(&w)).is_some_and(|f| Rectangle::new(f.loc + origin, f.size).overlaps(layout.zone));
        let menu = self.popup == Some(crate::shell::Popup::Menu(crate::shell::Menu::Dock));
        self.config.dock.hide == DockHide::Never || self.dock_reveal || menu || self.dock_ui.drag.is_some() || !overlaps
    }

    /// A cheap first check for every pointer motion: is `location` low enough to be on the dock at all?
    fn near_dock(&self, location: Point<f64, Logical>) -> bool {
        let height = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map_or(0, |g| g.size.h);
        location.y >= f64::from(height - reserve(&self.config.dock))
    }

    /// Whether the dock takes the pointer at `location` (output-relative).
    pub fn dock_covers(&self, location: Point<f64, Logical>) -> bool {
        if !self.near_dock(location) {
            return false;
        }
        let items = self.dock_items();
        self.dock_layout(items.len()).is_some_and(|l| !items.is_empty() && self.dock_visible(&l) && l.zone.to_f64().contains(location))
    }

    /// A press at `location`; `true` if the dock took it. With a popup open the shell closes it first.
    pub fn dock_press(&mut self, location: Point<f64, Logical>, button: u32) -> bool {
        if self.popup.is_some() || self.alert.is_some() {
            return false;
        }
        let items = self.dock_items();
        let Some(layout) = self.dock_layout(items.len()).filter(|l| self.dock_visible(l)) else { return false };
        if !layout.zone.to_f64().contains(location) {
            return false;
        }
        let Some((i, item)) = hit(&layout, location).and_then(|i| Some((i, items.into_iter().nth(i)?))) else { return true };
        match button {
            // A click acts on release, unless it became a drag (`dock_release`).
            BTN_LEFT => self.dock_ui.drag = Some(Drag { key: item.key, start: location, moved: false, slot: None }),
            BTN_MIDDLE => self.launch(item.app.as_ref()),
            BTN_RIGHT if self.modifiers().ctrl => self.dock_settings(),
            BTN_RIGHT => self.dock_open_menu(item.key, &layout, i),
            // ponytail: badges and the Plank importer are the rest of E3.1, not built yet.
            _ => {}
        }
        true
    }

    /// Left click: launches a closed app, minimises the focused one (Plank), else brings the app's windows forward
    /// (minimised ones too) and focuses its topmost.
    fn dock_activate(&mut self, item: Item) {
        if item.windows.is_empty() {
            return self.launch(item.app.as_ref());
        }
        let shown: Vec<Window> = item.windows.iter().filter(|w| self.space.elements().any(|s| s == *w)).cloned().collect();
        if self.focused_window().is_some_and(|f| shown.contains(&f)) {
            return self.hide_windows(shown);
        }
        let restored: Vec<(Window, Point<i32, Logical>)> = self.hidden.iter().filter(|(w, _)| item.windows.contains(w)).cloned().collect();
        self.hidden.retain(|(w, _)| !item.windows.contains(w));
        for (window, loc) in &restored {
            self.set_visible_loc(window, *loc);
        }
        // Bottom first, so their order is kept and the topmost ends on top with focus.
        for window in shown.iter().rev().skip(1) {
            self.space.raise_element(window, false);
        }
        let top = shown.first().or(restored.last().map(|(w, _)| w)).cloned();
        self.focus_window(top, SERIAL_COUNTER.next_serial());
        self.schedule_redraw();
    }

    /// Starts `app` from its `Exec` line.
    // ponytail: no launch bounce (Plank's 600 ms bounce needs the animation module); Terminal=true apps start without
    // a terminal.
    fn launch(&mut self, app: Option<&App>) {
        let Some(app) = app else { return };
        tracing::info!(app = app.id, "dock: launch");
        self.spawn(&app.command());
    }

    /// A wheel step over an icon cycles through that app's windows. `true`: over the dock, so apps don't get it.
    pub fn dock_scroll(&mut self, location: Point<f64, Logical>, step: bool) -> bool {
        if !self.dock_covers(location) {
            return false;
        }
        let items = self.dock_items();
        let Some(item) = self.dock_layout(items.len()).and_then(|l| hit(&l, location)).and_then(|i| items.into_iter().nth(i)) else { return true };
        let shown: Vec<Window> = item.windows.iter().filter(|w| self.space.elements().any(|s| s == *w)).cloned().collect();
        // The app's bottom window comes to the top: repeated steps go round all of them.
        if let Some(next) = shown.last().filter(|_| step).cloned() {
            self.focus_window(Some(next), SERIAL_COUNTER.next_serial());
            self.schedule_redraw();
        }
        true
    }

    /// The pointer moved: redraw while it zooms the dock; touching the bottom edge calls up a hidden dock, leaving it
    /// lets it hide again.
    pub fn dock_motion(&mut self, from: Point<f64, Logical>, to: Point<f64, Logical>) {
        self.dock_drag_motion(to);
        if !self.dock_reveal && !self.near_dock(from) && !self.near_dock(to) {
            return;
        }
        let n = self.dock_items().len();
        let (Some(layout), Some(size)) = (self.dock_layout(n), self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map(|g| g.size)) else {
            return;
        };
        let zone = layout.zone.to_f64();
        let edge = to.y >= f64::from(size.h) - 1.0;
        let reveal = if edge { true } else { self.dock_reveal && zone.contains(to) };
        if reveal != self.dock_reveal || (self.config.dock.zoom && (zone.contains(from) || zone.contains(to))) {
            self.dock_reveal = reveal;
            self.schedule_redraw();
        }
    }
}

/// The dock's interactions: the right-click menu, dragging an icon, files dragged in from an app.
#[derive(Default)]
pub struct Ui {
    /// The item key the open menu is for, and the point (output-relative) it stands centred over.
    pub menu: Option<(String, Point<i32, Logical>)>,
    drag: Option<Drag>,
    /// The `text/uri-list` of the app drag in progress, read as it starts.
    uris: Arc<Mutex<Option<String>>>,
}

/// An icon held down by the left button.
struct Drag {
    key: String,
    start: Point<f64, Logical>,
    /// Past the threshold: a drag, not a click.
    moved: bool,
    /// Where it would land; `None`: off the dock (released there, a pinned icon is removed).
    slot: Option<usize>,
}

/// How far the pointer moves before a press becomes a drag.
const DRAG_START: f64 = 8.0;

impl Ui {
    /// `items` as the drag would leave them: the dragged icon moved to its slot.
    fn preview(&self, mut items: Vec<Item>) -> Vec<Item> {
        let Some((key, slot)) = self.drag.as_ref().and_then(|d| Some((&d.key, d.slot?))) else { return items };
        if let Some(from) = items.iter().position(|i| i.key == *key) {
            let item = items.remove(from);
            items.insert(slot.min(items.len()), item);
        }
        items
    }
}

/// A row of a dock icon's menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Brings the app's n-th window forward.
    Window(usize),
    /// Keep in Dock / Remove from Dock.
    Pin,
    /// New Window / Open.
    Open,
    /// Close All / Quit.
    Close,
}

/// Plank's menu: the app's windows, then Keep in Dock (or Remove from Dock) and New Window (or Open) for apps with a
/// desktop entry, then Close All (Quit with one window) while it runs.
pub fn menu_rows(titles: &[String], pinned: bool, launchable: bool) -> Vec<Option<Row>> {
    let row = |label: &str, command| Some(Row { label: label.to_owned(), shortcut: "", action: Some(Action::Dock(command)) });
    let mut groups: Vec<Vec<Option<Row>>> = vec![titles.iter().enumerate().map(|(i, t)| row(t, Command::Window(i))).collect()];
    if launchable {
        let open = if titles.is_empty() { "Open" } else { "New Window" };
        groups.push(vec![row(if pinned { "Remove from Dock" } else { "Keep in Dock" }, Command::Pin), row(open, Command::Open)]);
    }
    if !titles.is_empty() {
        groups.push(vec![row(if titles.len() == 1 { "Quit" } else { "Close All" }, Command::Close)]);
    }
    let mut rows = Vec::new();
    for group in groups.into_iter().filter(|g| !g.is_empty()) {
        if !rows.is_empty() {
            rows.push(None);
        }
        rows.extend(group);
    }
    rows
}

/// The slot an icon dragged to `x` lands in: the column under it, clamped to `0..=max`.
pub fn slot(layout: &Layout, x: f64, max: usize) -> usize {
    let n = layout.columns.iter().take_while(|(_, right)| x >= *right).count();
    n.min(max)
}

/// `[dock] items` after moving `name` to shown slot `to` (`None`: unpinning it). `shown[i]` is the index in `items` of
/// the i-th pinned icon on the dock, `from` the dragged icon's slot if it is pinned. Entries for apps that aren't
/// installed keep their place.
pub fn repin(items: &[String], shown: &[usize], name: &str, from: Option<usize>, to: Option<usize>) -> Vec<String> {
    let mut list: Vec<(String, bool)> = items.iter().enumerate().map(|(i, s)| (s.clone(), shown.contains(&i))).collect();
    if let Some(at) = from.and_then(|f| shown.get(f)) {
        list.remove(*at);
    }
    if let Some(to) = to {
        let at = list.iter().enumerate().filter(|(_, (_, shown))| *shown).nth(to).map_or(list.len(), |(i, _)| i);
        list.insert(at, (name.to_owned(), true));
    }
    list.into_iter().map(|(s, _)| s).collect()
}

/// `text` (a config.toml) with `[dock] items` set to `items`; every other line, comments included, is kept.
// ponytail: brackets inside quoted names or comments of a multi-line `items` array would confuse the line scan.
pub fn set_items(text: &str, items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|s| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))).collect();
    let value = format!("items = [{}]\n", quoted.join(", "));
    let mut lines: Vec<String> = text.split_inclusive('\n').map(String::from).collect();
    if let Some(last) = lines.last_mut().filter(|l| !l.ends_with('\n')) {
        last.push('\n');
    }
    let header = |l: &str| l.trim_start().starts_with('[');
    let Some(dock) = lines.iter().position(|l| l.trim_start().starts_with("[dock]")) else {
        let gap = if lines.is_empty() { "" } else { "\n" };
        return format!("{}{gap}[dock]\n{value}", lines.concat());
    };
    let end = lines[dock + 1..].iter().position(|l| header(l)).map_or(lines.len(), |i| dock + 1 + i);
    let key = |l: &str| l.trim_start().strip_prefix("items").is_some_and(|r| r.trim_start().starts_with('='));
    match (dock + 1..end).find(|&i| key(&lines[i])) {
        Some(start) => {
            // A multi-line array runs until its brackets close.
            let mut depth = 0;
            let mut last = start;
            for (i, line) in lines.iter().enumerate().skip(start) {
                let code = line.split('#').next().unwrap_or("");
                depth += code.matches('[').count() as i32 - code.matches(']').count() as i32;
                last = i;
                if depth <= 0 {
                    break;
                }
            }
            lines.splice(start..=last, [value]);
        }
        None => lines.insert(dock + 1, value),
    }
    lines.concat()
}

/// The URIs of a `text/uri-list` (comments and blank lines skipped).
pub fn uri_list(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from).collect()
}

impl State {
    /// The pinned icons on the dock: (index in `[dock] items`, app id), left to right.
    fn dock_pinned(&self) -> Vec<(usize, String)> {
        let mut pinned: Vec<(usize, String)> = Vec::new();
        for (i, name) in self.config.dock.items.iter().enumerate() {
            if let Some(app) = self.apps.find(name).filter(|a| !pinned.iter().any(|(_, id)| *id == a.id)) {
                pinned.push((i, app.id.clone()));
            }
        }
        pinned
    }

    fn dock_item(&self, key: &str) -> Option<Item> {
        self.dock_items().into_iter().find(|i| i.key == key)
    }

    /// The open dock menu's rows (empty without one).
    pub fn dock_menu_rows(&self) -> Vec<Option<Row>> {
        let Some(item) = self.dock_ui.menu.as_ref().and_then(|(key, _)| self.dock_item(key)) else { return Vec::new() };
        let name = item.app.as_ref().map_or(item.key.as_str(), |a| a.name.as_str());
        let titles: Vec<String> = item
            .windows
            .iter()
            .map(|w| {
                let title = w.toplevel().and_then(|t| with_states(t.wl_surface(), |s| s.data_map.get::<XdgToplevelSurfaceData>().and_then(|d| d.lock().ok()?.title.clone())));
                title.filter(|t| !t.is_empty()).unwrap_or_else(|| name.to_owned())
            })
            .collect();
        let pinned = self.dock_pinned().iter().any(|(_, id)| *id == item.key);
        menu_rows(&titles, pinned, item.app.is_some())
    }

    /// Right click: icon `i`'s menu, centred over it.
    fn dock_open_menu(&mut self, key: String, layout: &Layout, i: usize) {
        let (left, right) = layout.columns[i];
        let at = Point::from((((left + right) / 2.0).round() as i32, layout.icons[i].loc.y.round() as i32));
        self.dock_ui.menu = Some((key, at));
        self.popup = Some(crate::shell::Popup::Menu(crate::shell::Menu::Dock));
        self.schedule_redraw();
    }

    /// `--show dock-menu`: the first running app's menu, else the first icon's.
    pub fn dock_show_menu(&mut self) {
        let items = self.dock_items();
        let Some(layout) = self.dock_layout(items.len()) else { return };
        if let Some(i) = items.iter().position(|i| !i.windows.is_empty()).or((!items.is_empty()).then_some(0)) {
            self.dock_open_menu(items[i].key.clone(), &layout, i);
        }
    }

    /// Ctrl+right click: Plank's preferences, which are config.toml's `[dock]` until the Settings app (M7).
    fn dock_settings(&mut self) {
        let path = config::path();
        if !path.exists() {
            let _ = config::save_atomic(&path, config::DEFAULT_TOML).inspect_err(|err| tracing::warn!("{}: {err}", path.display()));
        }
        // ponytail: always Kate, the default editor; the Settings app's Dock pane replaces this.
        self.spawn(&["kate".to_owned(), path.to_string_lossy().into_owned()]);
    }

    pub fn dock_command(&mut self, command: Command) {
        let Some(item) = self.dock_ui.menu.as_ref().and_then(|(key, _)| self.dock_item(key)) else { return };
        match command {
            Command::Window(i) => {
                let Some(window) = item.windows.get(i).cloned() else { return };
                if let Some(at) = self.hidden.iter().position(|(w, _)| *w == window) {
                    let (_, loc) = self.hidden.remove(at);
                    self.set_visible_loc(&window, loc);
                }
                self.focus_window(Some(window), SERIAL_COUNTER.next_serial());
            }
            Command::Pin => {
                let pinned = self.dock_pinned();
                let to = (!pinned.iter().any(|(_, id)| *id == item.key)).then_some(pinned.len());
                self.dock_repin(&item.key, to);
            }
            Command::Open => self.launch(item.app.as_ref()),
            Command::Close => item.windows.iter().filter_map(|w| w.toplevel()).for_each(|t| t.send_close()),
        }
        self.schedule_redraw();
    }

    /// Moves `key` to pinned slot `to`, or unpins it, and saves `[dock] items`.
    fn dock_repin(&mut self, key: &str, to: Option<usize>) {
        let pinned = self.dock_pinned();
        let shown: Vec<usize> = pinned.iter().map(|(i, _)| *i).collect();
        let from = pinned.iter().position(|(_, id)| id == key);
        let items = repin(&self.config.dock.items, &shown, key, from, to);
        if items == self.config.dock.items {
            return;
        }
        // Into the file the user edits (through a dotfiles symlink), so the live reload agrees.
        let path = config::path();
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if let Err(err) = config::save_atomic(&path, &set_items(&text, &items)) {
            tracing::warn!("{}: {err}", path.display());
        }
        self.config.dock.items = items;
        self.schedule_redraw();
    }

    /// The pointer moved to `to` with an icon held: past the threshold it drags, and the icons make room.
    fn dock_drag_motion(&mut self, to: Point<f64, Logical>) {
        let Some(drag) = self.dock_ui.drag.as_ref() else { return };
        let (start, key) = (drag.start, drag.key.clone());
        if !drag.moved && (to.x - start.x).hypot(to.y - start.y) < DRAG_START {
            return;
        }
        let n = self.dock_items().len();
        let Some(layout) = self.dock_layout(n) else { return };
        let pinned = self.dock_pinned();
        let is_pinned = pinned.iter().any(|(_, id)| *id == key);
        // Slightly above the icons still counts as on the dock.
        let on = to.y >= f64::from(layout.zone.loc.y) - layout.icon && to.x >= f64::from(layout.zone.loc.x) - layout.icon;
        let max = if is_pinned { pinned.len().saturating_sub(1) } else { pinned.len() };
        let slot = on.then(|| slot(&layout, to.x, max));
        if let Some(drag) = self.dock_ui.drag.as_mut() {
            drag.moved = true;
            if drag.slot != slot {
                drag.slot = slot;
                self.schedule_redraw();
            }
        }
    }

    /// The left button came up: a click on the held icon, or the end of its drag (reorder, pin, or remove).
    pub fn dock_release(&mut self, _location: Point<f64, Logical>) {
        let Some(drag) = self.dock_ui.drag.take() else { return };
        let Some(item) = self.dock_item(&drag.key) else { return };
        if !drag.moved {
            return self.dock_activate(item);
        }
        let pinned = self.dock_pinned().iter().any(|(_, id)| *id == item.key);
        match drag.slot {
            Some(to) if item.app.is_some() => self.dock_repin(&item.key, Some(to)),
            // ponytail: removed at once, without Plank's poof animation or the icon following the pointer.
            None if pinned => self.dock_repin(&item.key, None),
            _ => {}
        }
        self.schedule_redraw();
    }

    /// An app started a drag: its `text/uri-list`, if it offers one, is read now, since a drop on the dock (no client
    /// surface) gets cancelled before we could ask.
    pub fn dock_drag_started(&mut self, source: Option<WlDataSource>) {
        let uris = self.dock_ui.uris.clone();
        *uris.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let Some(source) = source else { return };
        let offers = with_source_metadata(&source, |m| m.mime_types.iter().any(|t| t == URI_LIST)).unwrap_or(false);
        let Some((mut reader, writer)) = offers.then(|| std::io::pipe().ok()).flatten() else { return };
        source.send(URI_LIST.to_owned(), writer.as_fd());
        drop(writer);
        // ponytail: a drop before the app has written the list is ignored.
        crate::clipboard::transfer(move || {
            let mut text = String::new();
            if reader.by_ref().take(1 << 20).read_to_string(&mut text).is_ok() {
                *uris.lock().unwrap_or_else(|e| e.into_inner()) = Some(text);
            }
        });
    }

    /// An app's drag ended over no window: on the dock, `.desktop` files are pinned where they land, other files open
    /// with the app they land on.
    // ponytail: only `.desktop` files of installed apps (in an `applications/` dir) pin; others are ignored.
    pub fn dock_drop(&mut self) {
        let Some(location) = self.seat.get_pointer().map(|p| self.to_output(p.current_location())) else { return };
        let text = self.dock_ui.uris.lock().unwrap_or_else(|e| e.into_inner()).take();
        let uris = uri_list(&text.unwrap_or_default());
        if uris.is_empty() || !self.dock_covers(location) {
            return;
        }
        let items = self.dock_items();
        let Some(layout) = self.dock_layout(items.len()) else { return };
        if uris.iter().all(|u| u.ends_with(".desktop")) {
            let mut at = slot(&layout, location.x, self.dock_pinned().len());
            for uri in &uris {
                let stem = crate::apps::file_path(uri).and_then(|p| Some(std::path::Path::new(&p).file_stem()?.to_string_lossy().into_owned()));
                if let Some(id) = stem.and_then(|s| self.apps.find(&s).map(|a| a.id.clone())) {
                    self.dock_repin(&id, Some(at));
                    at += 1;
                }
            }
        } else if let Some(app) = hit(&layout, location).and_then(|i| items.into_iter().nth(i)).and_then(|i| i.app) {
            tracing::info!(app = app.id, files = uris.len(), "dock: open dropped files");
            self.spawn(&app.command_with(&uris));
        }
    }
}

const URI_LIST: &str = "text/uri-list";

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: (i32, i32) = (1920, 1200);

    #[test]
    fn icons_shrink_to_fit() {
        assert_eq!(fit(8, 1920, 84), 84.0);
        // 30 apps on 1280: 1280 / (30 × 1.05) = 40.6.
        assert_eq!(fit(30, 1280, 84), 40.0);
        assert_eq!(fit(100, 1280, 84), f64::from(MIN_ICON));
        assert_eq!(fit(0, 1280, 84), 84.0);
    }

    #[test]
    fn unzoomed_dock_is_centred_and_tight() {
        let l = layout(8, SCREEN.into(), 84, 1.5, None);
        // 8 steps of 84 × 1.05 = 705.6 px, centred; 84 px icons on a 17 px bottom band.
        assert_eq!(l.zone, Rectangle::new((607, 1099).into(), (706, 101).into()));
        assert_eq!(l.icons[0].size.w, 84.0);
        assert_eq!(l.icons[0].loc.y, 1200.0 - 17.0 - 84.0);
        assert!((l.icons[1].loc.x - l.icons[0].loc.x - 88.2).abs() < 1e-9, "4.2 px gaps");
        // The bar covers the icons' lower half and reaches the screen bottom, end to end.
        assert_eq!((l.bar.loc.y, l.bar.loc.y + l.bar.size.h), (1141, 1200));
        assert_eq!((l.bar.loc.x, l.bar.size.w), (607, 706));
    }

    #[test]
    fn zoom_peaks_under_the_pointer_and_keeps_it_there() {
        let flat = layout(8, SCREEN.into(), 84, 1.5, None);
        let centre = flat.icons[3].loc + Point::from((42.0, 42.0));
        let l = layout(8, SCREEN.into(), 84, 1.5, Some(centre));
        assert!((l.icons[3].size.w - 126.0).abs() < 1e-9, "150% under the pointer");
        assert!(l.icons[2].size.w < 126.0 && l.icons[2].size.w > l.icons[1].size.w && l.icons[1].size.w > l.icons[0].size.w);
        assert_eq!(l.icons[7].size.w, 84.0, "out of reach: unzoomed");
        // The hovered icon is still centred under the pointer, and the zoomed icon stands on the same baseline.
        assert!((l.icons[3].loc.x + 63.0 - centre.x).abs() < 1e-9);
        assert_eq!(l.icons[3].loc.y + 126.0, flat.icons[3].loc.y + 84.0);
        assert!(l.bar.size.w > flat.bar.size.w);
        assert_eq!(hit(&l, centre), Some(3));
        // Off the dock or with zoom off: flat.
        assert_eq!(layout(8, SCREEN.into(), 84, 1.5, Some((100.0, 100.0).into())), flat);
        assert_eq!(layout(8, SCREEN.into(), 84, 1.0, Some(centre)), flat);
    }

    #[test]
    fn hits_follow_columns_inside_the_zone() {
        let l = layout(3, SCREEN.into(), 84, 1.0, None);
        let x = |i: usize| (l.columns[i].0 + l.columns[i].1) / 2.0;
        assert_eq!(hit(&l, (x(0), 1150.0).into()), Some(0));
        assert_eq!(hit(&l, (x(2), 1199.0).into()), Some(2));
        assert_eq!(hit(&l, (x(1), 1000.0).into()), None, "above the dock");
        assert_eq!(hit(&l, (10.0, 1150.0).into()), None, "beside it");
        // Every pointer position along a zoomed dock hits the item it zooms.
        for px in (l.zone.loc.x..l.zone.loc.x + l.zone.size.w).step_by(7) {
            let p = (f64::from(px), 1150.0).into();
            let z = layout(3, SCREEN.into(), 84, 1.5, Some(p));
            let i = hit(&z, p).expect("inside");
            assert!(z.icons.iter().all(|r| r.size.w <= z.icons[i].size.w + 1e-9), "at {px}");
        }
    }

    #[test]
    fn menu_lists_windows_then_pin_and_open_then_close() {
        let labels = |rows: Vec<Option<Row>>| rows.into_iter().map(|r| r.map(|r| r.label)).collect::<Vec<_>>();
        let s = |l: &str| Some(l.to_owned());
        let two = ["a.txt".to_owned(), "b.txt".to_owned()];
        assert_eq!(labels(menu_rows(&two, true, true)), [s("a.txt"), s("b.txt"), None, s("Remove from Dock"), s("New Window"), None, s("Close All")]);
        assert_eq!(labels(menu_rows(&[], false, true)), [s("Keep in Dock"), s("Open")]);
        // No desktop entry: nothing to pin or launch.
        assert_eq!(labels(menu_rows(&two[..1], false, false)), [s("a.txt"), None, s("Quit")]);
        assert_eq!(menu_rows(&two, false, true)[1].as_ref().and_then(|r| r.action.clone()), Some(Action::Dock(Command::Window(1))));
    }

    #[test]
    fn drag_slots_follow_columns() {
        let l = layout(4, SCREEN.into(), 84, 1.0, None);
        let mid = |i: usize| (l.columns[i].0 + l.columns[i].1) / 2.0;
        assert_eq!(slot(&l, mid(0), 3), 0);
        assert_eq!(slot(&l, mid(2), 3), 2);
        assert_eq!(slot(&l, 0.0, 3), 0, "left of the dock");
        assert_eq!(slot(&l, 1900.0, 3), 3, "right of it");
        // Clamped to the pinned slots.
        assert_eq!(slot(&l, mid(3), 1), 1);
    }

    #[test]
    fn repin_keeps_uninstalled_entries_in_place() {
        let items: Vec<String> = ["chrome", "kate", "vscode", "dolphin", "kitty"].map(String::from).to_vec();
        // chrome and vscode aren't installed: shown slots 0, 1, 2 are kate, dolphin, kitty.
        let shown = [1, 3, 4];
        let v = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(repin(&items, &shown, "kitty", Some(2), Some(0)), v(&["chrome", "kitty", "kate", "vscode", "dolphin"]));
        assert_eq!(repin(&items, &shown, "kate", Some(0), Some(2)), v(&["chrome", "vscode", "dolphin", "kitty", "kate"]));
        assert_eq!(repin(&items, &shown, "dolphin", Some(1), None), v(&["chrome", "kate", "vscode", "kitty"]));
        // A running app pinned between kate and dolphin.
        assert_eq!(repin(&items, &shown, "helium", None, Some(1)), v(&["chrome", "kate", "vscode", "helium", "dolphin", "kitty"]));
        assert_eq!(repin(&items, &shown, "helium", None, Some(3)), v(&["chrome", "kate", "vscode", "dolphin", "kitty", "helium"]));
    }

    #[test]
    fn preview_moves_the_dragged_icon() {
        let item = |k: &str| Item { key: k.into(), app: None, windows: Vec::new() };
        let mut ui = Ui { drag: Some(Drag { key: "a".into(), start: (0.0, 0.0).into(), moved: true, slot: Some(2) }), ..Default::default() };
        let keys: Vec<String> = ui.preview(vec![item("a"), item("b"), item("c")]).into_iter().map(|i| i.key).collect();
        assert_eq!(keys, ["b", "c", "a"]);
        ui.drag.as_mut().unwrap().slot = None;
        assert_eq!(ui.preview(vec![item("a"), item("b")]).len(), 2, "off the dock it stays in place until released");
    }

    #[test]
    fn items_rewritten_in_place() {
        let names: Vec<String> = ["kate", "say \"hi\""].map(String::from).to_vec();
        let text = "# top\n[dock]\n# pinned\nitems = [\n  \"a\", # first\n  \"b\",\n]\nzoom = true\n\n[bindings]\nitems = 1\n";
        assert_eq!(set_items(text, &names), "# top\n[dock]\n# pinned\nitems = [\"kate\", \"say \\\"hi\\\"\"]\nzoom = true\n\n[bindings]\nitems = 1\n");
        assert_eq!(set_items("[dock]\nzoom = false", &names[..1]), "[dock]\nitems = [\"kate\"]\nzoom = false\n");
        assert_eq!(set_items("version = 1\n", &names[..1]), "version = 1\n\n[dock]\nitems = [\"kate\"]\n");
        // The written file reads back as the new list.
        let config = config::parse(&set_items(config::DEFAULT_TOML, &names)).unwrap();
        assert_eq!(config.dock.items, names);
        assert_eq!(uri_list("# c\r\nfile:///a.desktop\r\n\r\nfile:///b\n"), ["file:///a.desktop", "file:///b"]);
    }

    #[test]
    fn reserve_matches_the_zone() {
        assert_eq!(reserve(&Dock::default()), layout(8, SCREEN.into(), 84, 1.0, None).zone.size.h);
    }
}
