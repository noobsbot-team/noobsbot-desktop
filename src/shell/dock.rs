//! The dock (E3.1): the owner's Plank dock rebuilt in the shell, behaving as Plank does. Pinned items from
//! `[dock] items`, then running apps; dots under running ones; click to launch, focus or minimise; scroll to cycle
//! windows; drag to reorder, pin or remove; Plank's zoom under the pointer and its hide modes. Layout is a pure
//! function (Plank's geometry) so drawing and clicks share it.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    io::Read,
    os::fd::AsFd,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use smithay::{
    desktop::Window,
    output::Output,
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_server::protocol::wl_data_source::WlDataSource,
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Size},
    wayland::{compositor::with_states, selection::data_device::with_source_metadata, shell::xdg::XdgToplevelSurfaceData},
};

use crate::{
    apps::App,
    config::{self, Dock, DockClick, DockDisplays, DockMiddle, DockScroll, quote},
    shell::{Row, dock_fx::CLICK_TIME, drawer, launchpad},
    shortcuts::Action,
    spaces::Assign,
    state::State,
    wm::Rect,
};

/// Icons never shrink below this to fit the screen (D13).
pub const MIN_ICON: i32 = 32;
/// Plank's largest icon size (Ctrl+scroll stops there).
pub const MAX_ICON: i32 = 128;

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
    /// The unzoomed dock (Plank's static dock region): where the pointer zooms and clicks.
    pub zone: Rect,
    /// The unzoomed icon size after fitting.
    pub icon: f64,
    /// The x of the line between the apps and the minimised-window tiles, if both are there.
    pub separator: Option<f64>,
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

/// Which of `n` displays (the main one first) have a dock under `mode`; `pointer`: the index of the one the pointer
/// is on.
pub fn dock_displays(mode: DockDisplays, n: usize, pointer: Option<usize>) -> Vec<usize> {
    match mode {
        DockDisplays::All => (0..n).collect(),
        DockDisplays::Main => (0..n.min(1)).collect(),
        DockDisplays::Pointer => pointer.filter(|i| *i < n).or((n > 0).then_some(0)).into_iter().collect(),
    }
}

/// The dock of `n` items on an `output`-sized screen, the last `tiles` of them minimised windows after a separator gap,
/// with Plank's zoom: icons within the zoomed icon size of the `pointer` grow up to `zoom` (1.5 = 150%) along a
/// parabola and the others move aside, all scaled by `progress` (0..1, the zoom easing in and out).
pub fn layout(n: usize, tiles: usize, output: Size<i32, Logical>, icon: i32, zoom: f64, progress: f64, pointer: Option<Point<f64, Logical>>) -> Layout {
    let s = fit(n, output.w, icon);
    let (pad, bottom) = (item_pad(s), bottom_pad(s));
    let step = s + pad;
    let split = n - tiles.min(n);
    let gap = if split > 0 && split < n { (step / 3.0).floor() } else { 0.0 };
    let (w, h) = (f64::from(output.w), f64::from(output.h));
    let left = ((w - step * n as f64 - gap) / 2.0).floor();
    let zone = Rectangle::new((left as i32, (h - s - bottom) as i32).into(), ((step * n as f64 + gap) as i32, (s + bottom) as i32).into());
    // Plank's ZoomIconSize: how far the zoom reaches, and the zoom at this point of the easing.
    let reach = (s * zoom).round();
    let now = 1.0 + (zoom - 1.0) * progress;
    let (mut icons, mut columns) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for i in 0..n {
        let rest = (left + step / 2.0).floor() + step * i as f64 + if i >= split { gap } else { 0.0 };
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
        // Whole pixels: an icon drawn at a fractional x is resampled blurry; its column keeps the exact centre.
        icons.push(Rectangle::new(((centre - size / 2.0).round(), h - bottom - size).into(), (size, size).into()));
        columns.push((centre - (size + pad) / 2.0, centre + (size + pad) / 2.0));
    }
    let separator = (gap > 0.0).then(|| (columns[split - 1].1 + columns[split].0) / 2.0);
    Layout { icons, columns, zone, icon: s, separator }
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

/// The window a cycle lands on: the next (or previous) of `groups[here]` (one app's windows, in the order they
/// opened); when the focused window is one of them and `across` is set, it walks on into the neighbouring groups (the
/// dock's other apps) instead of wrapping round.
pub fn cycle_target<T: PartialEq + Clone>(groups: &[Vec<T>], here: usize, focused: Option<&T>, forward: bool, across: bool) -> Option<T> {
    let own = groups.get(here)?;
    let at = own.iter().position(|w| Some(w) == focused);
    if across && at.is_some() {
        let flat: Vec<&T> = groups.iter().flatten().collect();
        let i = flat.iter().position(|w| Some(*w) == focused);
        return Some(flat[cycle(flat.len(), i, forward)].clone());
    }
    own.get(cycle(own.len(), at, forward)).cloned()
}

/// Hovering an icon with several windows this long shows their previews (E8.3).
pub const PEEK_DELAY: Duration = Duration::from_millis(500);
/// Thumbnail height, and the strip's padding and gaps.
const PEEK_H: i32 = 120;
const PEEK_PAD: i32 = 12;
/// Between the strip and the zoomed icon: the pointer crosses it without the strip going.
const PEEK_GAP: i32 = 8;

/// The window previews (E8.3), output-relative: the glass strip and each window's thumbnail.
#[derive(Debug, Clone, PartialEq)]
pub struct Peek {
    pub strip: Rect,
    pub thumbs: Vec<Rect>,
}

/// What a point on the previews is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeekHit {
    Close(usize),
    Thumb(usize),
    /// The strip between thumbnails, or the gap down to the icon.
    Strip,
}

/// The strip for windows of `sizes`, centred over `at` (the zoomed icon's top centre) on a `width`-wide screen: cells
/// PEEK_H tall, as wide as the window's shape (half to twice the height), all shrunk when they don't fit; each
/// window fitted and centred in its cell.
pub fn peek_layout(sizes: &[Size<i32, Logical>], at: Point<i32, Logical>, width: i32) -> Peek {
    let h = f64::from(PEEK_H);
    let cells: Vec<f64> = sizes.iter().map(|s| (f64::from(s.w) * h / f64::from(s.h.max(1))).clamp(h / 2.0, h * 2.0)).collect();
    let gaps = PEEK_PAD * (sizes.len() as i32 + 1);
    let k = (f64::from(width - 2 * PEEK_PAD - gaps) / cells.iter().sum::<f64>().max(1.0)).min(1.0);
    let cells: Vec<i32> = cells.iter().map(|c| (c * k).round() as i32).collect();
    let ch = (h * k).round() as i32;
    let w = cells.iter().sum::<i32>() + gaps;
    let x = (at.x - w / 2).clamp(PEEK_PAD, (width - w - PEEK_PAD).max(PEEK_PAD));
    let strip = Rectangle::new((x, at.y - PEEK_GAP - ch - 2 * PEEK_PAD).into(), (w, ch + 2 * PEEK_PAD).into());
    let mut left = x + PEEK_PAD;
    let thumbs = sizes
        .iter()
        .zip(cells)
        .map(|(s, cw)| {
            let f = (f64::from(cw) / f64::from(s.w.max(1))).min(f64::from(ch) / f64::from(s.h.max(1)));
            let (tw, th) = ((f64::from(s.w) * f).round() as i32, (f64::from(s.h) * f).round() as i32);
            let thumb = Rectangle::new((left + (cw - tw) / 2, strip.loc.y + PEEK_PAD + (ch - th) / 2).into(), (tw, th).into());
            left += cw + PEEK_PAD;
            thumb
        })
        .collect();
    Peek { strip, thumbs }
}

/// What `p` is over: the `hover`ed thumbnail's ✕ (Mission Control's `close_button`), a thumbnail, or the strip.
pub fn peek_hit(peek: &Peek, hover: Option<usize>, p: Point<f64, Logical>) -> Option<PeekHit> {
    if let Some(i) = hover.filter(|i| peek.thumbs.get(*i).is_some_and(|t| crate::mission::close_button(*t).to_f64().contains(p))) {
        return Some(PeekHit::Close(i));
    }
    if let Some(i) = peek.thumbs.iter().position(|t| t.to_f64().contains(p)) {
        return Some(PeekHit::Thumb(i));
    }
    let s = peek.strip;
    Rectangle::<i32, Logical>::new(s.loc, (s.size.w, s.size.h + PEEK_GAP).into()).to_f64().contains(p).then_some(PeekHit::Strip)
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
    /// The screenshot toolbar. Spectacle's entry runs it too: Spectacle refuses to start outside its own compositor.
    Screenshot,
    /// Jobs wait in the CUPS queue: shown among the running apps until it empties; a click opens Settings ▸ Printers.
    Printing,
}

/// The dock's old Trash item: `[dock] items` lists from before may still name it; it is skipped.
const TRASH_KEY: &str = "trash";
pub const PRINTING_KEY: &str = "printing";
pub const SCREENSHOT_KEY: &str = "screenshot";
/// Screenshot apps that can't work here, by desktop-file id.
const SCREENSHOT_APPS: [&str; 2] = ["org.kde.spectacle", "spectacle"];

impl Builtin {
    pub fn of(key: &str) -> Option<Self> {
        match key {
            launchpad::DOCK_KEY => Some(Self::Launchpad),
            drawer::DOCK_KEY => Some(Self::AppDrawer),
            SCREENSHOT_KEY => Some(Self::Screenshot),
            PRINTING_KEY => Some(Self::Printing),
            _ if SCREENSHOT_APPS.contains(&key) => Some(Self::Screenshot),
            _ => None,
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Launchpad => launchpad::ICON,
            Self::AppDrawer => drawer::ICON,
            Self::Screenshot => "spectacle",
            Self::Printing => "printer",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Launchpad => "Launchpad",
            Self::AppDrawer => "Applications",
            Self::Screenshot => "Screenshot",
            Self::Printing => "Printing",
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

/// Jobs in the CUPS queue, as of the last look.
static PRINTING: AtomicBool = AtomicBool::new(false);

/// Looks at the CUPS queue every 5 s, calling `changed` whenever it fills or empties; stops when `changed` returns
/// false. Nothing without CUPS.
// ponytail: polls `lpstat -o` instead of cupsd's D-Bus job signals, and only while cupsd runs, so the look doesn't
// socket-activate it and keep it from idling out; listen for `com.redhat.PrinterSpooler` if 5 s feels late.
pub fn watch_print_queue(mut changed: impl FnMut() -> bool + Send + 'static) {
    if !crate::ibus::on_path("lpstat") {
        return;
    }
    let cupsd = || std::fs::read_dir("/proc").into_iter().flatten().flatten().any(|e| std::fs::read(e.path().join("comm")).is_ok_and(|c| c == b"cupsd\n"));
    let jobs = || std::process::Command::new("lpstat").arg("-o").output().is_ok_and(|o| o.stdout.iter().any(|b| !b.is_ascii_whitespace()));
    let spawned = std::thread::Builder::new().name("print-queue".into()).spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(5));
            let busy = cupsd() && jobs();
            if PRINTING.swap(busy, Ordering::Relaxed) != busy && !changed() {
                return;
            }
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("print queue: {err}");
    }
}

/// On a window minimised (not hidden with its app): its app's icon stays in the dock while it is.
struct Minimised(std::cell::Cell<bool>);

pub fn set_minimised(window: &Window, on: bool) {
    window.user_data().get_or_insert(|| Minimised(false.into())).0.set(on);
}

pub fn is_minimised(window: &Window) -> bool {
    window.user_data().get::<Minimised>().is_some_and(|m| m.0.get())
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

/// How long a dragged file rests on an icon before its app springs forward.
const SPRING_DELAY: Duration = Duration::from_millis(600);

/// A file dragged (DnD) over the dock: the icon it rests on and since when; `sprung` once that app came forward.
#[derive(Debug, Default)]
pub struct Spring {
    over: Option<(String, Instant)>,
    sprung: bool,
}

impl Spring {
    /// The drag is over icon `key` (`None`: off the icons) at `now`; true when it arrived on another icon.
    fn over(&mut self, key: Option<&str>, now: Instant) -> bool {
        if self.over.as_ref().map(|(k, _)| k.as_str()) == key {
            return false;
        }
        self.over = key.map(|k| (k.to_owned(), now));
        self.sprung = false;
        key.is_some()
    }

    /// The icon whose app springs forward at `now`: rested on for `SPRING_DELAY`, once per visit.
    fn due(&mut self, now: Instant) -> Option<String> {
        let (key, since) = self.over.as_ref()?;
        if self.sprung || now < *since + SPRING_DELAY {
            return None;
        }
        self.sprung = true;
        Some(key.clone())
    }
}

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

    /// Whether `window` is on the current Space (shown, or minimised from it) of `output` (`None`: any display's).
    // ponytail: a minimised window counts as on the display its top-left corner was on.
    fn on_current_space(&self, window: &Window, output: Option<&Output>) -> bool {
        let top = self.first_origin().y;
        let hidden = || self.hidden.iter().find(|(w, _)| w == window).map(|(_, l)| *l);
        let loc = self.space.element_location(window).or_else(hidden);
        let here = output.is_none_or(|o| match self.window_output(window) {
            Some(on) => on == *o,
            None => hidden().is_none_or(|l| self.space.output_geometry(o).is_some_and(|g| g.contains(l))),
        });
        here && loc.is_none_or(|l| crate::spaces::row_at(l.y, top) == 0)
    }

    /// The items of the dock the pointer is on (`dock_output`).
    pub fn dock_items(&self) -> Vec<Item> {
        self.dock_items_on(self.dock_output().as_ref())
    }

    /// `output`'s dock's items: pinned items that are installed, in the order of its list (`dock_list`), then running
    /// apps in the order they started (with `[dock] pinned_only`, only those with a minimised window, to come back
    /// by). With `[dock] current_workspace_only`, only the current Space's apps and windows: with a dock on every
    /// display, that display's.
    pub fn dock_items_on(&self, output: Option<&Output>) -> Vec<Item> {
        let dock = &self.config.dock;
        let mut items: Vec<Item> = Vec::new();
        for name in self.dock_list(output) {
            if name == TRASH_KEY || name == PRINTING_KEY {
                continue;
            }
            if Builtin::of(name).is_some() && !items.iter().any(|i| i.key == *name) {
                items.push(Item { key: name.clone(), app: self.apps.find(name).cloned(), windows: Vec::new() });
            } else if let Some(app) = self.apps.find(name).filter(|a| !items.iter().any(|i| i.key == a.id)) {
                items.push(Item { key: app.id.clone(), app: Some(app.clone()), windows: Vec::new() });
            }
        }
        let dragging = output.is_none_or(|o| self.dock_output().as_ref() == Some(o));
        let output = output.filter(|_| dock.displays == DockDisplays::All);
        let mut windows: Vec<Window> = self.space.elements().rev().chain(self.hidden.iter().map(|(w, _)| w)).cloned().collect();
        // A window no bigger than 1×1 isn't shown: not drawn yet, or wl-copy's and wl-paste's, mapped for a moment
        // to get the keyboard focus they need to copy or paste (we have no data-control protocol).
        let drawn = |w: &Window| w.geometry().size.w > 1 || w.geometry().size.h > 1;
        windows.retain(|w| drawn(w) && !self.is_quake(w) && (!dock.current_workspace_only || self.on_current_space(w, output)));
        windows.sort_by_key(seen);
        let pinned = items.len();
        for window in windows {
            let Some((key, app)) = self.dock_key(&window) else { continue };
            match items.iter_mut().find(|i| i.key == key) {
                Some(item) => item.windows.push(window),
                None => items.push(Item { key, app, windows: vec![window] }),
            }
        }
        if dock.pinned_only {
            let running = items.split_off(pinned);
            items.extend(running.into_iter().filter(|i| i.windows.iter().any(is_minimised)));
        }
        // Topmost shown window first within each app.
        let stack: Vec<&Window> = self.space.elements().rev().collect();
        for item in &mut items {
            item.windows.sort_by_key(|w| stack.iter().position(|s| *s == w).unwrap_or(usize::MAX));
        }
        if PRINTING.load(Ordering::Relaxed) {
            items.push(Item { key: PRINTING_KEY.to_owned(), app: None, windows: Vec::new() });
        }
        // Only the dock the pointer is on makes room for a dragged icon.
        if dragging { self.dock_ui.preview(items) } else { items }
    }

    /// The displays with a dock (`[dock] displays`), the main one first.
    pub fn dock_outputs(&self) -> Vec<Output> {
        let outputs: Vec<Output> = self.space.outputs().cloned().collect();
        let pointer = self.pointer_output().and_then(|p| outputs.iter().position(|o| *o == p));
        dock_displays(self.config.dock.displays, outputs.len(), pointer).into_iter().map(|i| outputs[i].clone()).collect()
    }

    /// The dock the pointer last moved onto (`dock_follow`), else the first: hit tests, the hover, menus and drags are
    /// its. Every dock's coordinates are relative to its own display.
    pub fn dock_output(&self) -> Option<Output> {
        let docks = self.dock_outputs();
        self.dock_ui.output.clone().filter(|o| docks.contains(o)).or_else(|| docks.into_iter().next())
    }

    /// `location` (first-output-relative, as the shell gets it) relative to `dock_output`'s display.
    pub fn to_dock(&self, location: Point<f64, Logical>) -> Point<f64, Logical> {
        self.to_display(self.dock_output().as_ref(), location)
    }

    /// The pointer moved to `location` (first-output-relative): onto another display's dock, that one becomes
    /// `dock_output`, and the one it left stops zooming and hovering. An open dock menu keeps its dock.
    pub fn dock_follow(&mut self, location: Point<f64, Logical>) {
        if self.popup == Some(crate::shell::Popup::Menu(crate::shell::Menu::Dock)) {
            return;
        }
        let Some(first) = self.first_geometry() else { return };
        let at = location + first.loc.to_f64();
        let docks = self.dock_outputs();
        let Some(output) = docks.into_iter().find(|o| self.space.output_geometry(o).is_some_and(|g| g.to_f64().contains(at))) else { return };
        // The last one moved onto, even when it has no dock any more (`pointer`: the dock follows), gets redrawn.
        let Some(left) = self.dock_ui.output.replace(output.clone()).filter(|o| *o != output) else { return };
        self.dock_fx.side_mut(&left.name()).zoom.set(false, Instant::now());
        self.dock_reveal = false;
        self.dock_fx.hovered(None);
        self.dock_ui.peek = None;
        self.schedule_redraw();
    }

    /// The layout of the dock on `dock_output` for `items`.
    pub fn dock_layout(&self, items: &[Item]) -> Option<Layout> {
        self.dock_layout_on(&self.dock_output()?, items)
    }

    /// `output`'s dock's layout for `items`, zoomed around the pointer (or `--show dock-zoom`'s fake one).
    pub fn dock_layout_on(&self, output: &Output, items: &[Item]) -> Option<Layout> {
        let size = self.space.output_geometry(output)?.size;
        let dock = &self.config.dock;
        let now = Instant::now();
        let side = self.dock_fx.side(&output.name());
        // The zoom eases in and out around the last position over the dock, and goes as the dock hides (Plank).
        let eased = if self.dock_pointer.is_some() { 1.0 } else { side.zoom.value(now) * (1.0 - side.hide.value(now)) };
        let zoom = if dock.zoom { f64::from(dock.zoom_percent) / 100.0 } else { 1.0 };
        let pointer = self.dock_pointer.or(side.zoom_at);
        Some(layout(items.len(), 0, size, i32::from(dock.icon_size), zoom, eased, pointer))
    }

    /// `dock_output`'s dock is shown (or showing): the hide modes' state as of the last update.
    pub fn dock_shown(&self) -> bool {
        self.dock_output().is_some_and(|o| self.dock_shown_on(&o))
    }

    pub fn dock_shown_on(&self, output: &Output) -> bool {
        !self.dock_fx.side(&output.name()).hider.hidden
    }

    /// A cheap first check for every pointer motion: is `location` low enough to be on the dock, zoomed, at all?
    fn near_dock(&self, location: Point<f64, Logical>) -> bool {
        let height = self.dock_output().and_then(|o| self.space.output_geometry(&o)).map_or(0, |g| g.size.h);
        let dock = &self.config.dock;
        let zoom = if dock.zoom { f64::from(dock.zoom_percent) / 100.0 } else { 1.0 };
        let icon = f64::from(dock.icon_size);
        location.y >= f64::from(height) - (icon * zoom + bottom_pad(icon))
    }

    /// Whether `p` is on the shown dock: its resting area, or (while the pointer is on it) a zoomed icon.
    fn dock_on(&self, layout: &Layout, p: Point<f64, Logical>) -> bool {
        self.dock_shown() && (layout.zone.to_f64().contains(p) || (self.dock_reveal && hit(layout, p).is_some()))
    }

    /// The hovered icon's window previews once shown (E8.3): their layout, and each window with its frame
    /// (output-relative). Only windows on screen or on another Space have a frame: minimised ones get no thumbnail.
    pub fn dock_peek(&self) -> Option<(Peek, Vec<(Window, Rect)>)> {
        let peek = self.dock_ui.peek.as_ref().filter(|p| p.shown() || self.dock_ui.held_for_peek())?;
        if self.popup.is_some() || self.mission_open() || self.launchpad.is_some() || self.dock_ui.dragging() || !self.dock_shown() {
            return None;
        }
        let output = self.space.output_geometry(&self.dock_output()?)?;
        let windows: Vec<(Window, Rect)> = self
            .dock_item(&peek.key)?
            .windows
            .into_iter()
            .filter_map(|w| Some((w.clone(), self.frame(&w)?)))
            .map(|(w, f)| (w, Rectangle::new(f.loc - output.loc, f.size)))
            .collect();
        let sizes: Vec<Size<i32, Logical>> = windows.iter().map(|(_, f)| f.size).collect();
        (windows.len() >= 2).then(|| (peek_layout(&sizes, peek.at, output.size.w), windows))
    }

    /// What `location` is over on the shown previews, and their windows.
    fn dock_peek_at(&self, location: Point<f64, Logical>) -> Option<(PeekHit, Vec<(Window, Rect)>)> {
        let (peek, windows) = self.dock_peek()?;
        Some((peek_hit(&peek, self.dock_ui.peek.as_ref()?.hover, location)?, windows))
    }

    /// The pointer is over `item` (or no icon): an icon with several windows starts its preview delay; another icon
    /// or none ends the previews.
    fn dock_peek_track(&mut self, over: Option<(&Item, usize)>, layout: &Layout) {
        let several = over.filter(|(it, _)| it.windows.iter().filter(|w| self.space.element_location(w).is_some()).count() >= 2 && !self.dock_ui.dragging());
        let Some((item, i)) = several else {
            if self.dock_ui.peek.take().is_some_and(|p| p.shown()) {
                self.schedule_redraw();
            }
            return;
        };
        let zoom = if self.config.dock.zoom { f64::from(self.config.dock.zoom_percent) / 100.0 } else { 1.0 };
        let (left, right) = layout.columns[i];
        let top = f64::from(layout.zone.loc.y + layout.zone.size.h) - bottom_pad(layout.icon) - (layout.icon * zoom).round();
        let at = Point::from((((left + right) / 2.0).round() as i32, top.round() as i32));
        match self.dock_ui.peek.as_mut() {
            Some(p) if p.key == item.key => {
                if !p.shown() {
                    p.at = at;
                }
            }
            _ => {
                let since = Instant::now();
                self.dock_ui.peek = Some(Peeking { key: item.key.clone(), at, since, off: false, hover: None });
                self.dock_wake(since + PEEK_DELAY);
            }
        }
    }

    /// Whether the dock takes the pointer at `location` (output-relative).
    pub fn dock_covers(&self, location: Point<f64, Logical>) -> bool {
        if self.dock_peek_at(location).is_some() {
            return true;
        }
        if !self.near_dock(location) {
            return false;
        }
        let items = self.dock_items();
        self.dock_layout(&items).is_some_and(|l| !items.is_empty() && self.dock_on(&l, location))
    }

    /// A press at `location`; `true` if the dock took it. With a popup open the shell closes it first.
    pub fn dock_press(&mut self, location: Point<f64, Logical>, button: u32) -> bool {
        if self.popup.is_some() || self.alert.is_some() {
            return false;
        }
        // The previews: a thumbnail focuses its window (showing its Space), ✕ or the middle button closes it.
        if let Some((hit, windows)) = self.dock_peek_at(location) {
            match (hit, button) {
                (PeekHit::Close(i), BTN_LEFT) | (PeekHit::Thumb(i), BTN_MIDDLE) => {
                    if let Some(toplevel) = windows[i].0.toplevel() {
                        toplevel.send_close();
                    }
                }
                (PeekHit::Thumb(i), BTN_LEFT) => {
                    self.dock_ui.peek = None;
                    self.focus_window(Some(windows[i].0.clone()), SERIAL_COUNTER.next_serial());
                }
                _ => {}
            }
            self.schedule_redraw();
            return true;
        }
        if !self.near_dock(location) {
            return false;
        }
        if let Some(p) = self.dock_ui.peek.as_mut() {
            p.off = true;
        }
        let items = self.dock_items();
        let Some(layout) = self.dock_layout(&items).filter(|l| !items.is_empty() && self.dock_on(l, location)) else { return false };
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
            BTN_LEFT => {
                let since = Instant::now();
                // Held still on an icon with previews, they show (E8.3 long-press) instead of a drag starting.
                if self.dock_ui.peek.as_ref().is_some_and(|p| p.key == item.key) {
                    self.dock_wake(since + PEEK_DELAY);
                }
                self.dock_ui.drag = Some(Drag { key: item.key, from: self.dock_output(), start: location, since, moved: false, slot: None });
            }
            BTN_MIDDLE if self.config.dock.middle == DockMiddle::Close => item.windows.iter().filter_map(|w| w.toplevel()).for_each(|t| t.send_close()),
            BTN_MIDDLE => self.dock_click(item, true),
            BTN_RIGHT if ctrl => self.dock_settings(),
            BTN_RIGHT => self.dock_open_menu(item.key, &layout, i),
            _ => {}
        }
        true
    }

    /// A click on `item`: a builtin's action, else (Plank) a new instance with Ctrl or Shift, the middle button or no
    /// windows, else its windows come forward (`dock_activate`).
    pub(crate) fn dock_click(&mut self, item: Item, new: bool) {
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
            Builtin::Launchpad => {
                // On this dock's display.
                self.toggle_launchpad();
                let output = self.dock_output();
                if let Some(l) = self.launchpad.as_mut().filter(|l| !l.closing()) {
                    l.output = output;
                }
            }
            Builtin::AppDrawer => {
                // The drawer opens above its icon, on its dock's display.
                let items = self.dock_items();
                let at = items.iter().position(|i| i.key == key).zip(self.dock_layout(&items)).map(|(i, l)| {
                    let (left, right) = l.columns[i];
                    (((left + right) / 2.0).round() as i32, l.icons[i].loc.y.round() as i32).into()
                });
                self.toggle_drawer(at.map_or(drawer::Anchor::Corner, drawer::Anchor::Dock));
            }
            Builtin::Screenshot => self.screenshot(crate::shell::screenshot::Start::Toolbar),
            Builtin::Printing => {
                self.spawn(&["noobsbot-desktop-settings".into(), "--page".into(), "printers".into()]);
            }
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
            return match self.config.dock.click {
                DockClick::Toggle => self.minimise(shown),
                DockClick::Cycle => self.dock_step(shown, true),
            };
        }
        // Bottom first, so their order is kept and the topmost ends on top with focus.
        for window in shown.iter().rev().skip(1) {
            self.space.raise_element(window, false);
        }
        self.focus_window(shown.first().cloned(), SERIAL_COUNTER.next_serial());
        self.schedule_redraw();
    }

    /// Starts `app` from its `Exec` line, with Plank's launch bounce. `false`: it didn't start (no bounce either).
    pub fn launch(&mut self, app: Option<&App>) -> bool {
        let Some(app) = app else { return false };
        tracing::info!(app = app.id, "dock: launch");
        self.dock_fx.tip_hidden = true;
        self.schedule_redraw();
        let started = self.spawn(&app.command(&self.config.terminal)).is_some();
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
            self.dock_save(move |text| config::set_key(text, "dock", "icon_size", &size.to_string()));
            self.config.dock.icon_size = size as u16;
            self.schedule_redraw();
            return true;
        }
        let forward = match self.config.dock.scroll {
            DockScroll::Off => return true,
            DockScroll::On => dir > 0.0,
            DockScroll::Reverse => dir < 0.0,
        };
        let items = self.dock_items();
        let Some(item) = self.dock_layout(&items).and_then(|l| hit(&l, location)).and_then(|i| items.into_iter().nth(i)) else { return true };
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
        self.dock_step(item.windows, forward);
        true
    }

    /// Focuses the window after (or before) the focused one of `windows`, in the order they opened.
    pub(crate) fn dock_cycle(&mut self, mut windows: Vec<Window>, forward: bool) {
        windows.sort_by_key(seen);
        let focused = self.focused_window();
        if let Some(next) = cycle_target(&[windows], 0, focused.as_ref(), forward, false) {
            self.dock_bring(next);
        }
    }

    /// A dock click or scroll cycling `windows` (one app's): with `[dock] cycle_across_apps`, past its last window
    /// into the next running dock app's.
    fn dock_step(&mut self, windows: Vec<Window>, forward: bool) {
        if !self.config.dock.cycle_across_apps {
            return self.dock_cycle(windows, forward);
        }
        let mut groups: Vec<Vec<Window>> = self.dock_items().into_iter().filter(|i| !i.windows.is_empty()).map(|i| i.windows).collect();
        groups.iter_mut().for_each(|g| g.sort_by_key(seen));
        let Some(here) = groups.iter().position(|g| windows.first().is_some_and(|w| g.contains(w))) else {
            return self.dock_cycle(windows, forward);
        };
        let focused = self.focused_window();
        if let Some(next) = cycle_target(&groups, here, focused.as_ref(), forward, true) {
            self.dock_bring(next);
        }
    }

    /// `window` comes back if minimised and gets focus.
    fn dock_bring(&mut self, next: Window) {
        if let Some(at) = self.hidden.iter().position(|(w, _)| *w == next) {
            let (_, loc) = self.hidden.remove(at);
            self.set_visible_loc(&next, loc);
        }
        self.focus_window(Some(next), SERIAL_COUNTER.next_serial());
    }

    /// Super+n (E8.3): the n-th icon's app comes forward, or its next window if it has focus, or it opens; `new`:
    /// a new window. A builtin does its action.
    pub fn dock_app(&mut self, n: usize, new: bool) {
        let Some(item) = self.dock_items().into_iter().nth(n) else { return };
        if !new && Builtin::of(&item.key).is_none() && self.focused_window().is_some_and(|f| item.windows.contains(&f)) {
            self.dock_fx.clicked(item.key.clone());
            self.dock_cycle(item.windows, true);
            return self.schedule_redraw();
        }
        // Not focused, so `dock_activate` brings it forward rather than minimising it.
        self.dock_click(item, new);
        self.schedule_redraw();
    }

    /// The pointer moved. Plank's cursor region: the dock's resting area, shrinking to a 1 px strip at the screen
    /// edge as it hides, plus the hovered icon while zoomed. Being in it is "hovered": it shows the dock, zooms it
    /// and lightens the icon under the pointer.
    pub fn dock_motion(&mut self, from: Point<f64, Logical>, to: Point<f64, Logical>) {
        self.dock_drag_motion(to);
        self.dock_spring_motion(to);
        // On the previews the dock stays as it is (shown, zoomed where it was); only the hovered thumbnail changes.
        if let Some((hit, _)) = self.dock_peek_at(to) {
            let hover = match hit {
                PeekHit::Close(i) | PeekHit::Thumb(i) => Some(i),
                PeekHit::Strip => None,
            };
            if let Some(p) = self.dock_ui.peek.as_mut().filter(|p| p.hover != hover) {
                p.hover = hover;
                self.schedule_redraw();
            }
            return;
        }
        if !self.dock_reveal && !self.near_dock(from) && !self.near_dock(to) {
            return;
        }
        let items = self.dock_items();
        let Some(output) = self.dock_output() else { return };
        let (Some(layout), Some(size)) = (self.dock_layout(&items), self.space.output_geometry(&output).map(|g| g.size)) else {
            return;
        };
        let now = Instant::now();
        let zone = layout.zone.to_f64();
        let strip = (zone.size.h * (1.0 - self.dock_fx.side(&output.name()).hide.value(now))).max(1.0);
        let h = f64::from(size.h);
        let in_region = !items.is_empty() && to.x >= zone.loc.x && to.x < zone.loc.x + zone.size.w && to.y >= h - strip && to.y < h;
        let over = hit(&layout, to).filter(|_| self.dock_shown() && (in_region || self.dock_reveal));
        let hovered = in_region || over.is_some();
        let before = self.dock_fx.hovered_key().map(str::to_owned);
        self.dock_fx.hovered(over.map(|i| items[i].key.as_str()));
        self.dock_peek_track(over.map(|i| (&items[i], i)), &layout);
        let side = self.dock_fx.side_mut(&output.name());
        side.zoom.set(hovered, now);
        if hovered {
            side.zoom_at = Some(to);
        }
        let changed = hovered != self.dock_reveal;
        self.dock_reveal = hovered;
        if changed {
            self.dock_update_hide(&output, &layout);
        }
        if changed || (hovered && self.config.dock.zoom) || before.as_deref() != self.dock_fx.hovered_key() {
            self.schedule_redraw();
        }
    }
}

/// The dock's interactions: the right-click menu, dragging an icon, files dragged in from an app.
#[derive(Default)]
pub struct Ui {
    /// The display whose dock the pointer last moved onto (`State::dock_output`).
    pub output: Option<Output>,
    /// The item key the open menu is for, and the point (output-relative) it stands centred over.
    pub menu: Option<(String, Point<i32, Logical>)>,
    drag: Option<Drag>,
    /// The `text/uri-list` of the app drag in progress, read as it starts.
    pub uris: Arc<Mutex<Option<String>>>,
    /// The icon whose window previews show (after PEEK_DELAY).
    pub peek: Option<Peeking>,
    /// The open menu's recent files (URIs) and Open at Login state, read as it opens.
    recent: Vec<String>,
    login: Option<bool>,
    /// While an app's drag (DnD) is on.
    spring: Option<Spring>,
}

/// An icon with several windows, hovered.
pub struct Peeking {
    key: String,
    /// The zoomed icon's top centre, kept once the strip shows so it doesn't follow the zoom.
    at: Point<i32, Logical>,
    since: Instant,
    /// Clicked: no previews until the pointer leaves the icon.
    off: bool,
    /// The thumbnail under the pointer.
    pub hover: Option<usize>,
}

impl Peeking {
    pub fn shown(&self) -> bool {
        !self.off && self.since.elapsed() >= PEEK_DELAY
    }
}

/// A press held `held` shows the previews (long-press) unless the pointer `moved` into a drag first.
fn long_press_peek(held: Duration, moved: bool) -> bool {
    !moved && held >= PEEK_DELAY
}

/// An icon held down by the left button.
struct Drag {
    key: String,
    /// The display whose dock it was picked up from: dropped off every dock, it is unpinned there.
    from: Option<Output>,
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

    /// The held icon's previews show from a long press: no drag, no click.
    fn held_for_peek(&self) -> bool {
        let (Some(drag), Some(peek)) = (&self.drag, &self.peek) else { return false };
        drag.key == peek.key && long_press_peek(drag.since.elapsed(), drag.moved)
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
    /// Brings the app's n-th window forward (restoring it if minimised).
    Window(usize),
    /// Opens the n-th of the app's recent files with it.
    Recent(usize),
    /// Keep in Dock, ticked while pinned.
    Pin,
    /// Open at Login: a copy of the app's desktop file in `~/.config/autostart`.
    Login,
    /// Shows the app's desktop file in the file manager.
    ShowInFiles,
    /// Options ▸ Assign To: `ASSIGN[n]`.
    Assign(usize),
    /// Options ▸ Move to Display: the app's windows go to the n-th display.
    ToDisplay(usize),
    /// Close / Close All.
    Close,
    /// Closes every other app's windows, as Quit does.
    CloseOthers,
    /// The app's n-th desktop action.
    Action(usize),
    /// Separate Items on This Display: the dock's display gets its own pinned list, or goes back to the shared one.
    Separate,
    /// Printing's Open Printers: what a click does.
    Open,
}

/// Options ▸ Assign To, in order: `[dock] assign` "all", the Space it was chosen on, nothing.
pub const ASSIGN: [&str; 3] = ["All Spaces", "This Space", "None"];

/// What the Options submenu shows for an app.
#[derive(Debug, Default)]
pub struct Options {
    /// Keep in Dock, ticked while pinned; `None` when it can't be pinned.
    pub pinned: Option<bool>,
    /// Separate Items on This Display, ticked while the display has its own list; `None` hides it (one display, or
    /// items locked).
    pub separate: Option<bool>,
    /// Open at Login, ticked while on; `None` without a desktop file, which also hides Show in Files.
    pub login: Option<bool>,
    /// Assign To with `ASSIGN[n]` ticked; `None` hides it (a builtin).
    pub assign: Option<usize>,
    /// Move to Display: every display by name, and whether the app's windows can go there (not the one its front
    /// window is on). Shown with more than one display.
    pub displays: Vec<(String, bool)>,
}

/// The menu, in order: the app's windows (`titles`, the `active` one greyed), its `recent` files, its desktop
/// actions, the Options submenu (Keep in Dock, Open at Login, Show in Files, then the Assign To and Move to Display
/// submenus), then Close (Close All with several windows) and Close Others while `others` (other apps have windows).
pub fn menu_rows(titles: &[String], active: Option<usize>, recent: &[String], actions: &[String], options: &Options, others: bool) -> Vec<Option<Row>> {
    let row =
        |label: &str, shortcut, command: Option<Command>| Some(Row { sub: Vec::new(), label: label.to_owned(), shortcut, action: command.map(Action::Dock) });
    let tick = |on: bool| if on { "✓" } else { "" };
    let windows = titles.iter().enumerate().map(|(i, t)| row(t, "", (active != Some(i)).then_some(Command::Window(i)))).collect();
    let recent = recent.iter().enumerate().map(|(i, r)| row(r, "", Some(Command::Recent(i)))).collect();
    let actions = actions.iter().enumerate().map(|(i, a)| row(a, "", Some(Command::Action(i)))).collect();
    let mut sub = Vec::new();
    if let Some(pinned) = options.pinned {
        sub.push(row("Keep in Dock", tick(pinned), Some(Command::Pin)));
    }
    if let Some(on) = options.separate {
        sub.push(row("Separate Items on This Display", tick(on), Some(Command::Separate)));
    }
    if let Some(login) = options.login {
        sub.push(row("Open at Login", tick(login), Some(Command::Login)));
        sub.push(row("Show in Files", "", Some(Command::ShowInFiles)));
    }
    // Assign To and Move to Display open their own submenus, after a line.
    let nested = |label: &str, sub| Some(Row { sub, label: label.to_owned(), shortcut: "", action: None });
    let mut more = Vec::new();
    if let Some(on) = options.assign {
        more.push(nested("Assign To", ASSIGN.iter().enumerate().map(|(i, name)| row(name, tick(i == on), Some(Command::Assign(i)))).collect()));
    }
    if options.displays.len() > 1 {
        let displays = options.displays.iter().enumerate().map(|(i, (name, can))| row(name, "", can.then_some(Command::ToDisplay(i))));
        more.push(nested("Move to Display", displays.collect()));
    }
    if !sub.is_empty() && !more.is_empty() {
        sub.push(None);
    }
    sub.extend(more);
    let options = if sub.is_empty() { Vec::new() } else { vec![Some(Row { sub, label: "Options".to_owned(), shortcut: "", action: None })] };
    let mut close = if titles.is_empty() { Vec::new() } else { vec![row(if titles.len() > 1 { "Close All" } else { "Close" }, "", Some(Command::Close))] };
    if others {
        close.push(row("Close Others", "", Some(Command::CloseOthers)));
    }
    let mut rows = Vec::new();
    for group in [windows, recent, actions, options, close] {
        if group.is_empty() {
            continue;
        }
        if !rows.is_empty() {
            rows.push(None);
        }
        rows.extend(group);
    }
    rows
}

/// Each bookmark in a `recently-used.xbel`: its `href` and the apps that used it, with when (ISO 8601, UTC).
// ponytail: a string scan, not an XML parser; enough for the files GLib and Qt apps write.
fn bookmarks(xbel: &str) -> Vec<(String, Vec<(String, String)>)> {
    let attr = |tag: &str, key: &str| -> Option<String> {
        let start = tag.find(&format!(" {key}=\""))? + key.len() + 3;
        let value = &tag[start..start + tag[start..].find('"')?];
        Some(value.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&"))
    };
    // The tag up to `>`, with a space before its first attribute, which the split ate.
    let head = |s: &str| format!(" {}", &s[..s.find('>').unwrap_or(s.len())]);
    let mut found = Vec::new();
    for bookmark in xbel.split("<bookmark ").skip(1) {
        let Some(href) = attr(&head(bookmark), "href") else { continue };
        let apps = bookmark
            .split("<bookmark:application ")
            .skip(1)
            .map(head)
            .filter_map(|a| Some((attr(&a, "name")?, attr(&a, "modified").unwrap_or_default())))
            .collect();
        found.push((href, apps));
    }
    found
}

/// The `href`s in a `recently-used.xbel` whose bookmark names one of `apps` (any case), most recently used first.
pub fn recent_files(xbel: &str, apps: &[&str]) -> Vec<String> {
    let mut found: Vec<(String, String)> = bookmarks(xbel)
        .into_iter()
        .filter_map(|(href, used)| {
            let at = used.into_iter().filter(|(n, _)| apps.iter().any(|app| app.eq_ignore_ascii_case(n))).map(|(_, at)| at).max()?;
            Some((at, href))
        })
        .collect();
    // ISO 8601 times in UTC sort as text.
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, href)| href).collect()
}

/// Every app named in a `recently-used.xbel` and every file in it, each most recently used first.
pub fn recent_items(xbel: &str) -> (Vec<String>, Vec<String>) {
    let marks = bookmarks(xbel);
    let mut files: Vec<(String, String)> =
        marks.iter().map(|(href, used)| (used.iter().map(|(_, at)| at.clone()).max().unwrap_or_default(), href.clone())).collect();
    files.sort_by(|a, b| b.0.cmp(&a.0));
    let mut apps: Vec<(String, String)> = marks.into_iter().flat_map(|(_, used)| used).map(|(name, at)| (at, name)).collect();
    apps.sort_by(|a, b| b.cmp(a));
    let mut names: Vec<String> = Vec::new();
    for (_, name) in apps {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    (names, files.into_iter().map(|(_, href)| href).collect())
}

impl State {
    /// Opens the file manager with `path` selected (its ShowItems); without one on the bus, opens its folder.
    pub fn show_in_files(&mut self, path: &std::path::Path) {
        let dir = path.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
        let uri = format!("file://{}", path.display());
        let script = "dbus-send --session --print-reply --dest=org.freedesktop.FileManager1 /org/freedesktop/FileManager1 \
                      org.freedesktop.FileManager1.ShowItems array:string:\"$1\" string: >/dev/null 2>&1 || xdg-open \"$2\"";
        self.spawn(&["sh", "-c", script, "sh", &uri, &dir].map(String::from));
    }
}

/// `~/.local/share/recently-used.xbel`.
pub fn recent_path() -> std::path::PathBuf {
    config::xdg_dir("XDG_DATA_HOME", ".local/share").join("recently-used.xbel")
}

/// A `recently-used.xbel` with every bookmark gone (Clear Menu); `None` if it has no `<xbel>` to keep.
pub fn cleared(xbel: &str) -> Option<String> {
    let start = xbel.find("<xbel")?;
    let end = start + xbel[start..].find('>')? + 1;
    Some(format!("{}\n</xbel>\n", &xbel[..end]))
}

/// `~/.config/autostart/<id>.desktop`.
fn autostart_path(id: &str) -> std::path::PathBuf {
    config::xdg_dir("XDG_CONFIG_HOME", ".config").join("autostart").join(format!("{id}.desktop"))
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

/// `pairs` (key, TOML value) as a one-line TOML inline table.
fn inline(pairs: impl Iterator<Item = (String, String)>) -> String {
    let pairs: Vec<String> = pairs.map(|(k, v)| format!("{} = {v}", quote(&k))).collect();
    if pairs.is_empty() { "{}".to_owned() } else { format!("{{ {} }}", pairs.join(", ")) }
}

/// `map` as a one-line TOML inline table.
fn inline_table(map: &BTreeMap<String, String>) -> String {
    inline(map.iter().map(|(k, v)| (k.clone(), quote(v))))
}

/// A config.toml edit.
pub type Edit = Box<dyn FnOnce(&str) -> String + Send>;

/// `text` with `[dock] outputs` set to `lists`, all on one line: a hand-written `[dock.outputs]` table goes, not
/// duplicated.
pub fn set_outputs(text: &str, lists: &BTreeMap<String, Vec<String>>) -> String {
    let table = inline(lists.iter().map(|(k, v)| (k.clone(), format!("[{}]", v.iter().map(|s| quote(s)).collect::<Vec<_>>().join(", ")))));
    config::set_key(&config::set_section(text, "dock.outputs", ""), "dock", "outputs", &table)
}

/// Separate Items on This Display: `display` gets a copy of the shared list, or loses its own; returns the edit that
/// saves it.
pub fn separate(dock: &mut config::Dock, display: &str) -> Edit {
    if dock.outputs.remove(display).is_none() {
        dock.outputs.insert(display.to_owned(), dock.items.clone());
    }
    let lists = dock.outputs.clone();
    Box::new(move |text| set_outputs(text, &lists))
}

/// Puts `items` in `display`'s dock list (its own, else the shared `[dock] items`); returns the edit that saves it.
pub fn store_pins(dock: &mut config::Dock, display: Option<&str>, items: Vec<String>) -> Edit {
    match display.and_then(|d| dock.outputs.get_mut(d)) {
        Some(list) => {
            *list = items;
            let lists = dock.outputs.clone();
            Box::new(move |text| set_outputs(text, &lists))
        }
        None => {
            dock.items = items.clone();
            Box::new(move |text| set_items(text, &items))
        }
    }
}

/// The URIs of a `text/uri-list` (comments and blank lines skipped).
pub fn uri_list(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from).collect()
}

impl State {
    /// The item key a `[dock] items` entry pins: a builtin, or the app it names (`None`: not installed).
    fn pin_key(&self, name: &str) -> Option<String> {
        match Builtin::of(name) {
            Some(Builtin::Printing) => None,
            Some(_) => Some(name.to_owned()),
            None if name == TRASH_KEY => None,
            None => self.apps.find(name).map(|a| a.id.clone()),
        }
    }

    /// `output`'s dock's pinned list: the display's own (`[dock] outputs`), else `[dock] items`.
    fn dock_list(&self, output: Option<&Output>) -> &[String] {
        self.config.dock.items_on(output.map(|o| o.name()).as_deref())
    }

    /// The pinned icons on the dock: (index in its list, item key), left to right.
    pub(crate) fn dock_pinned(&self) -> Vec<(usize, String)> {
        self.dock_pinned_on(self.dock_output().as_ref())
    }

    fn dock_pinned_on(&self, output: Option<&Output>) -> Vec<(usize, String)> {
        let mut pinned: Vec<(usize, String)> = Vec::new();
        for (i, name) in self.dock_list(output).iter().enumerate() {
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
        if item.key == PRINTING_KEY {
            return vec![Some(Row { sub: Vec::new(), label: "Open Printers".into(), shortcut: "", action: Some(Action::Dock(Command::Open)) })];
        }
        let name = item.title().to_owned();
        let titles: Vec<String> = item
            .windows
            .iter()
            .map(|w| {
                let title = w
                    .toplevel()
                    .and_then(|t| with_states(t.wl_surface(), |s| s.data_map.get::<XdgToplevelSurfaceData>().and_then(|d| d.lock().ok()?.title.clone())));
                let title = title.filter(|t| !t.is_empty()).map_or_else(|| name.clone(), |t| short_title(&t, &name));
                if self.hidden.iter().any(|(h, _)| h == w) { format!("◆ {title}") } else { title }
            })
            .collect();
        let focused = self.focused_window();
        let active = item.windows.iter().position(|w| Some(w) == focused.as_ref());
        let pinnable = !self.config.dock.lock_items && (item.app.is_some() || Builtin::of(&item.key).is_some());
        let pinned = pinnable.then(|| self.dock_pinned().iter().any(|(_, id)| *id == item.key));
        let actions: Vec<String> = item.app.as_ref().map(|a| crate::apps::actions(&a.id).into_iter().map(|(n, _)| n).collect()).unwrap_or_default();
        let name_of = |uri: &String| crate::apps::file_path(uri).and_then(|p| Some(std::path::Path::new(&p).file_name()?.to_string_lossy().into_owned()));
        let recent: Vec<String> = self.dock_ui.recent.iter().map(|u| name_of(u).unwrap_or_else(|| u.clone())).collect();
        let assign = Builtin::of(&item.key).is_none().then(|| match self.config.dock.assign.get(&item.key).and_then(|v| Assign::parse(v)) {
            Some(Assign::All) => 0,
            Some(Assign::Space { .. }) => 1,
            None => 2,
        });
        let outputs: Vec<_> = self.space.outputs().collect();
        let own = self.dock_output().is_some_and(|o| self.config.dock.outputs.contains_key(&o.name()));
        let separate = (!self.config.dock.lock_items && (own || outputs.len() > 1)).then_some(own);
        let here = item.windows.first().and_then(|w| self.window_output(w));
        let displays = if item.windows.is_empty() { Vec::new() } else { outputs.iter().map(|o| (o.name(), here.as_ref() != Some(*o))).collect() };
        menu_rows(
            &titles,
            active,
            &recent,
            &actions,
            &Options { pinned, separate, login: self.dock_ui.login, assign, displays },
            Builtin::of(&item.key).is_none() && !self.dock_others(&item.key).is_empty(),
        )
    }

    /// Right click: icon `i`'s menu, centred over it.
    fn dock_open_menu(&mut self, key: String, layout: &Layout, i: usize) {
        let (left, right) = layout.columns[i];
        let at = Point::from((((left + right) / 2.0).round() as i32, layout.icons[i].loc.y.round() as i32));
        // An app with a desktop file (not a builtin) gets Open at Login, Show in Files and its recent files.
        let app = self.apps.find(&key).filter(|a| Builtin::of(&key).is_none() && !a.path.as_os_str().is_empty());
        self.dock_ui.login = app.map(|a| std::fs::read_to_string(autostart_path(&a.id)).is_ok_and(|t| !t.contains("Hidden=true")));
        self.dock_ui.recent = app.map_or_else(Vec::new, |a| {
            let xbel = std::fs::read_to_string(recent_path()).unwrap_or_default();
            let program = a.program();
            let files = recent_files(&xbel, &[&a.id, &a.name, &program]);
            files.into_iter().filter(|u| crate::apps::file_path(u).is_none_or(|p| std::path::Path::new(&p).exists())).take(5).collect()
        });
        self.dock_ui.menu = Some((key, at));
        self.dock_fx.tip_hidden = true;
        self.popup = Some(crate::shell::Popup::Menu(crate::shell::Menu::Dock));
        self.schedule_redraw();
    }

    /// `--show dock-menu`: the first running app's menu, else the first icon's.
    pub fn dock_show_menu(&mut self) {
        let items = self.dock_items();
        let Some(layout) = self.dock_layout(&items) else { return };
        if let Some(i) = items.iter().position(|i| !i.windows.is_empty()).or((!items.is_empty()).then_some(0)) {
            self.dock_open_menu(items[i].key.clone(), &layout, i);
        }
    }

    /// Ctrl+right click: Settings ▸ Desktop & Dock.
    fn dock_settings(&mut self) {
        self.spawn(&["noobsbot-desktop-settings".into(), "--page".into(), "desktop-dock".into()]);
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
            Command::Separate => {
                let Some(output) = self.dock_output() else { return };
                let edit = separate(&mut self.config.dock, &output.name());
                self.dock_save(edit);
            }
            Command::Recent(i) => {
                let (Some(app), Some(uri)) = (item.app, self.dock_ui.recent.get(i).cloned()) else { return };
                self.spawn(&app.command_with(&[uri], &self.config.terminal));
            }
            Command::Login => {
                let Some(app) = item.app else { return };
                let path = autostart_path(&app.id);
                let done = if self.dock_ui.login == Some(true) {
                    std::fs::remove_file(&path)
                } else {
                    path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::copy(&app.path, &path).map(drop))
                };
                let _ = done.inspect_err(|err| tracing::warn!("{}: {err}", path.display()));
            }
            Command::ShowInFiles => {
                let Some(app) = item.app else { return };
                self.show_in_files(&app.path);
            }
            Command::Assign(i) => {
                let here = self.pointer_output().map(|o| (o.name(), self.output_key(&o)));
                let assign = match (i, here) {
                    (0, _) => Some(Assign::All),
                    (1, Some((display, key))) => Some(Assign::Space { display, desk: self.desks_at(&key).current }),
                    _ => None,
                };
                match assign {
                    Some(a) => self.config.dock.assign.insert(item.key.clone(), a.value()),
                    None => self.config.dock.assign.remove(&item.key),
                };
                let table = inline_table(&self.config.dock.assign);
                // A hand-written `[dock.assign]` table goes: the whole map is written as one line under `[dock]`.
                self.dock_save(move |text| config::set_key(&config::set_section(text, "dock.assign", ""), "dock", "assign", &table));
            }
            Command::ToDisplay(to) => {
                let outputs: Vec<_> = self.space.outputs().cloned().collect();
                for window in &item.windows {
                    if let Some(from) = self.window_output(window).and_then(|o| outputs.iter().position(|x| *x == o)) {
                        self.move_to_display(window, to as i32 - from as i32);
                    }
                }
            }
            Command::Close => item.windows.iter().filter_map(|w| w.toplevel()).for_each(|t| t.send_close()),
            Command::CloseOthers => self.dock_others(&item.key).iter().filter_map(|w| w.toplevel()).for_each(|t| t.send_close()),
            Command::Open => self.dock_click(item, false),
            Command::Action(i) => {
                let Some(app) = item.app else { return };
                let Some((_, exec)) = crate::apps::actions(&app.id).into_iter().nth(i) else { return };
                self.spawn(&App { exec, ..app }.command(&self.config.terminal));
            }
        }
        self.schedule_redraw();
    }

    /// The windows, shown and minimised, of every dock app but `key` (not the quake terminal).
    fn dock_others(&self, key: &str) -> Vec<Window> {
        let all = self.space.elements().chain(self.hidden.iter().map(|(w, _)| w));
        all.filter(|w| !self.is_quake(w) && self.dock_key(w).is_some_and(|(k, _)| k != key)).cloned().collect()
    }

    /// Rewrites config.toml with `edit`, in the file the user edits (through a dotfiles symlink), so the live
    /// reload agrees.
    fn dock_save(&mut self, edit: impl FnOnce(&str) -> String + Send + 'static) {
        let path = config::path();
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        // Saved later and coalesced: a Ctrl+scroll writes once at the end, not per notch. A file that doesn't parse
        // left `self.config` at the defaults: an edit from those would write over the user's pins.
        config::save_later(path.clone(), move |text| {
            let text = if text.is_empty() { config::DEFAULT_TOML } else { text };
            match config::parse(text) {
                Ok(_) => edit(text),
                Err(err) => {
                    tracing::warn!("{}: {err}; the dock isn't saved over a config that doesn't parse", path.display());
                    text.to_owned()
                }
            }
        });
    }

    /// Moves `key` to pinned slot `to` of the dock the pointer is on, or unpins it there, and saves its list.
    pub fn dock_repin(&mut self, key: &str, to: Option<usize>) {
        self.dock_repin_on(self.dock_output(), key, to);
    }

    /// `dock_repin` on `output`'s dock: its own list (`[dock] outputs`), else `[dock] items`.
    fn dock_repin_on(&mut self, output: Option<Output>, key: &str, to: Option<usize>) {
        let list = self.dock_list(output.as_ref()).to_vec();
        let shown: Vec<usize> = self.dock_pinned_on(output.as_ref()).iter().map(|(i, _)| *i).collect();
        let drop: Vec<usize> = list.iter().enumerate().filter(|(_, n)| self.pin_key(n).as_deref() == Some(key)).map(|(i, _)| i).collect();
        let items = repin(&list, &shown, key, &drop, to);
        if items == list {
            return;
        }
        let edit = store_pins(&mut self.config.dock, output.map(|o| o.name()).as_deref(), items);
        self.dock_save(edit);
        self.schedule_redraw();
    }

    /// The pointer moved to `to` with an icon held: past the threshold it drags (unless items are locked), the icon
    /// follows the pointer and the others make room.
    fn dock_drag_motion(&mut self, to: Point<f64, Logical>) {
        let Some(drag) = self.dock_ui.drag.as_ref().filter(|_| !self.dock_ui.held_for_peek()) else { return };
        let (start, key) = (drag.start, drag.key.clone());
        if self.config.dock.lock_items || key == PRINTING_KEY || (!drag.moved && (to.x - start.x).hypot(to.y - start.y) < DRAG_START) {
            return;
        }
        let Some(layout) = self.dock_layout(&self.dock_items()) else { return };
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
        // A long press's previews stay up, for a thumbnail to be picked.
        if self.dock_ui.held_for_peek() {
            self.dock_ui.drag = None;
            if let Some(p) = self.dock_ui.peek.as_mut() {
                p.off = false;
            }
            return self.schedule_redraw();
        }
        let Some(drag) = self.dock_ui.drag.take() else { return };
        let Some(item) = self.dock_items_on(drag.from.as_ref()).into_iter().find(|i| i.key == drag.key) else { return };
        if !drag.moved {
            let items = self.dock_items();
            let same = self.dock_layout(&items).and_then(|l| hit(&l, location)).is_some_and(|i| items[i].key == item.key);
            if same && drag.since.elapsed() < LONG_PRESS {
                let mods = self.modifiers();
                self.dock_click(item, mods.ctrl || mods.shift);
            }
            return self.schedule_redraw();
        }
        let pinned = self.dock_pinned_on(drag.from.as_ref()).iter().any(|(_, id)| *id == item.key);
        let builtin = Builtin::of(&item.key).is_some();
        // Dropped on a dock (another display's too) it is pinned on that dock's list; off every dock, unpinned from
        // the one it came from.
        match dropped(drag.slot, pinned, item.app.is_some() || builtin, builtin) {
            Some(Some(to)) => self.dock_repin(&item.key, Some(to)),
            Some(None) => {
                self.dock_repin_on(drag.from, &item.key, None);
                self.dock_fx.poof(location);
            }
            None => {}
        }
        self.schedule_redraw();
    }

    /// An app started a drag: its `text/uri-list`, if it offers one, is read now, since a drop on the dock (no client
    /// surface) gets cancelled before we could ask.
    pub fn dock_drag_started(&mut self, source: Option<WlDataSource>) {
        self.dock_ui.spring = Some(Spring::default());
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

    /// An app's drag ended, dropped or not.
    pub fn dock_drag_ended(&mut self) {
        self.dock_ui.spring = None;
    }

    /// An app's drag (or the screenshot thumbnail's) moved to `to`: resting on an icon arms its spring.
    fn dock_spring_motion(&mut self, to: Point<f64, Logical>) {
        if self.screenshots.dragging {
            self.dock_ui.spring.get_or_insert_default();
        }
        if self.dock_ui.spring.is_none() {
            return;
        }
        let items = self.dock_items();
        let key = self.dock_layout(&items).and_then(|l| hit(&l, to)).filter(|_| self.dock_shown()).map(|i| items[i].key.as_str());
        if self.dock_ui.spring.as_mut().is_some_and(|s| s.over(key, Instant::now())) {
            let timer = Timer::from_duration(SPRING_DELAY);
            let _ = self.loop_handle.insert_source(timer, |_, _, state| {
                state.dock_spring();
                TimeoutAction::Drop
            });
        }
    }

    /// Spring-loading: a file rested on an icon brings its app's front window up, or App Exposé with several.
    fn dock_spring(&mut self) {
        let Some(key) = self.dock_ui.spring.as_mut().and_then(|s| s.due(Instant::now())) else { return };
        let Some(item) = self.dock_item(&key) else { return };
        let shown: Vec<&Window> = item.windows.iter().filter(|w| !self.hidden.iter().any(|(h, _)| h == *w)).collect();
        if shown.len() > 1 {
            if !self.mission_open() {
                let app = self.app_key(shown[0]);
                self.open_mission(app);
            }
        } else if let Some(window) = item.windows.first().cloned() {
            if let Some(at) = self.hidden.iter().position(|(w, _)| *w == window) {
                let (_, loc) = self.hidden.remove(at);
                self.set_visible_loc(&window, loc);
            }
            self.focus_window(Some(window), SERIAL_COUNTER.next_serial());
        }
        self.schedule_redraw();
    }

    /// An app's drag ended over no window: on the dock, `.desktop` files are pinned where they land (not while items
    /// are locked), other files open with the app they land on.
    // ponytail: only `.desktop` files of installed apps (in an `applications/` dir) pin; others are ignored.
    pub fn dock_drop(&mut self) {
        let location = self.to_dock(self.to_output(self.pointer_at));
        let text = self.dock_ui.uris.lock().unwrap_or_else(|e| e.into_inner()).take();
        let uris = uri_list(&text.unwrap_or_default());
        if uris.is_empty() || !self.dock_covers(location) {
            return;
        }
        let items = self.dock_items();
        let Some(layout) = self.dock_layout(&items) else { return };
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
            self.spawn(&app.command_with(&uris, &self.config.terminal));
        }
    }
}

const URI_LIST: &str = "text/uri-list";

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: (i32, i32) = (1920, 1200);

    #[test]
    fn a_long_still_press_peeks() {
        assert!(!long_press_peek(Duration::from_millis(300), false));
        assert!(long_press_peek(Duration::from_millis(500), false));
        assert!(!long_press_peek(Duration::from_secs(2), true), "moved first: a drag");
    }

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
        let l = layout(8, 0, SCREEN.into(), 84, 1.5, 0.0, None);
        // Transparent-Tight at 84 px: ItemPadding 4 px, BottomPadding 16 px; 8 × 88 = 704 px, centred.
        assert_eq!(l.zone, Rectangle::new((608, 1100).into(), (704, 100).into()));
        assert_eq!(l.icons[0], Rectangle::new((610.0, 1100.0).into(), (84.0, 84.0).into()));
        assert_eq!(l.icons[1].loc.x - l.icons[0].loc.x, 88.0);
        assert_eq!(l.columns[0], (608.0, 696.0));
    }

    #[test]
    fn zoom_is_planks() {
        let flat = layout(8, 0, SCREEN.into(), 84, 1.5, 0.0, None);
        let centre: Point<f64, Logical> = (916.0, 1142.0).into();
        assert_eq!(flat.icons[3].loc.x + 42.0, centre.x);
        let l = layout(8, 0, SCREEN.into(), 84, 1.5, 1.0, Some(centre));
        assert_eq!(l.icons[3], Rectangle::new((853.0, 1058.0).into(), (126.0, 126.0).into()), "150% under the pointer, same baseline");
        // A neighbour 88 px away: zoom 1 + (1 − (88/126)²) × 0.5, moved out by 88 × 0.5 × (1 − 88/378).
        let f: f64 = 88.0 / 126.0;
        assert_eq!(l.icons[4].size.w, (84.0 * (1.0 + (1.0 - f * f) * 0.5)).round());
        let shift = 88.0 * 0.5 * (1.0 - f / 3.0);
        assert!((l.icons[4].loc.x + l.icons[4].size.w / 2.0 - (1004.0 + shift)).abs() <= 0.5, "placed on a whole pixel");
        assert_eq!(l.icons[4].loc.x.fract(), 0.0);
        // Out of reach (126 px): unzoomed, pushed aside by 126 × 0.5 × 2/3 = 42 px.
        assert_eq!(l.icons[7].size.w, 84.0);
        assert_eq!(l.icons[7].loc.x, flat.icons[7].loc.x + 42.0);
        assert_eq!(l.icons[0].loc.x, flat.icons[0].loc.x - 42.0);
        assert_eq!(hit(&l, centre), Some(3));
        assert_eq!(hit(&l, (916.0, 1060.0).into()), Some(3), "the zoomed top of the icon counts");
        // Half-way through the easing: 125%.
        assert_eq!(layout(8, 0, SCREEN.into(), 84, 1.5, 0.5, Some(centre)).icons[3].size.w, 105.0);
        // No easing or zoom off: flat, wherever the pointer is.
        assert_eq!(layout(8, 0, SCREEN.into(), 84, 1.5, 0.0, Some(centre)), flat);
        assert_eq!(layout(8, 0, SCREEN.into(), 84, 1.0, 1.0, Some(centre)), flat);
    }

    #[test]
    fn tiles_sit_after_a_separator() {
        let flat = layout(5, 0, SCREEN.into(), 84, 1.0, 0.0, None);
        let l = layout(5, 2, SCREEN.into(), 84, 1.0, 0.0, None);
        // A third of a step (88 / 3 = 29 px) between the third icon and the first tile, the dock still centred.
        assert_eq!(l.zone.size.w, flat.zone.size.w + 29);
        assert_eq!(l.icons[3].loc.x - l.icons[2].loc.x, 88.0 + 29.0);
        let sep = l.separator.unwrap();
        assert!(l.columns[2].1 <= sep && sep <= l.columns[3].0);
        assert_eq!(hit(&l, (sep, 1150.0).into()), None, "the gap is no icon");
        assert_eq!(flat.separator, None);
        assert_eq!(layout(2, 2, SCREEN.into(), 84, 1.0, 0.0, None).separator, None, "only tiles: no line");
        // Tiles zoom like icons.
        let p = (l.icons[4].loc.x + 42.0, 1150.0).into();
        let z = layout(5, 2, SCREEN.into(), 84, 1.5, 1.0, Some(p));
        assert_eq!((z.icons[4].size.w, hit(&z, p)), (126.0, Some(4)));
    }

    #[test]
    fn hits_follow_columns_inside_the_zone() {
        let l = layout(3, 0, SCREEN.into(), 84, 1.0, 0.0, None);
        let x = |i: usize| (l.columns[i].0 + l.columns[i].1) / 2.0;
        assert_eq!(hit(&l, (x(0), 1150.0).into()), Some(0));
        assert_eq!(hit(&l, (x(2), 1199.0).into()), Some(2));
        assert_eq!(hit(&l, (x(1), 1000.0).into()), None, "above the dock");
        assert_eq!(hit(&l, (10.0, 1150.0).into()), None, "beside it");
        // The pointer over an icon's centre hits it, zoomed.
        for i in 0..3 {
            let p = (x(i), 1150.0).into();
            let z = layout(3, 0, SCREEN.into(), 84, 1.5, 1.0, Some(p));
            assert_eq!(hit(&z, p), Some(i));
            assert!(z.icons.iter().all(|r| r.size.w <= z.icons[i].size.w));
        }
    }

    #[test]
    fn previews_fit_over_the_icon_and_hit_their_parts() {
        let sizes = [Size::from((1600, 1000)), Size::from((800, 1000))];
        let peek = peek_layout(&sizes, (960, 1000).into(), 1920);
        assert_eq!(peek.strip, Rectangle::new((798, 848).into(), (324, 144).into()));
        assert_eq!(peek.thumbs, vec![Rectangle::new((810, 860).into(), (192, 120).into()), Rectangle::new((1014, 860).into(), (96, 120).into())]);
        let at = |x, y| Point::from((f64::from(x), f64::from(y)));
        assert_eq!(peek_hit(&peek, None, at(900, 900)), Some(PeekHit::Thumb(0)));
        assert_eq!(peek_hit(&peek, None, at(1010, 900)), Some(PeekHit::Strip), "between thumbnails");
        assert_eq!(peek_hit(&peek, None, at(900, 995)), Some(PeekHit::Strip), "the gap down to the icon");
        assert_eq!(peek_hit(&peek, None, at(900, 1001)), None);
        assert_eq!(peek_hit(&peek, None, at(806, 856)), Some(PeekHit::Strip), "no ✕ unless hovered");
        assert_eq!(peek_hit(&peek, Some(0), at(806, 856)), Some(PeekHit::Close(0)));
        assert_eq!(peek_hit(&peek, Some(0), at(1020, 870)), Some(PeekHit::Thumb(1)));
        // Kept on screen at the edge; many windows shrink to fit.
        assert_eq!(peek_layout(&sizes, (10, 1000).into(), 1920).strip.loc.x, PEEK_PAD);
        let many = peek_layout(&[Size::from((1600, 1000)); 20], (960, 1000).into(), 1920);
        assert!(many.strip.loc.x >= PEEK_PAD && many.strip.loc.x + many.strip.size.w <= 1920 - PEEK_PAD, "{:?}", many.strip);
        assert!(many.thumbs.windows(2).all(|t| t[0].loc.x + t[0].size.w < t[1].loc.x), "no overlap");
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
    fn cycle_target_stays_in_or_crosses_apps() {
        let groups = vec![vec![1, 2], vec![3], vec![4, 5]];
        // Within the app: round.
        assert_eq!(cycle_target(&groups, 0, Some(&2), true, false), Some(1));
        assert_eq!(cycle_target(&groups, 0, Some(&1), false, false), Some(2));
        // Another app focused: this app's first window, across or not.
        assert_eq!(cycle_target(&groups, 2, Some(&1), true, true), Some(4));
        assert_eq!(cycle_target(&groups, 2, None, false, false), Some(4));
        // Across: past the app's last window into the next app, and round the dock.
        assert_eq!(cycle_target(&groups, 0, Some(&2), true, true), Some(3));
        assert_eq!(cycle_target(&groups, 0, Some(&1), false, true), Some(5));
        assert_eq!(cycle_target(&groups, 2, Some(&5), true, true), Some(1));
        assert_eq!(cycle_target(&groups, 1, Some(&3), true, true), Some(4));
        assert_eq!(cycle_target::<i32>(&[], 0, None, true, true), None);
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
    fn menu_is_macs() {
        let labels = |rows: Vec<Option<Row>>| rows.into_iter().map(|r| r.map(|r| (r.label, r.shortcut, r.action.is_some()))).collect::<Vec<_>>();
        let s = |l: &str, tick: &'static str, on: bool| Some((l.to_owned(), tick, on));
        let two = ["a.txt".to_owned(), "◆ b.txt".to_owned()];
        let actions = ["New Window".to_owned()];
        let recent = ["notes.md".to_owned()];
        assert_eq!(
            labels(menu_rows(
                &two,
                Some(0),
                &recent,
                &actions,
                &Options { pinned: Some(true), login: Some(false), assign: Some(2), ..Options::default() },
                false
            )),
            [
                s("a.txt", "", false),
                s("◆ b.txt", "", true),
                None,
                s("notes.md", "", true),
                None,
                s("New Window", "", true),
                None,
                s("Options", "", false),
                None,
                s("Close All", "", true)
            ]
        );
        let rows = menu_rows(&two, None, &recent, &actions, &Options { pinned: Some(true), login: Some(false), assign: Some(2), ..Options::default() }, false);
        assert_eq!(
            labels(rows[7].clone().unwrap().sub),
            [s("Keep in Dock", "✓", true), s("Open at Login", "", true), s("Show in Files", "", true), None, s("Assign To", "", false)]
        );
        // Assign To is a submenu of its own, its choice ticked.
        let assign = rows[7].clone().unwrap().sub[4].clone().unwrap();
        assert_eq!(labels(assign.sub), [s("All Spaces", "", true), s("This Space", "", true), s("None", "✓", true)]);
        // Not running; locked items, no desktop file, a builtin: no Options at all.
        let options = |rows: Vec<Option<Row>>| rows.into_iter().flatten().find(|r| r.label == "Options").map(|r| labels(r.sub));
        assert_eq!(menu_rows(&[], None, &[], &[], &Options::default(), false), []);
        assert_eq!(
            options(menu_rows(&[], None, &[], &[], &Options { pinned: Some(true), login: Some(true), ..Options::default() }, false)),
            Some(vec![s("Keep in Dock", "✓", true), s("Open at Login", "✓", true), s("Show in Files", "", true)])
        );
        let all = options(menu_rows(&[], None, &[], &[], &Options { assign: Some(0), ..Options::default() }, false)).unwrap();
        assert_eq!(all, [s("Assign To", "", false)], "no separator at the top");
        assert_eq!(labels(menu_rows(&two[..1], None, &[], &[], &Options::default(), false)), [s("a.txt", "", true), None, s("Close", "", true)]);
        // Close Others while other apps have windows, even for an app with none.
        assert_eq!(labels(menu_rows(&two[..1], None, &[], &[], &Options::default(), true))[2..], [s("Close", "", true), s("Close Others", "", true)]);
        assert_eq!(labels(menu_rows(&[], None, &[], &[], &Options::default(), true)), [s("Close Others", "", true)]);
        // Move to Display only with more than one display; not to the one the app is on.
        let one = Options { displays: vec![("eDP-1".into(), false)], ..Options::default() };
        assert_eq!(menu_rows(&two[..1], None, &[], &[], &one, false).len(), 3, "one display: no Options");
        let two_displays = Options { assign: Some(2), displays: vec![("eDP-1".into(), false), ("DP-1".into(), true)], ..Options::default() };
        let sub = options(menu_rows(&two[..1], None, &[], &[], &two_displays, false)).unwrap();
        assert_eq!(sub, [s("Assign To", "", false), s("Move to Display", "", false)]);
        let rows = menu_rows(&two[..1], None, &[], &[], &two_displays, false);
        let to = &crate::shell::sub_rows(&rows, &[2, 1]);
        assert_eq!(labels(to.to_vec()), [s("eDP-1", "", false), s("DP-1", "", true)]);
        assert_eq!(to[1].as_ref().and_then(|r| r.action.clone()), Some(Action::Dock(Command::ToDisplay(1))));
        let rows = menu_rows(&two, None, &recent, &actions, &Options { pinned: Some(true), login: Some(false), assign: Some(2), ..Options::default() }, false);
        assert_eq!(rows[1].as_ref().and_then(|r| r.action.clone()), Some(Action::Dock(Command::Window(1))));
        assert_eq!(rows[3].as_ref().and_then(|r| r.action.clone()), Some(Action::Dock(Command::Recent(0))));
        assert_eq!(rows[5].as_ref().and_then(|r| r.action.clone()), Some(Action::Dock(Command::Action(0))));
    }

    #[test]
    fn spring_fires_once_after_resting_on_an_icon() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut spring = Spring::default();
        assert!(spring.over(Some("kate"), t0), "arriving arms it");
        assert!(!spring.over(Some("kate"), ms(300)), "moving on the same icon doesn't");
        assert_eq!(spring.due(ms(599)), None);
        assert_eq!(spring.due(ms(600)).as_deref(), Some("kate"));
        assert_eq!(spring.due(ms(900)), None, "once per visit");
        assert!(spring.over(Some("kitty"), ms(1000)));
        assert!(!spring.over(None, ms(1300)), "leaving arms nothing");
        assert_eq!(spring.due(ms(1700)), None, "left before the delay");
        spring.over(Some("kate"), ms(2000));
        assert_eq!(spring.due(ms(2600)).as_deref(), Some("kate"), "coming back springs again");
    }

    #[test]
    fn assign_saves_as_one_line() {
        let mut map = std::collections::BTreeMap::new();
        assert_eq!(inline_table(&map), "{}");
        map.insert("org.kde.kate".to_owned(), "all".to_owned());
        map.insert("kitty".to_owned(), "DP-1:2".to_owned());
        let line = inline_table(&map);
        assert_eq!(line, r#"{ "kitty" = "DP-1:2", "org.kde.kate" = "all" }"#);
        let back: toml::Table = toml::from_str(&format!("assign = {line}")).unwrap();
        assert_eq!(back["assign"]["org.kde.kate"].as_str(), Some("all"));
        // A hand-written `[dock.assign]` table is replaced, not duplicated.
        let text = "[dock]\nsize = 48\n\n[dock.assign]\nkitty = \"all\"\n\n[clock]\nhour24 = true\n";
        let saved = config::set_key(&config::set_section(text, "dock.assign", ""), "dock", "assign", &line);
        let saved: toml::Table = toml::from_str(&saved).unwrap();
        assert_eq!(saved["dock"]["assign"]["kitty"].as_str(), Some("DP-1:2"));
        assert_eq!(saved["clock"]["hour24"].as_bool(), Some(true));
    }

    #[test]
    fn separating_copies_the_shared_list_and_back() {
        let mut dock = config::parse(config::DEFAULT_TOML).unwrap().dock;
        dock.outputs.insert("DP-1".into(), vec!["kitty".into()]);
        let edit = separate(&mut dock, "HDMI-A-1");
        assert_eq!(dock.outputs["HDMI-A-1"], dock.items, "on: a copy of the shared list");
        let saved = edit(config::DEFAULT_TOML);
        let back = config::parse(&saved).unwrap().dock;
        assert_eq!(back.outputs, dock.outputs);
        // Off again: only that display's list goes; another display's (connected or not) stays.
        let edit = separate(&mut dock, "HDMI-A-1");
        let back = config::parse(&edit(&saved)).unwrap().dock;
        assert_eq!(back.outputs.keys().collect::<Vec<_>>(), ["DP-1"]);
        assert_eq!(back.items, config::Dock::default().items);
        let rows = menu_rows(&[], None, &[], &[], &Options { pinned: Some(true), separate: Some(true), ..Options::default() }, false);
        let sub = &rows[0].as_ref().unwrap().sub;
        assert_eq!((sub[1].as_ref().unwrap().label.as_str(), sub[1].as_ref().unwrap().shortcut), ("Separate Items on This Display", "✓"));
        assert_eq!(sub[1].as_ref().unwrap().action, Some(Action::Dock(Command::Separate)));
    }

    #[test]
    fn pins_go_to_the_displays_own_list() {
        let v = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let mut dock = config::parse(config::DEFAULT_TOML).unwrap().dock;
        dock.outputs.insert("HDMI-A-1".into(), v(&["kitty"]));
        let shared = dock.items.clone();
        // A display with its own list: only that list changes, saved as one `outputs` line.
        let edit = store_pins(&mut dock, Some("HDMI-A-1"), v(&["kitty", "kate"]));
        assert_eq!(dock.items, shared);
        let saved = config::parse(&edit(config::DEFAULT_TOML)).unwrap().dock;
        assert_eq!(saved.items_on(Some("HDMI-A-1")), ["kitty", "kate"]);
        assert_eq!(saved.items, shared);
        // Any other display (or none known): the shared list.
        for display in [Some("eDP-1"), None] {
            let edit = store_pins(&mut dock, display, v(&["dolphin"]));
            let saved = config::parse(&edit(config::DEFAULT_TOML)).unwrap().dock;
            assert_eq!((saved.items.as_slice(), dock.items.as_slice()), (&v(&["dolphin"])[..], &v(&["dolphin"])[..]));
            assert_eq!(dock.outputs["HDMI-A-1"], ["kitty", "kate"]);
        }
        // A hand-written `[dock.outputs]` table is replaced, not duplicated; quotes survive.
        let text = "[dock]\nzoom = true\n\n[dock.outputs]\n\"DP-1\" = [\"a\"]\n\n[clock]\nhour24 = true\n";
        let lists = BTreeMap::from([("DP-1".to_owned(), v(&["say \"hi\""])), ("HDMI-A-1".to_owned(), Vec::new())]);
        let saved = set_outputs(text, &lists);
        assert_eq!(saved.matches("outputs").count(), 1, "{saved}");
        let back = config::parse(&saved).unwrap();
        assert_eq!(back.dock.outputs, lists);
        assert_eq!(saved.parse::<toml::Table>().unwrap()["clock"]["hour24"].as_bool(), Some(true));
    }

    #[test]
    fn recent_files_of_an_app() {
        let xbel = r#"<?xml version="1.0" encoding="UTF-8"?>
<xbel version="1.0" xmlns:bookmark="http://www.freedesktop.org/standards/desktop-bookmarks">
  <bookmark href="file:///home/u/old.txt" added="2026-01-01T00:00:00Z" modified="2026-01-01T00:00:00Z">
    <info><metadata owner="http://freedesktop.org"><bookmark:applications>
      <bookmark:application name="Kate" exec="&apos;kate %u&apos;" modified="2026-01-02T00:00:00Z" count="1"/>
    </bookmark:applications></metadata></info>
  </bookmark>
  <bookmark href="file:///home/u/a%20b&amp;c.txt" added="2026-01-01T00:00:00Z" modified="2026-03-01T00:00:00Z">
    <info><metadata owner="http://freedesktop.org"><bookmark:applications>
      <bookmark:application name="gedit" exec="&apos;gedit %U&apos;" modified="2026-05-01T00:00:00Z" count="2"/>
      <bookmark:application name="org.kde.kate" exec="&apos;kate %u&apos;" modified="2026-02-01T00:00:00Z" count="1"/>
    </bookmark:applications></metadata></info>
  </bookmark>
  <bookmark href="file:///home/u/pic.png" modified="2026-09-01T00:00:00Z">
    <info><metadata owner="http://freedesktop.org"><bookmark:applications>
      <bookmark:application name="gwenview" exec="gwenview %u" modified="2026-09-01T00:00:00Z" count="1"/>
    </bookmark:applications></metadata></info>
  </bookmark>
</xbel>"#;
        // Ordered by when this app used them, not by another app's later use.
        assert_eq!(recent_files(xbel, &["org.kde.kate", "kate"]), ["file:///home/u/a%20b&c.txt", "file:///home/u/old.txt"]);
        assert_eq!(recent_files(xbel, &["GEDIT"]), ["file:///home/u/a%20b&c.txt"]);
        assert!(recent_files(xbel, &["dolphin"]).is_empty());
        assert!(recent_files("", &["kate"]).is_empty());

        let (apps, files) = recent_items(xbel);
        assert_eq!(apps, ["gwenview", "gedit", "org.kde.kate", "Kate"]);
        assert_eq!(files, ["file:///home/u/pic.png", "file:///home/u/a%20b&c.txt", "file:///home/u/old.txt"]);

        let empty = cleared(xbel).unwrap();
        assert!(empty.starts_with("<?xml") && empty.ends_with("</xbel>\n") && !empty.contains("<bookmark "));
        assert_eq!(recent_items(&empty), (Vec::new(), Vec::new()));
        assert_eq!(cleared("garbage"), None);
    }

    #[test]
    fn builtins_and_the_screenshot_app() {
        assert_eq!(Builtin::of("launchpad"), Some(Builtin::Launchpad));
        assert_eq!(Builtin::of("screenshot"), Some(Builtin::Screenshot));
        assert_eq!(Builtin::of("app-drawer"), Some(Builtin::AppDrawer));
        assert_eq!(Builtin::of("org.kde.spectacle"), Some(Builtin::Screenshot), "Spectacle needs its own compositor: it opens our toolbar");
        assert_eq!(Builtin::of("org.kde.kate"), None);
        let item = |key: &str| Item { key: key.into(), app: None, windows: Vec::new() };
        assert_eq!((item("screenshot").icon(), item("screenshot").title()), ("spectacle", "Screenshot"));
        assert_eq!(item("launchpad").icon(), launchpad::ICON);
        assert_eq!(item("some-app").title(), "some-app");
    }

    #[test]
    fn drag_slots_follow_columns() {
        let l = layout(4, 0, SCREEN.into(), 84, 1.0, 0.0, None);
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
        let drag = Drag { key: "a".into(), from: None, start: (0.0, 0.0).into(), since: Instant::now(), moved: true, slot: Some(2) };
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
        let tricky = "[dock]\nitems = [\n  \"a]\", # ] not yet\n  \"b\\\"[\",\n]\nzoom = true\n";
        assert_eq!(set_items(tricky, &names[..1]), "[dock]\nitems = [\"kate\"]\nzoom = true\n", "brackets in quotes or comments");
        assert_eq!(config::bracket_depth(r#"x = ["a]", 'b[', "\\"]"] # ]"#), 0);
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
    fn docks_go_on_every_display_the_main_one_or_the_pointers() {
        assert_eq!(dock_displays(DockDisplays::All, 3, Some(2)), [0, 1, 2]);
        assert_eq!(dock_displays(DockDisplays::Main, 3, Some(2)), [0]);
        assert_eq!(dock_displays(DockDisplays::Pointer, 3, Some(2)), [2]);
        assert_eq!(dock_displays(DockDisplays::Pointer, 3, None), [0], "nowhere: the main one");
        assert_eq!(dock_displays(DockDisplays::Pointer, 2, Some(5)), [0], "a display that went");
        assert!(dock_displays(DockDisplays::All, 0, None).is_empty() && dock_displays(DockDisplays::Pointer, 0, None).is_empty());
    }

    #[test]
    fn reserve_matches_the_zone() {
        assert_eq!(reserve(&Dock::default()), layout(8, 0, SCREEN.into(), 84, 1.0, 0.0, None).zone.size.h);
    }
}
