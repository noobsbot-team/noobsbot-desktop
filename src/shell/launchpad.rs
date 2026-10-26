//! Launchpad (E4.4): every app in a full-screen grid of pages over the dimmed wallpaper, a search field on top. Opens
//! from its dock icon or the `launchpad` binding (F4, off by default); Esc, a click on empty space or the binding
//! again closes it. Pages slide with a two-finger horizontal swipe, the wheel, ←/→ or a click on a page dot. Typing
//! filters; Return opens the first match. Animations run only while something moves.
//!
//! The order and folders are saved in `$XDG_STATE_HOME/noobsbot-desktop/launchpad.json`; apps it doesn't name go at
//! the end, by name.

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use smithay::{
    input::keyboard::Keysym,
    reexports::calloop::{
        RegistrationToken,
        timer::{TimeoutAction, Timer},
    },
    utils::{Logical, Point, Rectangle, Size},
};

use crate::{
    apps::App,
    state::State,
    text::ts,
    ui::field::{Edit, TextField},
    wm::Rect,
};

/// The dock item that opens Launchpad (`[dock] items`).
pub const DOCK_KEY: &str = "launchpad";
/// Its icon in the icon theme: WhiteSur draws the Launchpad rocket for Slingscold, a Launchpad clone.
pub const ICON: &str = "slingscold";

pub mod layout {
    /// D13's examples (5 × 4 at 1280 × 720, 7 × 5 at 1920 × 1080, 9 × 6 at 4K): a column takes 232 px past the 48 px
    /// side margins; the first row 164 px, each further one 184. (D13's own formula doesn't give its examples.)
    pub const SIDE: i32 = 48;
    pub const COLUMN: i32 = 232;
    pub const FIRST_ROW: i32 = 164;
    pub const ROW: i32 = 184;
    pub const COLS: (usize, usize) = (5, 9);
    pub const ROWS: (usize, usize) = (3, 6);
    /// 80 px icons, 64 under 1366 wide.
    pub const ICON: i32 = 80;
    pub const SMALL_ICON: i32 = 64;
    pub const NARROW: i32 = 1366;
    /// Icon to name, and the name line (Callout). `NAME_H` and the search pill grow with the text size (`text::ts`).
    pub const NAME_GAP: i32 = 8;
    pub const NAME_H: i32 = 15;
    /// The search pill, centred at the top.
    pub const FIELD_W: i32 = 240;
    pub const FIELD_H: i32 = 28;
    pub const FIELD_Y: i32 = 40;
    /// Page dots: 8 px, 12 apart, a 24 px band above the dock.
    pub const DOT: i32 = 8;
    pub const DOT_STEP: i32 = 20;
    pub const DOTS_H: i32 = 48;
}

/// Where everything is on one page, output-relative, for the page in view.
#[derive(Debug, Clone, PartialEq)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    pub icon: i32,
    /// Cells in reading order, `cols × rows` of them.
    pub cells: Vec<Rect>,
    pub field: Rect,
    /// The centre line of the page dots.
    pub dots_y: i32,
    pub width: i32,
}

impl Grid {
    pub fn per_page(&self) -> usize {
        self.cols * self.rows
    }

    /// The icon square in cell `i` (the name sits under it).
    pub fn icon_rect(&self, i: usize) -> Rect {
        let cell = self.cells[i];
        let block = self.icon + layout::NAME_GAP + ts(layout::NAME_H);
        let (x, y) = (cell.loc.x + (cell.size.w - self.icon) / 2, cell.loc.y + (cell.size.h - block) / 2);
        Rectangle::new((x, y).into(), (self.icon, self.icon).into())
    }

    pub fn name_rect(&self, i: usize) -> Rect {
        let (cell, icon) = (self.cells[i], self.icon_rect(i));
        Rectangle::new((cell.loc.x + 4, icon.loc.y + self.icon + layout::NAME_GAP).into(), (cell.size.w - 8, ts(layout::NAME_H)).into())
    }

    /// One dot per page, centred.
    pub fn dots(&self, pages: usize) -> Vec<Rect> {
        use layout::{DOT, DOT_STEP};
        let w = pages as i32 * DOT_STEP;
        let x0 = (self.width - w) / 2 + (DOT_STEP - DOT) / 2;
        (0..pages as i32).map(|i| Rectangle::new((x0 + i * DOT_STEP, self.dots_y - DOT / 2).into(), (DOT, DOT).into())).collect()
    }

    /// The cell `p` is in, icon or not.
    pub fn cell_at(&self, p: Point<f64, Logical>) -> Option<usize> {
        self.cells.iter().position(|c| c.to_f64().contains(p))
    }

    /// The cell whose icon or name is at `p`, on the page in view.
    pub fn hit(&self, p: Point<f64, Logical>) -> Option<usize> {
        (0..self.cells.len()).find(|&i| self.icon_rect(i).to_f64().contains(p) || self.name_rect(i).to_f64().contains(p))
    }
}

/// An open folder of `n` apps: its glass panel, centred over `grid`, and the grid inside (its `field` is the name).
pub fn folder_grid(grid: &Grid, n: usize) -> (Rect, Grid) {
    const PAD: i32 = 24;
    let cols = n.clamp(1, grid.cols.min(4));
    // ponytail: past the rows a page holds, the rest of a big folder is cut off; scroll it if folders get that big.
    let rows = n.div_ceil(cols).clamp(1, grid.rows.saturating_sub(1).max(1));
    let cell = grid.cells[0].size;
    let field_h = ts(layout::FIELD_H);
    let (w, h) = (cols as i32 * cell.w + 2 * PAD, PAD + field_h + 12 + rows as i32 * cell.h + PAD);
    let (top, bottom) = (grid.cells[0].loc.y, grid.cells[grid.cells.len() - 1].loc.y + cell.h);
    let panel = Rectangle::new(((grid.width - w) / 2, (top + bottom - h) / 2).into(), (w, h).into());
    let y0 = panel.loc.y + PAD + field_h + 12;
    let cells =
        (0..rows as i32).flat_map(|r| (0..cols as i32).map(move |c| Rectangle::new((panel.loc.x + PAD + c * cell.w, y0 + r * cell.h).into(), cell))).collect();
    let field = Rectangle::new((panel.loc.x + PAD, panel.loc.y + PAD).into(), (w - 2 * PAD, field_h).into());
    (panel, Grid { cols, rows, icon: grid.icon, cells, field, dots_y: 0, width: grid.width })
}

/// Which way a page edge at `p` turns: -1 at the left side margin, 1 at the right.
fn edge_of(grid: &Grid, p: Point<f64, Logical>) -> Option<isize> {
    let side = f64::from(layout::SIDE);
    if p.x < side {
        Some(-1)
    } else if p.x >= f64::from(grid.width) - side {
        Some(1)
    } else {
        None
    }
}

/// The grid for an `output`-sized screen with `bottom` px kept for the dock.
pub fn grid(output: Size<i32, Logical>, bottom: i32) -> Grid {
    use layout::*;
    let cols = (((output.w - 2 * SIDE) / COLUMN).max(0) as usize).clamp(COLS.0, COLS.1);
    let rows = (((output.h - FIRST_ROW) / ROW + 1).max(0) as usize).clamp(ROWS.0, ROWS.1);
    let icon = if output.w < NARROW { SMALL_ICON } else { ICON };
    let (field_w, field_h, name_h) = (ts(FIELD_W), ts(FIELD_H), ts(NAME_H));
    let top = FIELD_Y + field_h + 24;
    let bottom = output.h - bottom - DOTS_H;
    // Cells no wider or taller than the steps above, so a big screen keeps the grid together, centred.
    let cell_w = ((output.w - 2 * SIDE) / cols as i32).clamp(1, COLUMN);
    let cell_h = ((bottom - top) / rows as i32).min(ROW).max(icon + NAME_GAP + name_h);
    let (x0, y0) = ((output.w - cell_w * cols as i32) / 2, top + ((bottom - top) - cell_h * rows as i32).max(0) / 2);
    let cells = (0..rows as i32)
        .flat_map(|r| (0..cols as i32).map(move |c| Rectangle::new((x0 + c * cell_w, y0 + r * cell_h).into(), (cell_w, cell_h).into())))
        .collect();
    let field = Rectangle::new(((output.w - field_w) / 2, FIELD_Y).into(), (field_w, field_h).into());
    Grid { cols, rows, icon, cells, field, dots_y: bottom + DOTS_H / 2, width: output.w }
}

/// How many pages `n` apps fill; at least one.
pub fn pages(n: usize, per_page: usize) -> usize {
    n.div_ceil(per_page.max(1)).max(1)
}

/// The launchable apps matching `query`: all of them (by name) when it's empty, else those whose name contains it
/// (any case), names that start with it first.
pub fn filter<'a>(apps: &'a [App], query: &str) -> Vec<&'a App> {
    let query = query.trim().to_lowercase();
    let mut out: Vec<&App> = apps.iter().filter(|a| !a.no_display && a.name.to_lowercase().contains(&query)).collect();
    if !query.is_empty() {
        // Stable: alphabetical within each group.
        out.sort_by_key(|a| !a.name.to_lowercase().starts_with(&query));
    }
    out
}

/// One place in the grid: an app's desktop-file id, or a folder of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Entry {
    App(String),
    Folder { name: String, apps: Vec<String> },
}

/// What a place shows.
pub enum Tile<'a> {
    App(&'a App),
    Folder(String, Vec<&'a App>),
}

/// `order` against the installed `apps`: gone apps dropped, folders left with one app dissolved, apps it doesn't
/// name appended in `apps`' order (by name).
pub fn arrange(order: &[Entry], apps: &[App]) -> Vec<Entry> {
    let known: HashSet<&str> = apps.iter().filter(|a| !a.no_display).map(|a| a.id.as_str()).collect();
    let mut seen = HashSet::new();
    let mut keep = |id: &String| known.contains(id.as_str()) && seen.insert(id.clone());
    let mut out: Vec<Entry> = order
        .iter()
        .filter_map(|e| match e {
            Entry::App(id) => keep(id).then(|| e.clone()),
            Entry::Folder { name, apps } => tidy(Entry::Folder { name: name.clone(), apps: apps.iter().filter(|id| keep(id)).cloned().collect() }),
        })
        .collect();
    out.extend(apps.iter().filter(|a| !a.no_display && !seen.contains(&a.id)).map(|a| Entry::App(a.id.clone())));
    out
}

/// What an entry's slide is remembered by: its app, or a folder's first app.
fn key(entry: &Entry) -> &str {
    match entry {
        Entry::App(id) => id,
        Entry::Folder { apps, .. } => apps.first().map_or("", String::as_str),
    }
}

/// A folder with one app is that app; with none, nothing.
fn tidy(entry: Entry) -> Option<Entry> {
    match entry {
        Entry::Folder { mut apps, .. } if apps.len() < 2 => apps.pop().map(Entry::App),
        e => Some(e),
    }
}

/// What the grid shows: the saved `order` while `query` is empty, else every app matching it (folders' too).
pub fn tiles<'a>(apps: &'a [App], order: &[Entry], query: &str) -> Vec<Tile<'a>> {
    if !query.trim().is_empty() {
        return filter(apps, query).into_iter().map(Tile::App).collect();
    }
    let by_id: HashMap<&str, &App> = apps.iter().map(|a| (a.id.as_str(), a)).collect();
    arrange(order, apps)
        .into_iter()
        .filter_map(|e| match e {
            Entry::App(id) => by_id.get(id.as_str()).copied().map(Tile::App),
            Entry::Folder { name, apps } => Some(Tile::Folder(name, apps.iter().filter_map(|id| by_id.get(id.as_str()).copied()).collect())),
        })
        .collect()
}

/// `v` with item `from` moved to `to` (at most the end).
pub fn move_to<T>(v: &mut Vec<T>, from: usize, to: usize) {
    if from < v.len() {
        let item = v.remove(from);
        v.insert(to.min(v.len()), item);
    }
}

/// freedesktop main categories and the folder names they give, most specific first.
const CATEGORIES: [(&str, &str); 13] = [
    ("Game", "Games"),
    ("Development", "Development"),
    ("Graphics", "Graphics"),
    ("Office", "Office"),
    ("Education", "Education"),
    ("Science", "Science"),
    ("Audio", "Audio"),
    ("Video", "Video"),
    ("AudioVideo", "Media"),
    ("Network", "Internet"),
    ("Settings", "Settings"),
    ("System", "System"),
    ("Utility", "Utilities"),
];

/// A new folder's name: the main category all its apps share, else "Folder".
pub fn folder_name(ids: &[String], apps: &[App]) -> String {
    let has = |id: &String, cat: &str| apps.iter().find(|a| a.id == *id).is_some_and(|a| a.categories.iter().any(|c| c == cat));
    CATEGORIES.iter().find(|(cat, _)| ids.iter().all(|id| has(id, cat))).map_or("Folder", |(_, name)| name).to_owned()
}

/// Drops entry `from` onto `onto`: two apps make a folder, an app joins a folder; a folder goes into nothing.
/// `false`: nothing changed.
pub fn merge(order: &mut Vec<Entry>, from: usize, onto: usize, apps: &[App]) -> bool {
    if from == onto || onto >= order.len() {
        return false;
    }
    let Some(Entry::App(id)) = order.get(from).cloned() else { return false };
    order.remove(from);
    let target = &mut order[if from < onto { onto - 1 } else { onto }];
    *target = match std::mem::replace(target, Entry::App(String::new())) {
        Entry::Folder { name, apps: mut ids } => {
            ids.push(id);
            Entry::Folder { name, apps: ids }
        }
        Entry::App(other) => {
            let ids = vec![other, id];
            Entry::Folder { name: folder_name(&ids, apps), apps: ids }
        }
    };
    true
}

/// Takes app `j` out of the folder at `folder`, to just after it (a folder left with one app dissolves). Its new place.
pub fn take_out(order: &mut Vec<Entry>, folder: usize, j: usize) -> Option<usize> {
    let Some(Entry::Folder { apps, .. }) = order.get_mut(folder) else { return None };
    let id = (j < apps.len()).then(|| apps.remove(j))?;
    if let Some(left) = tidy(order[folder].clone()) {
        order[folder] = left;
    }
    order.insert(folder + 1, Entry::App(id));
    Some(folder + 1)
}

fn order_file() -> std::path::PathBuf {
    super::drawer::state_file("launchpad.json")
}

/// The saved order; empty when there is none (or it doesn't parse).
fn load_order() -> Vec<Entry> {
    std::fs::read(order_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_order(order: &[Entry]) {
    let text = serde_json::to_string_pretty(order).unwrap_or_default();
    crate::config::save_later(order_file(), move |_| text);
}

/// Movement that lifts a pressed icon.
const DRAG_START: f64 = 8.0;
/// A press held this long lifts the icon too, and clicks nothing.
const HOLD: Duration = Duration::from_millis(500);

/// A lifted icon held this long at a side edge turns the page (and again while it stays).
const EDGE_PAUSE: Duration = Duration::from_millis(600);

/// A press on a tile, until the button comes up.
pub struct Press {
    /// The tile's place in `tiles` (and in the order, which is arranged first while not searching).
    pub index: usize,
    pub start: Point<f64, Logical>,
    pub at: Point<f64, Logical>,
    /// From the icon's corner to the press.
    pub grab: Point<f64, Logical>,
    pub since: Instant,
    /// Dragged past `DRAG_START`.
    pub moved: bool,
    /// Held over this tile's icon: dropping makes a folder (`merge`).
    pub onto: Option<usize>,
    /// In the open folder: the app's place in it (`index` is the folder's).
    pub inner: Option<usize>,
}

impl Press {
    pub fn lifted(&self, now: Instant) -> bool {
        self.moved || now.saturating_duration_since(self.since) >= HOLD
    }
}

/// D10 `smooth`: a critically damped spring with a 0.4 s response.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spring {
    from: f64,
    to: f64,
    start: Instant,
}

/// ω = 2π / response.
const OMEGA: f64 = std::f64::consts::TAU / 0.4;

impl Spring {
    pub fn new(from: f64, to: f64, now: Instant) -> Self {
        Self { from, to, start: now }
    }

    fn remaining(&self, now: Instant) -> f64 {
        let t = now.saturating_duration_since(self.start).as_secs_f64() * OMEGA;
        (1.0 + t) * (-t).exp()
    }

    pub fn at(&self, now: Instant) -> f64 {
        if self.done(now) { self.to } else { self.to + (self.from - self.to) * self.remaining(now) }
    }

    /// Within 0.1% of the way: settled (about 0.75 s).
    pub fn done(&self, now: Instant) -> bool {
        self.remaining(now) < 1e-3
    }

    /// Heads for `to` from wherever it is now.
    pub fn go(&mut self, to: f64, now: Instant) {
        *self = Self::new(self.at(now), to, now);
    }
}

/// A finger swipe that ends past this share of a page turns it.
const TURN: f64 = 0.12;
/// No scroll event for this long ends a swipe (for devices that send no scroll stop).
const SWIPE_END: Duration = Duration::from_millis(150);

pub struct Launchpad {
    pub field: TextField,
    /// The saved order (see `arrange`).
    pub order: Vec<Entry>,
    pub page: usize,
    /// 0 closed → 1 open: icons zoom from 1.1 and fade in.
    pub shown: Spring,
    /// The page position the slide is at.
    pub slide: Spring,
    /// A finger swipe in progress, in pages (positive: towards the next page).
    pub drag: f64,
    /// Ends the swipe after `SWIPE_END` without events.
    swipe_end: Option<RegistrationToken>,
    pub press: Option<Press>,
    /// Icons sliding aside for a dragged one: from this far off their place, springing back to it.
    moves: HashMap<String, (Point<f64, Logical>, Spring)>,
    /// The edge timer is running (`launchpad_edge`).
    edge: bool,
    /// The open folder: its place in the order, and its name being edited.
    pub folder: Option<(usize, TextField)>,
}

impl Launchpad {
    fn new(now: Instant) -> Self {
        Self {
            field: TextField::default(),
            order: load_order(),
            page: 0,
            shown: Spring::new(0.0, 1.0, now),
            slide: Spring::new(0.0, 0.0, now),
            drag: 0.0,
            swipe_end: None,
            press: None,
            moves: HashMap::new(),
            edge: false,
            folder: None,
        }
    }

    /// Where tile `i`'s icon sits, on a strip of pages laid side by side.
    fn place(grid: &Grid, i: usize) -> Point<f64, Logical> {
        let per_page = grid.per_page().max(1);
        (grid.icon_rect(i % per_page).loc + Point::from(((i / per_page) as i32 * grid.width, 0))).to_f64()
    }

    /// How far each tile is drawn off its place (indexes as `tiles`).
    pub fn offsets(&self, now: Instant) -> Vec<Point<i32, Logical>> {
        if self.moves.is_empty() || self.searching() {
            return Vec::new();
        }
        self.order.iter().map(|e| self.moves.get(key(e)).map_or_else(Point::default, |(off, s)| off.upscale(s.at(now)).to_i32_round())).collect()
    }

    /// Moves the tile at `from` to `to`; those between slide over from where they are drawn.
    fn reorder(&mut self, grid: &Grid, from: usize, to: usize, now: Instant) {
        let (lo, hi) = (from.min(to), from.max(to).min(self.order.len().saturating_sub(1)));
        let drawn: Vec<Point<f64, Logical>> = (lo..=hi)
            .map(|i| Self::place(grid, i) + self.moves.get(key(&self.order[i])).map_or_else(Point::default, |(off, s)| off.upscale(s.at(now))))
            .collect();
        let keys: Vec<String> = self.order[lo..=hi].iter().map(|e| key(e).to_owned()).collect();
        move_to(&mut self.order, from, to);
        for (k, was) in keys.into_iter().zip(drawn) {
            if let Some(i) = self.order.iter().position(|e| key(e) == k).filter(|&i| i != to) {
                self.moves.insert(k, (was - Self::place(grid, i), Spring::new(1.0, 0.0, now)));
            }
        }
    }

    pub fn searching(&self) -> bool {
        !self.field.text.trim().is_empty()
    }

    /// The tile being dragged (and in an open folder, the app in it), and where its icon is drawn (output-relative).
    pub fn lifted(&self, grid: &Grid, now: Instant) -> Option<(usize, Option<usize>, Rect)> {
        let p = self.press.as_ref().filter(|p| p.lifted(now) && !self.searching())?;
        Some((p.index, p.inner, Rectangle::new((p.at - p.grab).to_i32_round(), (grid.icon, grid.icon).into())))
    }

    pub fn closing(&self) -> bool {
        self.shown.to == 0.0
    }

    /// The page position in view.
    pub fn position(&self, now: Instant) -> f64 {
        self.slide.at(now) + self.drag
    }

    pub fn animating(&self, now: Instant) -> bool {
        !self.shown.done(now) || !self.slide.done(now) || self.moves.values().any(|(_, s)| !s.done(now))
    }

    /// Slides to `page`, kept within `pages`.
    fn turn_to(&mut self, page: usize, pages: usize, now: Instant) {
        let page = page.min(pages.saturating_sub(1));
        self.slide = Spring::new(self.position(now), page as f64, now);
        (self.page, self.drag) = (page, 0.0);
    }
}

impl State {
    /// What the grid shows.
    pub fn launchpad_tiles(&self) -> Vec<Tile<'_>> {
        self.launchpad.as_ref().map_or_else(Vec::new, |l| tiles(self.apps.list(), &l.order, &l.field.text))
    }

    /// The open folder's panel and grid.
    pub fn launchpad_folder(&self) -> Option<(Rect, Grid)> {
        let i = self.launchpad.as_ref()?.folder.as_ref()?.0;
        let n = match self.launchpad_tiles().into_iter().nth(i)? {
            Tile::Folder(_, apps) => apps.len(),
            Tile::App(_) => return None,
        };
        Some(folder_grid(&self.launchpad_grid()?, n))
    }

    pub fn launchpad_grid(&self) -> Option<Grid> {
        let size = self.space.outputs().next().and_then(|o| self.space.output_geometry(o))?.size;
        Some(grid(size, super::dock::reserve(&self.config.dock)))
    }

    /// Open and not on its way out: it takes the keyboard and the pointer.
    pub fn launchpad_open(&self) -> bool {
        self.launchpad.as_ref().is_some_and(|l| !l.closing())
    }

    pub fn open_launchpad(&mut self) {
        self.popup = None;
        let now = Instant::now();
        match self.launchpad.as_mut() {
            // Closing: turn back.
            Some(l) => l.shown.go(1.0, now),
            None => self.launchpad = Some(Launchpad::new(now)),
        }
        self.schedule_redraw();
    }

    /// Fades out; the last frame of the fade drops it (see `launchpad_frame`).
    pub fn close_launchpad(&mut self) -> bool {
        let Some(l) = self.launchpad.as_mut().filter(|l| !l.closing()) else { return false };
        l.shown.go(0.0, Instant::now());
        l.press = None;
        if let Some(token) = l.swipe_end.take() {
            self.loop_handle.remove(token);
        }
        self.schedule_redraw();
        true
    }

    pub fn toggle_launchpad(&mut self) {
        if !self.close_launchpad() {
            self.open_launchpad();
        }
    }

    /// Called once per frame: drops a finished close, and asks for the next frame while something moves.
    pub fn launchpad_frame(&mut self, now: Instant) {
        if let Some(l) = self.launchpad.as_mut() {
            l.moves.retain(|_, (_, s)| !s.done(now));
        }
        let Some(l) = self.launchpad.as_ref() else { return };
        if l.closing() && l.shown.done(now) {
            self.launchpad = None;
        } else if l.animating(now) {
            self.schedule_redraw();
        }
    }

    fn launchpad_pages(&self) -> usize {
        let n = self.launchpad_tiles().len();
        self.launchpad_grid().map_or(1, |g| pages(n, g.per_page()))
    }

    fn launchpad_turn(&mut self, page: usize) {
        let pages = self.launchpad_pages();
        if let Some(l) = self.launchpad.as_mut() {
            l.turn_to(page, pages, Instant::now());
            self.schedule_redraw();
        }
    }

    /// Starts `app` and closes Launchpad.
    fn launchpad_launch(&mut self, app: Option<App>) {
        if let Some(app) = app {
            tracing::info!(app = app.id, "launchpad: launch");
            if self.launch(Some(&app)) {
                self.remember_launch(&app.id);
            }
        }
        self.close_launchpad();
    }

    /// A key while open (all keys but desktop shortcuts come here): typing searches, ←/→ turn pages while the field is
    /// empty, Return opens the first match, Esc clears the search, then closes.
    pub fn launchpad_key(&mut self, sym: Keysym, ctrl: bool) {
        let Some(l) = self.launchpad.as_mut() else { return };
        // An open folder: typing renames it; Return or Esc closes it.
        if let Some((i, name)) = l.folder.as_mut() {
            let typed = sym.key_char().map(String::from);
            match name.key(sym, typed.as_deref(), ctrl) {
                Edit::Submit | Edit::Cancel => l.folder = None,
                Edit::Changed if !name.text.trim().is_empty() => {
                    if let Some(Entry::Folder { name: saved, .. }) = l.order.get_mut(*i) {
                        *saved = name.text.trim().to_owned();
                        save_order(&l.order);
                    }
                }
                _ => {}
            }
            return self.schedule_redraw();
        }
        let empty = l.field.text.is_empty();
        let page = l.page;
        match sym {
            Keysym::Left | Keysym::KP_Left if empty => return self.launchpad_turn(page.saturating_sub(1)),
            Keysym::Right | Keysym::KP_Right if empty => return self.launchpad_turn(page + 1),
            Keysym::Escape if !empty => l.field = TextField::default(),
            Keysym::Escape => {
                self.close_launchpad();
                return;
            }
            _ => {}
        }
        let typed = sym.key_char().map(String::from);
        match l.field.key(sym, typed.as_deref(), ctrl) {
            Edit::Submit => {
                let first = self.launchpad_tiles().into_iter().find_map(|t| match t {
                    Tile::App(a) => Some(a.clone()),
                    Tile::Folder(..) => None,
                });
                return self.launchpad_launch(first);
            }
            Edit::Changed => {
                // A new search starts on its first page.
                (l.page, l.drag) = (0, 0.0);
                l.slide = Spring::new(0.0, 0.0, Instant::now());
            }
            _ => {}
        }
        self.schedule_redraw();
    }

    /// A press at `location` (output-relative) while open: an icon opens its app, a dot turns to its page, the field
    /// keeps it open, anything else closes it. `true`: taken.
    pub fn launchpad_press(&mut self, location: Point<f64, Logical>, secondary: bool) -> bool {
        let Some(page) = self.launchpad.as_ref().filter(|l| !l.closing()).map(|l| l.page) else { return false };
        let Some(grid) = self.launchpad_grid() else { return false };
        if secondary || grid.field.to_f64().contains(location) {
            return true;
        }
        if let Some((panel, inner)) = self.launchpad_folder() {
            let Some(l) = self.launchpad.as_mut() else { return true };
            let index = l.folder.as_ref().map_or(0, |(i, _)| *i);
            match inner.hit(location).filter(|_| panel.to_f64().contains(location)) {
                Some(j) => {
                    let grab = location - inner.icon_rect(j).loc.to_f64();
                    l.press = Some(Press { index, start: location, at: location, grab, since: Instant::now(), moved: false, onto: None, inner: Some(j) });
                }
                // Outside the panel: closes the folder.
                None if !panel.to_f64().contains(location) => l.folder = None,
                None => {}
            }
            self.schedule_redraw();
            return true;
        }
        let tiles = self.launchpad_tiles();
        let pages = pages(tiles.len(), grid.per_page());
        if pages > 1
            // A dot is small: its hit area reaches 6 px around it.
            && let Some(dot) = grid.dots(pages).iter().position(|d| Rectangle::new(d.loc - Point::from((6, 6)), d.size + Size::from((12, 12))).to_f64().contains(location))
        {
            self.launchpad_turn(dot);
            return true;
        }
        let hit = grid.hit(location).filter(|i| page * grid.per_page() + i < tiles.len());
        drop(tiles);
        let Some(i) = hit else {
            self.close_launchpad();
            return true;
        };
        // It acts on release, unless it became a drag or a long press (`launchpad_release`).
        let apps = self.apps.list();
        let Some(l) = self.launchpad.as_mut() else { return true };
        if !l.searching() {
            l.order = arrange(&l.order, apps);
        }
        let grab = location - grid.icon_rect(i).loc.to_f64();
        l.press = Some(Press {
            index: page * grid.per_page() + i,
            start: location,
            at: location,
            grab,
            since: Instant::now(),
            moved: false,
            onto: None,
            inner: None,
        });
        // The icon lifts after `HOLD` without moving: draw it then.
        let lift = self.loop_handle.insert_source(Timer::from_duration(HOLD), |_, _, state| {
            state.schedule_redraw();
            TimeoutAction::Drop
        });
        lift.inspect_err(|err| tracing::warn!("launchpad hold timer: {err}")).ok();
        true
    }

    /// The pointer moved to `to` (output-relative): a lifted icon follows it, and takes the place under it.
    pub fn launchpad_motion(&mut self, to: Point<f64, Logical>) {
        let Some(grid) = self.launchpad_grid() else { return };
        let folder = self.launchpad_folder();
        let Some(l) = self.launchpad.as_mut() else { return };
        let searching = l.searching();
        let Some(p) = l.press.as_mut() else { return };
        p.at = to;
        p.moved |= (to.x - p.start.x).hypot(to.y - p.start.y) >= DRAG_START;
        if !p.moved || searching {
            return;
        }
        // In an open folder: a new place in it, or dragged off the panel, out of it and on into the grid.
        if let (Some(j), Some((panel, inner))) = (p.inner, folder) {
            let Some(Entry::Folder { apps, .. }) = l.order.get_mut(p.index) else { return };
            if panel.to_f64().contains(to) {
                if let Some(k) = inner.cell_at(to).map(|k| k.min(apps.len() - 1)).filter(|k| *k != j) {
                    move_to(apps, j, k);
                    p.inner = Some(k);
                }
                return self.schedule_redraw();
            }
            let Some(at) = take_out(&mut l.order, p.index, j) else { return };
            (p.index, p.inner, l.folder) = (at, None, None);
            // Its place keeps it on screen: the page it is on.
            let page = at / grid.per_page().max(1);
            let pages = pages(l.order.len(), grid.per_page());
            l.turn_to(page, pages, Instant::now());
            save_order(&l.order);
            return self.schedule_redraw();
        }
        p.onto = None;
        if let Some(i) = grid.cell_at(to) {
            let slot = (l.page * grid.per_page() + i).min(l.order.len().saturating_sub(1));
            let app = matches!(l.order.get(p.index), Some(Entry::App(_)));
            // Over another icon (an app only: folders don't nest): a folder on drop; between icons: a new place.
            if app && slot != p.index && grid.icon_rect(i).to_f64().contains(to) {
                p.onto = Some(slot);
            } else if slot != p.index {
                let from = std::mem::replace(&mut p.index, slot);
                l.reorder(&grid, from, slot, Instant::now());
            }
        }
        if edge_of(&grid, to).is_some() && !std::mem::replace(&mut l.edge, true) {
            let timer = self.loop_handle.insert_source(Timer::from_duration(EDGE_PAUSE), |_, _, state| state.launchpad_edge());
            timer.inspect_err(|err| tracing::warn!("launchpad edge timer: {err}")).ok();
        }
        self.schedule_redraw();
    }

    /// The edge timer: a lifted icon still at a side edge turns the page that way; elsewhere, the timer ends.
    fn launchpad_edge(&mut self) -> TimeoutAction {
        let (grid, pages) = (self.launchpad_grid(), self.launchpad_pages());
        let Some(l) = self.launchpad.as_mut() else { return TimeoutAction::Drop };
        let searching = l.searching();
        let way = l.press.as_ref().filter(|p| p.moved && !searching).zip(grid).and_then(|(p, g)| edge_of(&g, p.at));
        let Some(way) = way else {
            l.edge = false;
            return TimeoutAction::Drop;
        };
        let page = l.page.saturating_add_signed(way);
        l.turn_to(page, pages, Instant::now());
        self.schedule_redraw();
        TimeoutAction::ToDuration(EDGE_PAUSE)
    }

    /// The button came up: a drag keeps its new order; a short press without moving opens what it pressed.
    pub fn launchpad_release(&mut self) {
        let Some(l) = self.launchpad.as_mut() else { return };
        let Some(p) = l.press.take() else { return };
        if p.moved && !l.searching() {
            if let Some(onto) = p.onto {
                merge(&mut l.order, p.index, onto, self.apps.list());
            }
            save_order(&l.order);
        }
        let page = l.page;
        self.schedule_redraw();
        if p.moved {
            return;
        }
        if p.lifted(Instant::now()) {
            return;
        }
        let Some(grid) = self.launchpad_grid() else { return };
        let same = match (p.inner, self.launchpad_folder()) {
            (Some(j), Some((_, inner))) => inner.hit(p.at) == Some(j),
            _ => grid.hit(p.at).map(|i| page * grid.per_page() + i) == Some(p.index),
        };
        if !same {
            return;
        }
        let app = match (self.launchpad_tiles().into_iter().nth(p.index), p.inner) {
            (Some(Tile::App(a)), _) => a.clone(),
            (Some(Tile::Folder(_, apps)), Some(j)) => match apps.get(j) {
                Some(a) => (*a).clone(),
                None => return,
            },
            (Some(Tile::Folder(name, _)), None) => {
                if let Some(l) = self.launchpad.as_mut() {
                    l.folder = Some((p.index, TextField { caret: name.len(), text: name, ..Default::default() }));
                }
                return;
            }
            (None, _) => return,
        };
        self.launchpad_launch(Some(app));
    }

    /// Scrolling while open: a wheel step turns one page; a finger swipe drags the pages 1:1 and turns one when let go
    /// past `TURN`. `stop`: the fingers lifted. `true`: taken (all scrolling is, while open).
    pub fn launchpad_scroll(&mut self, dx: f64, dy: f64, wheel: f64, stop: bool) -> bool {
        if !self.launchpad_open() {
            return false;
        }
        let page = self.launchpad.as_ref().map_or(0, |l| l.page);
        if wheel != 0.0 {
            self.launchpad_turn(if wheel > 0.0 { page + 1 } else { page.saturating_sub(1) });
            return true;
        }
        let (Some(grid), pages) = (self.launchpad_grid(), self.launchpad_pages()) else { return true };
        let Some(l) = self.launchpad.as_mut() else { return true };
        if dx != 0.0 && dx.abs() >= dy.abs() {
            // Past the first and last page it only gives a little (rubber band).
            let next = l.drag + dx / f64::from(grid.width.max(1));
            let at = page as f64 + next;
            l.drag = if at < 0.0 || at > (pages - 1) as f64 { l.drag + (next - l.drag) / 3.0 } else { next };
            if let Some(token) = l.swipe_end.take() {
                self.loop_handle.remove(token);
            }
            let end = self.loop_handle.insert_source(Timer::from_duration(SWIPE_END), |_, _, state| {
                if let Some(l) = state.launchpad.as_mut() {
                    l.swipe_end = None;
                }
                state.launchpad_swipe_end();
                TimeoutAction::Drop
            });
            if let Some(l) = self.launchpad.as_mut() {
                l.swipe_end = end.ok();
            }
            self.schedule_redraw();
        }
        if stop {
            self.launchpad_swipe_end();
        }
        true
    }

    /// The fingers let go: on to the next page if dragged far enough, else back.
    fn launchpad_swipe_end(&mut self) {
        let Some(l) = self.launchpad.as_mut().filter(|l| l.drag != 0.0) else { return };
        if let Some(token) = l.swipe_end.take() {
            self.loop_handle.remove(token);
        }
        let page = match l.drag {
            d if d > TURN => l.page + 1,
            d if d < -TURN => l.page.saturating_sub(1),
            _ => l.page,
        };
        self.launchpad_turn(page);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str) -> App {
        App {
            id: name.to_lowercase(),
            name: name.into(),
            exec: name.to_lowercase(),
            icon: String::new(),
            wm_class: None,
            no_display: false,
            ..Default::default()
        }
    }

    #[test]
    fn grid_follows_the_screen() {
        let shape = |w, h| {
            let g = grid((w, h).into(), 101);
            (g.cols, g.rows, g.icon)
        };
        assert_eq!(shape(1280, 720), (5, 4, 64));
        assert_eq!(shape(1366, 768), (5, 4, 80));
        assert_eq!(shape(1920, 1080), (7, 5, 80));
        assert_eq!(shape(3840, 2160), (9, 6, 80));
        assert_eq!(shape(1080, 1920), (5, 6, 64), "portrait: clamped to 5 columns");
    }

    #[test]
    fn cells_fit_between_field_and_dots() {
        for (w, h) in [(1280, 720), (1920, 1080), (2560, 1440), (1080, 1920)] {
            let g = grid((w, h).into(), 101);
            assert_eq!(g.cells.len(), g.per_page());
            let screen = Rectangle::from_size((w, h).into());
            for i in 0..g.cells.len() {
                let (icon, name) = (g.icon_rect(i), g.name_rect(i));
                assert!(screen.contains_rect(icon) && g.cells[i].contains_rect(icon) && g.cells[i].contains_rect(name), "{w}×{h} cell {i}");
                assert!(icon.loc.y > g.field.loc.y + g.field.size.h, "below the search field");
                assert!(name.loc.y + name.size.h < g.dots_y, "above the dots");
            }
            // Centred.
            let (first, last) = (g.cells[0], g.cells[g.cols - 1]);
            assert!((first.loc.x - (w - last.loc.x - last.size.w)).abs() <= 1, "{w}×{h}");
        }
        let g = grid((1920, 1080).into(), 101);
        assert_eq!(g.field, Rectangle::new((840, 40).into(), (240, 28).into()));
        assert_eq!(g.hit(g.icon_rect(8).loc.to_f64() + Point::from((1.0, 1.0))), Some(8));
        assert_eq!(g.hit(g.name_rect(3).loc.to_f64() + Point::from((1.0, 1.0))), Some(3));
        assert_eq!(g.hit((5.0, 5.0).into()), None);
        assert_eq!(edge_of(&g, (10.0, 500.0).into()), Some(-1));
        assert_eq!(edge_of(&g, (1915.0, 500.0).into()), Some(1));
        assert_eq!(edge_of(&g, (960.0, 500.0).into()), None);
    }

    #[test]
    fn names_and_fields_grow_with_text_size() {
        for (w, h) in [(1280, 720), (1920, 1080)] {
            let at = |k| crate::text::with_scale(k, || grid((w, h).into(), 101));
            let (one, big) = (at(1.0), at(1.5));
            assert_eq!(one, grid((w, h).into(), 101), "1.0 is the default");
            assert_eq!((big.field.size.w, big.field.size.h), (360, 42));
            assert_eq!((big.cols, big.rows), (one.cols, one.rows), "the same apps per page");
            crate::text::with_scale(1.5, || {
                for i in 0..big.cells.len() {
                    // A Callout line (15 px) at 1.5 fits the name; icon and name stay in the cell, under the field.
                    let (icon, name) = (big.icon_rect(i), big.name_rect(i));
                    assert!(name.size.h >= 22 && big.cells[i].contains_rect(icon) && big.cells[i].contains_rect(name), "{w}×{h} cell {i}");
                    assert!(icon.loc.y > big.field.loc.y + big.field.size.h && name.loc.y + name.size.h < big.dots_y);
                }
                // The folder's name (17 / 22 px Title) fits its field at 1.5.
                let (panel, inner) = folder_grid(&big, 9);
                assert!(inner.field.size.h >= 33 && panel.contains_rect(inner.field) && inner.cells.iter().all(|c| !c.overlaps(inner.field)));
                assert!(Rectangle::from_size((w, h).into()).contains_rect(panel));
            });
        }
    }

    #[test]
    fn reorders() {
        let mut v = vec!['a', 'b', 'c', 'd'];
        move_to(&mut v, 0, 2);
        assert_eq!(v, ['b', 'c', 'a', 'd']);
        move_to(&mut v, 3, 0);
        assert_eq!(v, ['d', 'b', 'c', 'a']);
        move_to(&mut v, 1, 99);
        assert_eq!(v, ['d', 'c', 'a', 'b'], "past the end: last");
        move_to(&mut v, 9, 0);
        assert_eq!(v, ['d', 'c', 'a', 'b'], "nothing there: no change");
    }

    #[test]
    fn folder_panel_fits() {
        for (w, h) in [(1280, 720), (1920, 1080)] {
            let g = grid((w, h).into(), 101);
            for n in [1, 3, 9, 40] {
                let (panel, inner) = folder_grid(&g, n);
                assert!(Rectangle::from_size((w, h).into()).contains_rect(panel), "{w}×{h} {n}");
                assert!(inner.cells.iter().all(|c| panel.contains_rect(*c)) && panel.contains_rect(inner.field));
                assert!(inner.cells.iter().all(|c| !c.overlaps(inner.field)));
            }
            assert_eq!(folder_grid(&g, 3).1.per_page(), 3, "a small folder: one row");
        }
    }

    #[test]
    fn paging() {
        assert_eq!(pages(0, 35), 1);
        assert_eq!(pages(35, 35), 1);
        assert_eq!(pages(36, 35), 2);
        assert_eq!(pages(500, 35), 15);
        let g = grid((1920, 1080).into(), 101);
        let dots = g.dots(3);
        assert_eq!(dots[1].loc.x + layout::DOT / 2, 960, "the middle dot is centred");
        assert_eq!(dots[1].loc.x - dots[0].loc.x, layout::DOT_STEP);
        let mut l = Launchpad::new(Instant::now());
        let now = Instant::now();
        l.turn_to(5, 3, now);
        assert_eq!(l.page, 2, "clamped to the last page");
        l.turn_to(0, 0, now);
        assert_eq!(l.page, 0);
    }

    #[test]
    fn others_slide_aside() {
        let g = grid((1920, 1080).into(), 101);
        let now = Instant::now();
        let mut l = Launchpad::new(now);
        l.order = ["a", "b", "c", "d"].map(|id| Entry::App(id.into())).to_vec();
        l.reorder(&g, 0, 2, now);
        assert_eq!(l.order.iter().map(key).collect::<Vec<_>>(), ["b", "c", "a", "d"]);
        let off = l.offsets(now);
        let step = g.icon_rect(1).loc.x - g.icon_rect(0).loc.x;
        assert_eq!(off, [(step, 0).into(), (step, 0).into(), (0, 0).into(), (0, 0).into()], "b and c start where they were");
        assert!(l.offsets(now + Duration::from_secs(1)).iter().all(|p| *p == Point::default()), "and settle in place");
        // Across a page: the first tile of page 2 comes from the far right of page 1's strip.
        let pp = g.per_page();
        l.order = (0..pp + 1).map(|i| Entry::App(i.to_string())).collect();
        l.moves.clear();
        l.reorder(&g, pp, 0, now);
        assert_eq!(Launchpad::place(&g, pp).x - Launchpad::place(&g, 0).x, f64::from(g.width));
    }

    #[test]
    fn filters_by_name() {
        let mut hidden = app("Settings Daemon");
        hidden.no_display = true;
        let apps = [app("Ark"), app("Dolphin"), app("Firefox"), app("Kate"), app("Kdenlive"), hidden];
        let names = |q: &str| filter(&apps, q).iter().map(|a| a.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names(""), ["Ark", "Dolphin", "Firefox", "Kate", "Kdenlive"], "no NoDisplay apps");
        assert_eq!(names("k"), ["Kate", "Kdenlive", "Ark"], "prefix matches first");
        assert_eq!(names("FIRE"), ["Firefox"]);
        assert_eq!(names(" set"), Vec::<&str>::new());
    }

    fn app_id(e: &Entry) -> &str {
        match e {
            Entry::App(id) => id,
            Entry::Folder { name, .. } => name,
        }
    }

    #[test]
    fn saved_order_then_new_apps() {
        let apps = [app("Ark"), app("Dolphin"), app("Firefox"), app("Kate")];
        let a = |id: &str| Entry::App(id.into());
        let folder = |ids: &[&str]| Entry::Folder { name: "Tools".into(), apps: ids.iter().map(|i| i.to_string()).collect() };
        let order = [a("kate"), a("gone"), folder(&["dolphin", "gone2"]), a("kate")];
        let got = arrange(&order, &apps);
        assert_eq!(got.iter().map(app_id).collect::<Vec<_>>(), ["kate", "dolphin", "ark", "firefox"], "a folder of one dissolves, new apps last");
        let kept = arrange(&[folder(&["firefox", "ark"])], &apps);
        assert_eq!(kept[0], folder(&["firefox", "ark"]));
        assert_eq!(kept.len(), 3);
        assert_eq!(arrange(&[], &apps).len(), 4, "nothing saved: all, by name");
        let t = tiles(&apps, &[folder(&["firefox", "ark"])], "");
        assert!(matches!(&t[0], Tile::Folder(name, apps) if name == "Tools" && apps.len() == 2));
        assert_eq!(tiles(&apps, &[folder(&["firefox", "ark"])], "fire").len(), 1, "search finds apps in folders");
    }

    #[test]
    fn folders_form_on_drop() {
        let mut games = [app("Chess"), app("Mines"), app("Kate")];
        games[0].categories = vec!["Game".into(), "BoardGame".into()];
        games[1].categories = vec!["Game".into()];
        games[2].categories = vec!["Utility".into()];
        let mut order: Vec<Entry> = ["chess", "kate", "mines"].map(|id| Entry::App(id.into())).to_vec();
        let ids = |e: &Entry| match e {
            Entry::Folder { name, apps } => format!("{name}:{}", apps.join(",")),
            Entry::App(id) => id.clone(),
        };
        assert!(merge(&mut order, 2, 0, &games));
        assert_eq!(order.iter().map(ids).collect::<Vec<_>>(), ["Games:chess,mines", "kate"]);
        assert!(merge(&mut order, 1, 0, &games), "an app joins a folder");
        assert_eq!(order.iter().map(ids).collect::<Vec<_>>(), ["Games:chess,mines,kate"]);
        let mut two = vec![Entry::App("kate".into()), Entry::App("chess".into())];
        assert!(merge(&mut two, 0, 1, &games), "dragged forward onto the next");
        assert_eq!(two.iter().map(ids).collect::<Vec<_>>(), ["Folder:chess,kate"], "no shared category");
        assert!(!merge(&mut order, 0, 0, &games));
        let mut nest = vec![two[0].clone(), Entry::App("x".into())];
        assert!(!merge(&mut nest, 0, 1, &games), "folders don't nest");
    }

    #[test]
    fn dragged_out_of_a_folder() {
        let folder = |ids: &[&str]| Entry::Folder { name: "F".into(), apps: ids.iter().map(|i| i.to_string()).collect() };
        let mut order = vec![Entry::App("x".into()), folder(&["a", "b", "c"])];
        assert_eq!(take_out(&mut order, 1, 1), Some(2));
        assert_eq!(order, [Entry::App("x".into()), folder(&["a", "c"]), Entry::App("b".into())]);
        assert_eq!(take_out(&mut order, 1, 0), Some(2));
        assert_eq!(order, ["x", "c", "a", "b"].map(|id| Entry::App(id.into())), "a folder of one dissolves");
        assert_eq!(take_out(&mut order, 0, 0), None, "not a folder");
    }

    #[test]
    fn order_round_trips() {
        let order = vec![Entry::App("kate".into()), Entry::Folder { name: "Games".into(), apps: vec!["a".into(), "b".into()] }];
        let text = serde_json::to_string(&order).unwrap();
        assert_eq!(text, r#"["kate",{"name":"Games","apps":["a","b"]}]"#);
        assert_eq!(serde_json::from_str::<Vec<Entry>>(&text).unwrap(), order);
    }

    #[test]
    fn spring_settles() {
        let t0 = Instant::now();
        let s = Spring::new(0.0, 1.0, t0);
        assert_eq!(s.at(t0), 0.0);
        let mid = s.at(t0 + Duration::from_millis(200));
        assert!(mid > 0.5 && mid < 1.0, "{mid}");
        assert!(!s.done(t0 + Duration::from_millis(400)));
        assert!(s.done(t0 + Duration::from_millis(800)));
        assert_eq!(s.at(t0 + Duration::from_secs(1)), 1.0, "exactly there once settled");
        let mut s = s;
        s.go(0.0, t0 + Duration::from_millis(200));
        assert_eq!(s.at(t0 + Duration::from_millis(200)), mid, "turns back from where it is");
    }
}
