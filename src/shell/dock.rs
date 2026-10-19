//! The dock (E3.1): the owner's Plank dock rebuilt in the shell, behaving as Plank does. Pinned items from
//! `[dock] items`, then running apps; dots under running ones; click to launch, focus or minimise; scroll to cycle
//! windows; drag to reorder, pin or remove; Plank's zoom under the pointer and its hide modes. Layout is a pure
//! function (Plank's geometry) so drawing and clicks share it.

use std::{
    cell::RefCell,
    io::Read,
    os::fd::AsFd,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use smithay::{
    desktop::Window,
    reexports::wayland_server::protocol::wl_data_source::WlDataSource,
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Size},
    wayland::{compositor::with_states, selection::data_device::with_source_metadata, shell::xdg::XdgToplevelSurfaceData},
};

use crate::{
    apps::App,
    config::{self, Dock},
    shell::{Row, dock_fx::CLICK_TIME, drawer, launchpad},
    shortcuts::Action,
    state::State,
    wm::Rect,
};

/// Icons never shrink below this to fit the screen (D13).
pub const MIN_ICON: i32 = 32;
/// Plank's largest icon size (Ctrl+scroll stops there).
pub const MAX_ICON: i32 = 128;
/// The bar stands behind the lower half of the icons only; the top half rises above it (our look).
const BAR_COVER: f64 = 0.5;

/// Plank's theme paddings are tenths of the icon size, truncated to whole px as Plank does. ItemPadding 0.5.
pub fn item_pad(icon: f64) -> f64 {
    (0.5 * icon / 10.0).trunc()
}

/// BottomPadding 2 (Transparent-Tight).
pub fn bottom_pad(icon: f64) -> f64 {
    (2.0 * icon / 10.0).trunc()
}

/// Where everything is, output-relative.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// One icon square per item, zoomed.
    pub icons: Vec<Rectangle<f64, Logical>>,
    /// Each item's hover column: its icon plus the item padding, centred on it (Plank's hover region).
    pub columns: Vec<(f64, f64)>,
    /// The background bar.
    pub bar: Rect,
    /// The unzoomed dock (Plank's static dock region): where the pointer zooms and clicks.
    pub zone: Rect,
    /// The unzoomed icon size after fitting.
    pub icon: f64,
}

/// The icon size for `n` items on a `width`-wide screen: `icon`, shrunk until they fit (D13), never below MIN_ICON.
pub fn fit(n: usize, width: i32, icon: i32) -> f64 {
    let fits = f64::from(width) / (n.max(1) as f64 * 1.05);
    f64::from(icon).min(fits.floor()).max(f64::from(MIN_ICON))
}

/// The dock's height at its configured size: what windows keep clear of when it never hides (Plank's struts).
pub fn reserve(dock: &Dock) -> i32 {
    let icon = f64::from(dock.icon_size);
    (icon + bottom_pad(icon)) as i32
}

/// The dock of `n` items on an `output`-sized screen, with Plank's zoom: icons within the zoomed icon size of the
/// `pointer` grow up to `zoom` (1.5 = 150%) along a parabola and the others move aside, all scaled by `progress`
/// (0..1, the zoom easing in and out).
pub fn layout(n: usize, output: Size<i32, Logical>, icon: i32, zoom: f64, progress: f64, pointer: Option<Point<f64, Logical>>) -> Layout {
    let s = fit(n, output.w, icon);
    let (pad, bottom) = (item_pad(s), bottom_pad(s));
    let step = s + pad;
    let (w, h) = (f64::from(output.w), f64::from(output.h));
    let left = ((w - step * n as f64) / 2.0).floor();
    let zone = Rectangle::new((left as i32, (h - s - bottom) as i32).into(), ((step * n as f64) as i32, (s + bottom) as i32).into());
    // Plank's ZoomIconSize: how far the zoom reaches, and the zoom at this point of the easing.
    let reach = (s * zoom).round();
    let now = 1.0 + (zoom - 1.0) * progress;
    let (mut icons, mut columns) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for i in 0..n {
        let rest = (left + step / 2.0).floor() + step * i as f64;
        let (mut centre, mut scale) = (rest, 1.0);
        if let Some(p) = pointer.filter(|_| now > 1.0) {
            let distance = (p.x - rest).abs().min(reach);
            let fraction = if distance / reach > 0.99 { 1.0 } else { distance / reach };
            let shift = distance * (now - 1.0) * (1.0 - fraction / 3.0);
            centre += if p.x > rest { -shift } else { shift };
            scale = 1.0 + (1.0 - fraction * fraction) * (now - 1.0);
            if scale == 1.0 {
                centre = centre.round();
            }
        }
        let size = (scale * s).round();
        icons.push(Rectangle::new((centre - size / 2.0, h - bottom - size).into(), (size, size).into()));
        columns.push((centre - (size + pad) / 2.0, centre + (size + pad) / 2.0));
    }
    let (start, end) = (columns.first().map_or(left, |c| c.0), columns.last().map_or(left, |c| c.1));
    let bar_top = (h - bottom - s * BAR_COVER).round() as i32;
    let bar = Rectangle::new((start.round() as i32, bar_top).into(), ((end - start).round() as i32, output.h - bar_top).into());
    Layout { icons, columns, bar, zone, icon: s }
}

/// The item whose hover region (its column, from its icon's top to the screen edge) holds `p`; the first wins.
pub fn hit(layout: &Layout, p: Point<f64, Logical>) -> Option<usize> {
    let bottom = f64::from(layout.zone.loc.y + layout.zone.size.h);
    layout.columns.iter().zip(&layout.icons).position(|((l, r), icon)| p.x >= *l && p.x < *r && p.y >= icon.loc.y && p.y < bottom)
}

/// The window after (`forward`) or before the active one of `n`, round; the first without an active one (Plank's
/// focus_next / focus_previous).
pub fn cycle(n: usize, active: Option<usize>, forward: bool) -> usize {
    match active {
        Some(i) if forward => (i + 1) % n,
        Some(i) => (i + n - 1) % n,
        None => 0,
    }
}

/// Plank's window names in the menu: the app's name (or a word of it) cut off "App - title", "title - App",
/// "App: title".
pub fn short_title(title: &str, app: &str) -> String {
    let lower = title.to_lowercase();
    // The byte offsets below come from `lower`: a title whose lowercase has another length (İ) is kept whole.
    if lower.len() != title.len() {
        return title.to_owned();
    }
    let mut names: Vec<String> = vec![app.to_lowercase()];
    names.extend(app.split([' ', '-', '–']).filter(|w| w.chars().count() >= 3).map(str::to_lowercase));
    for name in &names {
        for sep in [" - ", " – "] {
            if let Some(rest) = lower.strip_prefix(&format!("{name}{sep}")).filter(|r| !r.is_empty()) {
                return title[title.len() - rest.len()..].to_owned();
            }
            if let Some(rest) = lower.strip_suffix(&format!("{sep}{name}")).filter(|r| !r.is_empty()) {
                return title[..rest.len()].to_owned();
            }
        }
        if let Some(rest) = lower.strip_prefix(&format!("{name}: ")).filter(|r| !r.is_empty()) {
            return title[title.len() - rest.len()..].to_owned();
        }
    }
    title.to_owned()
}

/// Dock items that are the shell's own rather than apps. `[dock] items` names them; add a kind here and in
/// `State::dock_builtin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    Launchpad,
    AppDrawer,
    /// The screenshot toolbar. Spectacle's entry runs it too: Spectacle refuses to start outside KWin.
    Screenshot,
}

pub const SCREENSHOT_KEY: &str = "screenshot";
/// Screenshot apps that can't work here, by desktop-file id.
const SCREENSHOT_APPS: [&str; 2] = ["org.kde.spectacle", "spectacle"];

impl Builtin {
    pub fn of(key: &str) -> Option<Self> {
        match key {
            launchpad::DOCK_KEY => Some(Self::Launchpad),
            drawer::DOCK_KEY => Some(Self::AppDrawer),
            SCREENSHOT_KEY => Some(Self::Screenshot),
            _ if SCREENSHOT_APPS.contains(&key) => Some(Self::Screenshot),
            _ => None,
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Launchpad => launchpad::ICON,
            Self::AppDrawer => drawer::ICON,
            Self::Screenshot => "spectacle",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Launchpad => "Launchpad",
            Self::AppDrawer => "Applications",
            Self::Screenshot => "Screenshot",
        }
    }
}

/// One dock icon.
#[derive(Debug, Clone)]
pub struct Item {
    /// The desktop-file id, a builtin's name, or the window's app id when no entry matches.
    pub key: String,
    pub app: Option<App>,
    /// Shown and minimised, topmost shown first.
    pub windows: Vec<Window>,
}

impl Item {
    pub fn icon(&self) -> &str {
        match (&self.app, Builtin::of(&self.key)) {
            (Some(app), _) => &app.icon,
            (None, Some(b)) => b.icon(),
            (None, None) => &self.key,
        }
    }

    /// The tooltip.
    pub fn title(&self) -> &str {
        match (&self.app, Builtin::of(&self.key)) {
            (Some(app), _) => &app.name,
            (None, Some(b)) => b.title(),
            (None, None) => &self.key,
        }
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
/// Plank's LONG_PRESS_TIME: a press held this long without moving clicks nothing.
const LONG_PRESS: Duration = Duration::from_millis(750);

/// What a window resolved to under one `Apps` load (`State::apps_epoch`): its `app_key`, the item key and the app.
#[derive(Default)]
struct Resolved {
    epoch: u64,
    raw: String,
    key: String,
    app: Option<App>,
}

impl State {
    /// The app a window belongs to: its app id, else (noobsbot and others set none) its program's name.
    pub fn app_key(&self, window: &Window) -> Option<String> {
        self.app_id(window)
    }

    /// The dock item a window belongs to: its `app_key` resolved by `Apps::find` (the app's id, else the key as it
    /// is), kept on the window for one `Apps` load, since every pointer motion over the dock asks.
    pub fn dock_key(&self, window: &Window) -> Option<(String, Option<App>)> {
        let raw = self.app_key(window)?;
        let cache: &RefCell<Resolved> = window.user_data().get_or_insert(Default::default);
        let mut r = cache.borrow_mut();
        if (r.epoch, &r.raw) != (self.apps_epoch, &raw) || r.raw.is_empty() {
            let app = self.apps.find(&raw).cloned();
            let key = app.as_ref().map_or_else(|| raw.clone(), |a| a.id.clone());
            *r = Resolved { epoch: self.apps_epoch, raw, key, app };
        }
        Some((r.key.clone(), r.app.clone()))
    }

    /// Whether `window` is on the current Space (shown, or minimised from it).
    fn on_current_space(&self, window: &Window) -> bool {
        let top = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map_or(0, |g| g.loc.y);
        let loc = self.space.element_location(window).or_else(|| self.hidden.iter().find(|(w, _)| w == window).map(|(_, l)| *l));
        loc.is_none_or(|l| crate::spaces::row_at(l.y, top) == 0)
    }

    /// Pinned items that are installed, in config order, then running apps in the order they started.
    pub fn dock_items(&self) -> Vec<Item> {
        let dock = &self.config.dock;
        let mut items: Vec<Item> = Vec::new();
        for name in &dock.items {
            if Builtin::of(name).is_some() && !items.iter().any(|i| i.key == *name) {
                items.push(Item { key: name.clone(), app: self.apps.find(name).cloned(), windows: Vec::new() });
            } else if let Some(app) = self.apps.find(name).filter(|a| !items.iter().any(|i| i.key == a.id)) {
                items.push(Item { key: app.id.clone(), app: Some(app.clone()), windows: Vec::new() });
            }
        }
        let mut windows: Vec<Window> = self.space.elements().rev().chain(self.hidden.iter().map(|(w, _)| w)).cloned().collect();
        windows.retain(|w| !self.is_quake(w) && (!dock.current_workspace_only || self.on_current_space(w)));
        windows.sort_by_key(seen);
        for window in windows {
            let Some((key, app)) = self.dock_key(&window) else { continue };
            match items.iter_mut().find(|i| i.key == key) {
                Some(item) => item.windows.push(window),
                None if !dock.pinned_only => items.push(Item { key, app, windows: vec![window] }),
                None => {}
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
        let now = Instant::now();
        // The zoom eases in and out around the last position over the dock, and goes as the dock hides (Plank).
        let eased = if self.dock_pointer.is_some() { 1.0 } else { self.dock_fx.zoom.value(now) * (1.0 - self.dock_fx.hide.value(now)) };
        let zoom = if dock.zoom { f64::from(dock.zoom_percent) / 100.0 } else { 1.0 };
        let pointer = self.dock_pointer.or(self.dock_fx.zoom_at);
        Some(layout(n, size, i32::from(dock.icon_size), zoom, eased, pointer))
    }

    /// Shown (or showing): the hide modes' state as of the last update.
    pub fn dock_shown(&self) -> bool {
        !self.dock_fx.hider.hidden
    }

    /// A cheap first check for every pointer motion: is `location` low enough to be on the dock, zoomed, at all?
    fn near_dock(&self, location: Point<f64, Logical>) -> bool {
        let height = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map_or(0, |g| g.size.h);
        let dock = &self.config.dock;
        let zoom = if dock.zoom { f64::from(dock.zoom_percent) / 100.0 } else { 1.0 };
        let icon = f64::from(dock.icon_size);
        location.y >= f64::from(height) - (icon * zoom + bottom_pad(icon))
    }

    /// Whether `p` is on the shown dock: its resting area, or (while the pointer is on it) a zoomed icon.
    fn dock_on(&self, layout: &Layout, p: Point<f64, Logical>) -> bool {
        self.dock_shown() && (layout.zone.to_f64().contains(p) || (self.dock_reveal && hit(layout, p).is_some()))
    }

    /// Whether the dock takes the pointer at `location` (output-relative).
    pub fn dock_covers(&self, location: Point<f64, Logical>) -> bool {
        if !self.near_dock(location) {
            return false;
        }
        let items = self.dock_items();
        self.dock_layout(items.len()).is_some_and(|l| !items.is_empty() && self.dock_on(&l, location))
    }

    /// A press at `location`; `true` if the dock took it. With a popup open the shell closes it first.
    pub fn dock_press(&mut self, location: Point<f64, Logical>, button: u32) -> bool {
        if self.popup.is_some() || self.alert.is_some() || !self.near_dock(location) {
            return false;
        }
        let items = self.dock_items();
        let Some(layout) = self.dock_layout(items.len()).filter(|l| !items.is_empty() && self.dock_on(l, location)) else { return false };
        let ctrl = self.modifiers().ctrl;
        let Some((i, item)) = hit(&layout, location).and_then(|i| Some((i, items.into_iter().nth(i)?))) else {
            // Plank: a right click on the dock but not an icon opens its preferences.
            if button == BTN_RIGHT {
                self.dock_settings();
            }
            return true;
        };
        if item.key != launchpad::DOCK_KEY {
            self.close_launchpad();
        }
        match button {
            // A click acts on release, unless it became a drag or a long press (`dock_release`).
            BTN_LEFT => self.dock_ui.drag = Some(Drag { key: item.key, start: location, since: Instant::now(), moved: false, slot: None }),
            BTN_MIDDLE => self.dock_click(item, true),
            BTN_RIGHT if ctrl => self.dock_settings(),
            BTN_RIGHT => self.dock_open_menu(item.key, &layout, i),
            _ => {}
        }
        true
    }

    /// A click on `item`: a builtin's action, else (Plank) a new instance with Ctrl, the middle button or no
    /// windows, else its windows come forward (`dock_activate`).
    fn dock_click(&mut self, item: Item, new: bool) {
        if let Some(builtin) = Builtin::of(&item.key) {
            self.dock_fx.clicked(item.key.clone());
            return self.dock_builtin(builtin, &item.key);
        }
        if new || item.windows.is_empty() {
            self.launch(item.app.as_ref());
            return;
        }
        self.dock_fx.clicked(item.key.clone());
        self.dock_activate(item);
    }

    fn dock_builtin(&mut self, builtin: Builtin, key: &str) {
        match builtin {
            Builtin::Launchpad => self.toggle_launchpad(),
            Builtin::AppDrawer => {
                // The drawer opens above its icon.
                let items = self.dock_items();
                let at = items.iter().position(|i| i.key == key).zip(self.dock_layout(items.len())).map(|(i, l)| {
                    let (left, right) = l.columns[i];
                    (((left + right) / 2.0).round() as i32, l.icons[i].loc.y.round() as i32).into()
                });
                self.toggle_drawer(at.map_or(drawer::Anchor::Corner, drawer::Anchor::Dock));
            }
            Builtin::Screenshot => self.screenshot(crate::shell::screenshot::Start::Toolbar),
        }
    }

    /// Plank's smart focus: an urgent window first; else minimised windows come back; else the focused app
    /// minimises; else all its windows come forward and the topmost gets focus.
    fn dock_activate(&mut self, item: Item) {
        let restore = |state: &mut Self, windows: &[Window]| {
            let back: Vec<(Window, Point<i32, Logical>)> = state.hidden.iter().filter(|(w, _)| windows.contains(w)).cloned().collect();
            state.hidden.retain(|(w, _)| !windows.contains(w));
            for (window, loc) in &back {
                state.set_visible_loc(window, *loc);
            }
            back.into_iter().map(|(w, _)| w).last()
        };
        if let Some(window) = self.dock_fx.urgent_of(&item.windows).into_iter().next() {
            restore(self, std::slice::from_ref(&window));
            self.focus_window(Some(window), SERIAL_COUNTER.next_serial());
            return self.schedule_redraw();
        }
        if let Some(last) = restore(self, &item.windows) {
            self.focus_window(Some(last), SERIAL_COUNTER.next_serial());
            return self.schedule_redraw();
        }
        let shown = item.windows;
        if self.focused_window().is_some_and(|f| shown.contains(&f)) {
            return self.minimise(shown);
        }
        // Bottom first, so their order is kept and the topmost ends on top with focus.
        for window in shown.iter().rev().skip(1) {
            self.space.raise_element(window, false);
        }
        self.focus_window(shown.first().cloned(), SERIAL_COUNTER.next_serial());
        self.schedule_redraw();
    }

    /// Starts `app` from its `Exec` line, with Plank's launch bounce. `false`: it didn't start (no bounce either).
    // ponytail: Terminal=true apps start without a terminal.
    pub fn launch(&mut self, app: Option<&App>) -> bool {
        let Some(app) = app else { return false };
        tracing::info!(app = app.id, "dock: launch");
        self.dock_fx.tip_hidden = true;
        self.schedule_redraw();
        let started = self.spawn(&app.command()).is_some();
        if started {
            self.dock_fx.launched(app.id.clone());
        }
        started
    }

    /// A wheel step over the dock (`dir` > 0: down or right, < 0: up or left, 0: touchpad scrolling, swallowed):
    /// cycles the hovered app's windows, at most one per ClickTime; with Ctrl, resizes the icons by 2 px.
    /// `true`: over the dock, so apps don't get it.
    pub fn dock_scroll(&mut self, location: Point<f64, Logical>, dir: f64) -> bool {
        if !self.dock_covers(location) {
            return false;
        }
        if dir == 0.0 || self.dock_ui.dragging() {
            return true;
        }
        if self.modifiers().ctrl {
            let size = (i32::from(self.config.dock.icon_size) + if dir < 0.0 { 2 } else { -2 }).clamp(MIN_ICON, MAX_ICON);
            self.dock_save(|text| config::set_key(text, "dock", "icon_size", &size.to_string()));
            self.config.dock.icon_size = size as u16;
            self.schedule_redraw();
            return true;
        }
        let items = self.dock_items();
        let Some(item) = self.dock_layout(items.len()).and_then(|l| hit(&l, location)).and_then(|i| items.into_iter().nth(i)) else { return true };
        if item.windows.is_empty() {
            return true;
        }
        self.dock_fx.clicked(item.key.clone());
        self.schedule_redraw();
        let now = Instant::now();
        if self.dock_fx.scrolled.get(&item.key).is_some_and(|at| now - *at < CLICK_TIME) {
            return true;
        }
        self.dock_fx.scrolled.insert(item.key.clone(), now);
        let mut windows = item.windows;
        windows.sort_by_key(seen);
        let focused = self.focused_window();
        let next = windows[cycle(windows.len(), windows.iter().position(|w| Some(w) == focused.as_ref()), dir > 0.0)].clone();
        if let Some(at) = self.hidden.iter().position(|(w, _)| *w == next) {
            let (_, loc) = self.hidden.remove(at);
            self.set_visible_loc(&next, loc);
        }
        self.focus_window(Some(next), SERIAL_COUNTER.next_serial());
        true
    }

    /// The pointer moved. Plank's cursor region: the dock's resting area, shrinking to a 1 px strip at the screen
    /// edge as it hides, plus the hovered icon while zoomed. Being in it is "hovered": it shows the dock, zooms it
    /// and lightens the icon under the pointer.
    pub fn dock_motion(&mut self, from: Point<f64, Logical>, to: Point<f64, Logical>) {
        self.dock_drag_motion(to);
        if !self.dock_reveal && !self.near_dock(from) && !self.near_dock(to) {
            return;
        }
        let items = self.dock_items();
        let (Some(layout), Some(size)) =
            (self.dock_layout(items.len()), self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map(|g| g.size))
        else {
            return;
        };
        let now = Instant::now();
        let zone = layout.zone.to_f64();
        let strip = (zone.size.h * (1.0 - self.dock_fx.hide.value(now))).max(1.0);
        let h = f64::from(size.h);
        let in_region = !items.is_empty() && to.x >= zone.loc.x && to.x < zone.loc.x + zone.size.w && to.y >= h - strip && to.y < h;
        let over = hit(&layout, to).filter(|_| self.dock_shown() && (in_region || self.dock_reveal));
        let hovered = in_region || over.is_some();
        let before = self.dock_fx.hovered_key().map(str::to_owned);
        self.dock_fx.hovered(over.map(|i| items[i].key.as_str()));
        self.dock_fx.zoom.set(hovered, now);
        if hovered {
            self.dock_fx.zoom_at = Some(to);
        }
        let changed = hovered != self.dock_reveal;
        self.dock_reveal = hovered;
        if changed {
            self.dock_update_hide(&layout);
        }
        if changed || (hovered && self.config.dock.zoom) || before.as_deref() != self.dock_fx.hovered_key() {
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
    since: Instant,
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

    /// An icon is being dragged (past the threshold).
    pub fn dragging(&self) -> bool {
        self.dragged().is_some()
    }

    /// The dragged icon's item key: not drawn in the dock, but under the pointer.
    pub fn dragged(&self) -> Option<&str> {
        self.drag.as_ref().filter(|d| d.moved).map(|d| d.key.as_str())
    }
}

/// A row of a dock icon's menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Brings the app's n-th window forward.
    Window(usize),
    /// Keep in Dock, ticked while pinned.
    Pin,
    /// Close / Close All.
    Close,
    /// The app's n-th desktop action.
    Action(usize),
}

/// Plank's menu: Keep in Dock (`pinned`: `None` when it can't be pinned) and Close (Close All with several windows),
/// then the app's desktop actions, then its windows when there are several, the `active` one greyed.
pub fn menu_rows(titles: &[String], active: Option<usize>, pinned: Option<bool>, actions: &[String]) -> Vec<Option<Row>> {
    let row = |label: &str, shortcut, command: Option<Command>| Some(Row { label: label.to_owned(), shortcut, action: command.map(Action::Dock) });
    let mut first = Vec::new();
    if let Some(pinned) = pinned {
        first.push(row("Keep in Dock", if pinned { "✓" } else { "" }, Some(Command::Pin)));
    }
    if !titles.is_empty() {
        first.push(row(if titles.len() > 1 { "Close All" } else { "Close" }, "", Some(Command::Close)));
    }
    let actions: Vec<Option<Row>> = actions.iter().enumerate().map(|(i, a)| row(a, "", Some(Command::Action(i)))).collect();
    let windows: Vec<Option<Row>> = if titles.len() > 1 {
        titles.iter().enumerate().map(|(i, t)| row(t, "", (active != Some(i)).then_some(Command::Window(i)))).collect()
    } else {
        Vec::new()
    };
    let mut rows = Vec::new();
    for group in [first, actions, windows].into_iter().filter(|g| !g.is_empty()) {
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
/// the i-th pinned icon on the dock, `drop` the indices of every entry naming the moved icon (`kate` and
/// `org.kde.kate` both). Entries for apps that aren't installed keep their place.
pub fn repin(items: &[String], shown: &[usize], name: &str, drop: &[usize], to: Option<usize>) -> Vec<String> {
    let mut list: Vec<(String, bool)> = items.iter().enumerate().filter(|(i, _)| !drop.contains(i)).map(|(i, s)| (s.clone(), shown.contains(&i))).collect();
    if let Some(to) = to {
        let at = list.iter().enumerate().filter(|(_, (_, shown))| *shown).nth(to).map_or(list.len(), |(i, _)| i);
        list.insert(at, (name.to_owned(), true));
    }
    list.into_iter().map(|(s, _)| s).collect()
}

/// What releasing a dragged icon at `slot` does: `Some(Some(slot))` pins it there, `Some(None)` unpins it (dropped off
/// the dock), `None` nothing. A builtin is never unpinned that way: nothing would bring it back.
pub fn dropped(slot: Option<usize>, pinned: bool, pinnable: bool, builtin: bool) -> Option<Option<usize>> {
    match slot {
        Some(to) if pinnable => Some(Some(to)),
        None if pinned && !builtin => Some(None),
        _ => None,
    }
}

/// `text` (a config.toml) with `[dock] items` set to `items`; every other line, comments included, is kept.
pub fn set_items(text: &str, items: &[String]) -> String {
    config::set_array(text, "dock", "items", items)
}

/// The URIs of a `text/uri-list` (comments and blank lines skipped).
pub fn uri_list(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from).collect()
}

impl State {
    /// The item key a `[dock] items` entry pins: a builtin, or the app it names (`None`: not installed).
    fn pin_key(&self, name: &str) -> Option<String> {
        if Builtin::of(name).is_some() { Some(name.to_owned()) } else { self.apps.find(name).map(|a| a.id.clone()) }
    }

    /// The pinned icons on the dock: (index in `[dock] items`, item key), left to right.
    pub(crate) fn dock_pinned(&self) -> Vec<(usize, String)> {
        let mut pinned: Vec<(usize, String)> = Vec::new();
        for (i, name) in self.config.dock.items.iter().enumerate() {
            if let Some(key) = self.pin_key(name).filter(|k| !pinned.iter().any(|(_, p)| p == k)) {
                pinned.push((i, key));
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
        let name = item.title().to_owned();
        let titles: Vec<String> = item
            .windows
            .iter()
            .map(|w| {
                let title = w
                    .toplevel()
                    .and_then(|t| with_states(t.wl_surface(), |s| s.data_map.get::<XdgToplevelSurfaceData>().and_then(|d| d.lock().ok()?.title.clone())));
                title.filter(|t| !t.is_empty()).map_or_else(|| name.clone(), |t| short_title(&t, &name))
            })
            .collect();
        let focused = self.focused_window();
        let active = item.windows.iter().position(|w| Some(w) == focused.as_ref());
        let pinnable = !self.config.dock.lock_items && (item.app.is_some() || Builtin::of(&item.key).is_some());
        let pinned = pinnable.then(|| self.dock_pinned().iter().any(|(_, id)| *id == item.key));
        let actions: Vec<String> = item.app.as_ref().map(|a| crate::apps::actions(&a.id).into_iter().map(|(n, _)| n).collect()).unwrap_or_default();
        menu_rows(&titles, active, pinned, &actions)
    }

    /// Right click: icon `i`'s menu, centred over it.
    fn dock_open_menu(&mut self, key: String, layout: &Layout, i: usize) {
        let (left, right) = layout.columns[i];
        let at = Point::from((((left + right) / 2.0).round() as i32, layout.icons[i].loc.y.round() as i32));
        self.dock_ui.menu = Some((key, at));
        self.dock_fx.tip_hidden = true;
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
            Command::Close => item.windows.iter().filter_map(|w| w.toplevel()).for_each(|t| t.send_close()),
            Command::Action(i) => {
                let Some(app) = item.app else { return };
                let Some((_, exec)) = crate::apps::actions(&app.id).into_iter().nth(i) else { return };
                self.spawn(&App { exec, ..app }.command());
            }
        }
        self.schedule_redraw();
    }

    /// Rewrites config.toml with `edit`, in the file the user edits (through a dotfiles symlink), so the live
    /// reload agrees.
    fn dock_save(&mut self, edit: impl FnOnce(&str) -> String) {
        let path = config::path();
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        let text = std::fs::read_to_string(&path).unwrap_or_else(|_| config::DEFAULT_TOML.to_owned());
        // A file that doesn't parse left `self.config` at the defaults: an edit from those would write over the user's pins.
        if let Err(err) = config::parse(&text) {
            return tracing::warn!("{}: {err}; the dock isn't saved over a config that doesn't parse", path.display());
        }
        if let Err(err) = config::save_atomic(&path, &edit(&text)) {
            tracing::warn!("{}: {err}", path.display());
        }
    }

    /// Moves `key` to pinned slot `to`, or unpins it, and saves `[dock] items`.
    pub fn dock_repin(&mut self, key: &str, to: Option<usize>) {
        let shown: Vec<usize> = self.dock_pinned().iter().map(|(i, _)| *i).collect();
        let drop: Vec<usize> = self.config.dock.items.iter().enumerate().filter(|(_, n)| self.pin_key(n).as_deref() == Some(key)).map(|(i, _)| i).collect();
        let items = repin(&self.config.dock.items, &shown, key, &drop, to);
        if items == self.config.dock.items {
            return;
        }
        self.dock_save(|text| set_items(text, &items));
        self.config.dock.items = items;
        self.schedule_redraw();
    }

    /// The pointer moved to `to` with an icon held: past the threshold it drags (unless items are locked), the icon
    /// follows the pointer and the others make room.
    fn dock_drag_motion(&mut self, to: Point<f64, Logical>) {
        let Some(drag) = self.dock_ui.drag.as_ref() else { return };
        let (start, key) = (drag.start, drag.key.clone());
        if self.config.dock.lock_items || (!drag.moved && (to.x - start.x).hypot(to.y - start.y) < DRAG_START) {
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
            (drag.moved, drag.slot) = (true, slot);
        }
        self.dock_fx.tip_hidden = true;
        self.schedule_redraw();
    }

    /// The left button came up: a click on the held icon (not after a long press, and only on the same icon), or
    /// the end of its drag: reordered (which pins it, Plank's auto-pinning), or off the dock and pinned: removed in
    /// a poof.
    pub fn dock_release(&mut self, location: Point<f64, Logical>) {
        let Some(drag) = self.dock_ui.drag.take() else { return };
        let Some(item) = self.dock_item(&drag.key) else { return };
        if !drag.moved {
            let items = self.dock_items();
            let same = self.dock_layout(items.len()).and_then(|l| hit(&l, location)).is_some_and(|i| items[i].key == item.key);
            if same && drag.since.elapsed() < LONG_PRESS {
                let ctrl = self.modifiers().ctrl;
                self.dock_click(item, ctrl);
            }
            return self.schedule_redraw();
        }
        let pinned = self.dock_pinned().iter().any(|(_, id)| *id == item.key);
        let builtin = Builtin::of(&item.key).is_some();
        if let Some(to) = dropped(drag.slot, pinned, item.app.is_some() || builtin, builtin) {
            self.dock_repin(&item.key, to);
            if to.is_none() {
                self.dock_fx.poof(location);
            }
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

    /// An app's drag ended over no window: on the dock, `.desktop` files are pinned where they land (not while items
    /// are locked), other files open with the app they land on.
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
            if self.config.dock.lock_items {
                return;
            }
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
    fn unzoomed_dock_has_planks_numbers() {
        let l = layout(8, SCREEN.into(), 84, 1.5, 0.0, None);
        // Transparent-Tight at 84 px: ItemPadding 4 px, BottomPadding 16 px; 8 × 88 = 704 px, centred.
        assert_eq!(l.zone, Rectangle::new((608, 1100).into(), (704, 100).into()));
        assert_eq!(l.icons[0], Rectangle::new((610.0, 1100.0).into(), (84.0, 84.0).into()));
        assert_eq!(l.icons[1].loc.x - l.icons[0].loc.x, 88.0);
        assert_eq!(l.columns[0], (608.0, 696.0));
        // The bar covers the icons' lower half and reaches the screen bottom, end to end.
        assert_eq!((l.bar.loc.y, l.bar.loc.y + l.bar.size.h), (1142, 1200));
        assert_eq!((l.bar.loc.x, l.bar.size.w), (608, 704));
    }

    #[test]
    fn zoom_is_planks() {
        let flat = layout(8, SCREEN.into(), 84, 1.5, 0.0, None);
        let centre: Point<f64, Logical> = (916.0, 1142.0).into();
        assert_eq!(flat.icons[3].loc.x + 42.0, centre.x);
        let l = layout(8, SCREEN.into(), 84, 1.5, 1.0, Some(centre));
        assert_eq!(l.icons[3], Rectangle::new((853.0, 1058.0).into(), (126.0, 126.0).into()), "150% under the pointer, same baseline");
        // A neighbour 88 px away: zoom 1 + (1 − (88/126)²) × 0.5, moved out by 88 × 0.5 × (1 − 88/378).
        let f: f64 = 88.0 / 126.0;
        assert_eq!(l.icons[4].size.w, (84.0 * (1.0 + (1.0 - f * f) * 0.5)).round());
        let shift = 88.0 * 0.5 * (1.0 - f / 3.0);
        assert!((l.icons[4].loc.x + l.icons[4].size.w / 2.0 - (1004.0 + shift)).abs() < 1e-9);
        // Out of reach (126 px): unzoomed, pushed aside by 126 × 0.5 × 2/3 = 42 px.
        assert_eq!(l.icons[7].size.w, 84.0);
        assert_eq!(l.icons[7].loc.x, flat.icons[7].loc.x + 42.0);
        assert_eq!(l.icons[0].loc.x, flat.icons[0].loc.x - 42.0);
        assert_eq!((l.bar.loc.x, l.bar.size.w), (608 - 42, 704 + 84), "the background follows the end icons");
        assert_eq!(hit(&l, centre), Some(3));
        assert_eq!(hit(&l, (916.0, 1060.0).into()), Some(3), "the zoomed top of the icon counts");
        // Half-way through the easing: 125%.
        assert_eq!(layout(8, SCREEN.into(), 84, 1.5, 0.5, Some(centre)).icons[3].size.w, 105.0);
        // No easing or zoom off: flat, wherever the pointer is.
        assert_eq!(layout(8, SCREEN.into(), 84, 1.5, 0.0, Some(centre)), flat);
        assert_eq!(layout(8, SCREEN.into(), 84, 1.0, 1.0, Some(centre)), flat);
    }

    #[test]
    fn hits_follow_columns_inside_the_zone() {
        let l = layout(3, SCREEN.into(), 84, 1.0, 0.0, None);
        let x = |i: usize| (l.columns[i].0 + l.columns[i].1) / 2.0;
        assert_eq!(hit(&l, (x(0), 1150.0).into()), Some(0));
        assert_eq!(hit(&l, (x(2), 1199.0).into()), Some(2));
        assert_eq!(hit(&l, (x(1), 1000.0).into()), None, "above the dock");
        assert_eq!(hit(&l, (10.0, 1150.0).into()), None, "beside it");
        // The pointer over an icon's centre hits it, zoomed.
        for i in 0..3 {
            let p = (x(i), 1150.0).into();
            let z = layout(3, SCREEN.into(), 84, 1.5, 1.0, Some(p));
            assert_eq!(hit(&z, p), Some(i));
            assert!(z.icons.iter().all(|r| r.size.w <= z.icons[i].size.w));
        }
    }

    #[test]
    fn scrolling_cycles_round() {
        assert_eq!(cycle(3, Some(0), true), 1);
        assert_eq!(cycle(3, Some(2), true), 0);
        assert_eq!(cycle(3, Some(0), false), 2);
        assert_eq!(cycle(3, None, false), 0, "none active: the first");
        assert_eq!(cycle(1, Some(0), true), 0);
    }

    #[test]
    fn window_names_are_shortened_like_plank() {
        assert_eq!(short_title("notes.txt - Kate", "Kate"), "notes.txt");
        assert_eq!(short_title("Dolphin – Home", "Dolphin"), "Home");
        assert_eq!(short_title("New Tab - Google Chrome", "Google Chrome"), "New Tab");
        assert_eq!(short_title("Inbox - Chrome", "Google Chrome"), "Inbox", "a word of the name");
        assert_eq!(short_title("VS Code: main.rs", "VS Code"), "main.rs");
        // Lowercasing "İ" is longer by a byte: the offsets would split a char.
        assert_eq!(short_title("İstanbul - Kate", "Kate"), "İstanbul - Kate");
        assert_eq!(short_title("Kate", "Kate"), "Kate");
        assert_eq!(short_title("~/src", "noobsbot"), "~/src");
    }

    #[test]
    fn menu_is_planks() {
        let labels = |rows: Vec<Option<Row>>| rows.into_iter().map(|r| r.map(|r| (r.label, r.shortcut, r.action.is_some()))).collect::<Vec<_>>();
        let s = |l: &str, tick: &'static str, on: bool| Some((l.to_owned(), tick, on));
        let two = ["a.txt".to_owned(), "b.txt".to_owned()];
        let actions = ["New Window".to_owned()];
        assert_eq!(
            labels(menu_rows(&two, Some(1), Some(true), &actions)),
            [s("Keep in Dock", "✓", true), s("Close All", "", true), None, s("New Window", "", true), None, s("a.txt", "", true), s("b.txt", "", false)]
        );
        // One window: Close, and no window list.
        assert_eq!(labels(menu_rows(&two[..1], Some(0), Some(false), &[])), [s("Keep in Dock", "", true), s("Close", "", true)]);
        // Not running; locked items (or no desktop entry): nothing to pin.
        assert_eq!(labels(menu_rows(&[], None, Some(true), &[])), [s("Keep in Dock", "✓", true)]);
        assert_eq!(labels(menu_rows(&two[..1], None, None, &[])), [s("Close", "", true)]);
        let rows = menu_rows(&two, None, None, &actions);
        assert_eq!(rows[2].as_ref().and_then(|r| r.action.clone()), Some(Action::Dock(Command::Action(0))));
        assert_eq!(rows[5].as_ref().and_then(|r| r.action.clone()), Some(Action::Dock(Command::Window(1))));
    }

    #[test]
    fn builtins_and_the_screenshot_app() {
        assert_eq!(Builtin::of("launchpad"), Some(Builtin::Launchpad));
        assert_eq!(Builtin::of("screenshot"), Some(Builtin::Screenshot));
        assert_eq!(Builtin::of("app-drawer"), Some(Builtin::AppDrawer));
        assert_eq!(Builtin::of("org.kde.spectacle"), Some(Builtin::Screenshot), "Spectacle needs KWin: it opens our toolbar");
        assert_eq!(Builtin::of("org.kde.kate"), None);
        let item = |key: &str| Item { key: key.into(), app: None, windows: Vec::new() };
        assert_eq!((item("screenshot").icon(), item("screenshot").title()), ("spectacle", "Screenshot"));
        assert_eq!(item("launchpad").icon(), launchpad::ICON);
        assert_eq!(item("some-app").title(), "some-app");
    }

    #[test]
    fn drag_slots_follow_columns() {
        let l = layout(4, SCREEN.into(), 84, 1.0, 0.0, None);
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
        assert_eq!(repin(&items, &shown, "kitty", &[4], Some(0)), v(&["chrome", "kitty", "kate", "vscode", "dolphin"]));
        assert_eq!(repin(&items, &shown, "kate", &[1], Some(2)), v(&["chrome", "vscode", "dolphin", "kitty", "kate"]));
        assert_eq!(repin(&items, &shown, "dolphin", &[3], None), v(&["chrome", "kate", "vscode", "kitty"]));
        // A running app pinned between kate and dolphin.
        assert_eq!(repin(&items, &shown, "helium", &[], Some(1)), v(&["chrome", "kate", "vscode", "helium", "dolphin", "kitty"]));
        assert_eq!(repin(&items, &shown, "helium", &[], Some(3)), v(&["chrome", "kate", "vscode", "dolphin", "kitty", "helium"]));
        // "Add to Dock": the slot after every pinned icon, builtins counted (counting apps alone landed before them).
        let items = v(&["launchpad", "kate", "screenshot"]);
        assert_eq!(repin(&items, &[0, 1, 2], "helium", &[], Some(3)), v(&["launchpad", "kate", "screenshot", "helium"]));
        assert_eq!(repin(&items, &[0, 1, 2], "helium", &[], Some(1)), v(&["launchpad", "helium", "kate", "screenshot"]));
        // Two entries naming one app (only the first is shown): unpinning drops both, else the other took its place.
        let items = v(&["kate", "org.kde.kate", "dolphin"]);
        assert_eq!(repin(&items, &[0, 2], "org.kde.kate", &[0, 1], None), v(&["dolphin"]));
        assert_eq!(repin(&items, &[0, 2], "org.kde.kate", &[0, 1], Some(1)), v(&["dolphin", "org.kde.kate"]));
    }

    #[test]
    fn dropping_off_the_dock_unpins_apps_only() {
        assert_eq!(dropped(None, true, true, false), Some(None), "an app dragged off");
        assert_eq!(dropped(None, true, true, true), None, "a builtin stays");
        assert_eq!(dropped(None, false, true, false), None, "a running, unpinned app");
        assert_eq!(dropped(Some(2), false, true, true), Some(Some(2)), "builtins still move");
        assert_eq!(dropped(Some(0), false, false, false), None, "an uninstalled app can't be pinned");
    }

    #[test]
    fn preview_moves_the_dragged_icon() {
        let item = |k: &str| Item { key: k.into(), app: None, windows: Vec::new() };
        let drag = Drag { key: "a".into(), start: (0.0, 0.0).into(), since: Instant::now(), moved: true, slot: Some(2) };
        let mut ui = Ui { drag: Some(drag), ..Default::default() };
        let keys: Vec<String> = ui.preview(vec![item("a"), item("b"), item("c")]).into_iter().map(|i| i.key).collect();
        assert_eq!(keys, ["b", "c", "a"]);
        assert_eq!(ui.dragged(), Some("a"));
        ui.drag.as_mut().unwrap().slot = None;
        assert_eq!(ui.preview(vec![item("a"), item("b")]).len(), 2, "off the dock it stays in place until released");
        ui.drag.as_mut().unwrap().moved = false;
        assert!(!ui.dragging(), "a press is not a drag yet");
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
    fn program_names_are_not_cut_short() {
        // This test binary's own name is longer than comm's 15 chars.
        let exe = std::env::current_exe().unwrap().file_name().unwrap().to_str().unwrap().to_owned();
        assert_eq!(crate::state::program_name(std::process::id() as i32).as_deref(), Some(exe.as_str()));
        assert_eq!(crate::state::program_name(i32::MAX), None);
    }

    #[test]
    fn reserve_matches_the_zone() {
        assert_eq!(reserve(&Dock::default()), layout(8, SCREEN.into(), 84, 1.0, 0.0, None).zone.size.h);
    }
}
