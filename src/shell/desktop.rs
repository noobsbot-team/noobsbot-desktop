//! Desktop icons (E7.2, L12): the files in the Desktop folder on the wallpaper, like macOS. A grid from the top right
//! down each column, then leftwards; click selects, double-click opens (xdg-open), drag rearranges (saved), right-click
//! menu: Open, Rename, Move to Trash (`gio trash`), New Folder. The folder is watched with inotify, so idle costs nothing.

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

use notify::{RecursiveMode, Watcher};
use smithay::{
    input::keyboard::Keysym,
    reexports::calloop::channel,
    utils::{Logical, Point, Rectangle, Size},
};

use super::{Anchor, Menu, Popup, Row};
use crate::{
    render::menubar,
    shortcuts::Action,
    state::State,
    ui::field::{Edit, TextField},
    wm::Rect,
};

/// Icon size and grid cell, in logical px (macOS: 64 px icons on a ~100 px grid).
pub const ICON: i32 = 64;
pub const CELL_W: i32 = 100;
pub const CELL_H: i32 = 110;
/// Names wrap to two lines this wide.
pub const LABEL_W: i32 = 92;
pub const LABEL_LINE: i32 = 15;
const MARGIN: i32 = 12;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// The pointer moves this far with the button down before an icon drags.
const DRAG_START: f64 = 4.0;
const FOLDER: &str = "folder";
/// Drawn when the theme has no icon for a file's type.
pub const FALLBACK_ICON: &str = "text-x-generic";

/// A file on the desktop.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    /// An icon-theme name (or path) for `apps::Icons`.
    pub icon: String,
}

/// A grid cell: column from the right edge, row from the top.
pub type Cell = (i32, i32);

/// A right-click menu row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Open,
    Rename,
    Trash,
    NewFolder,
}

/// An icon being pressed or dragged.
#[derive(Debug, Clone, PartialEq)]
pub struct Drag {
    pub name: String,
    start: Point<f64, Logical>,
    pub at: Point<f64, Logical>,
    pub moved: bool,
}

/// The icons and what the pointer and keyboard are doing with them. Identity is the file name, so a rescan never
/// mixes them up.
#[derive(Default)]
pub struct Desktop {
    /// The folder shown; empty until `desktop_start` finds it.
    pub dir: PathBuf,
    /// Where the arrangement is saved.
    pub store: PathBuf,
    started: bool,
    pub entries: Vec<Entry>,
    /// Cells the user put icons in, by name.
    pub saved: HashMap<String, Cell>,
    pub selected: Option<String>,
    pub renaming: Option<(String, TextField)>,
    /// The icon the open menu is for (`None`: the desktop itself) and where it opened, output-relative.
    pub menu: Option<(Option<String>, Point<i32, Logical>)>,
    pub drag: Option<Drag>,
    last_click: Option<(String, Instant)>,
}

/// Where the grid fits on an output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    pub width: i32,
    pub top: i32,
    pub rows: i32,
    pub cols: i32,
}

impl Grid {
    /// The grid on a `size` output, keeping `bottom` px clear for the dock.
    pub fn new(size: Size<i32, Logical>, bottom: i32) -> Self {
        let top = menubar::HEIGHT + 8;
        let rows = ((size.h - top - bottom) / CELL_H).max(1);
        let cols = ((size.w - 2 * MARGIN) / CELL_W).max(1);
        Self { width: size.w, top, rows, cols }
    }

    pub fn rect(&self, (col, row): Cell) -> Rect {
        Rectangle::new((self.width - MARGIN - (col + 1) * CELL_W, self.top + row * CELL_H).into(), (CELL_W, CELL_H).into())
    }

    /// The cell under `p`, kept on the grid.
    pub fn at(&self, p: Point<f64, Logical>) -> Cell {
        let col = ((f64::from(self.width - MARGIN) - p.x) / f64::from(CELL_W)).floor() as i32;
        let row = ((p.y - f64::from(self.top)) / f64::from(CELL_H)).floor() as i32;
        (col.clamp(0, self.cols - 1), row.clamp(0, self.rows - 1))
    }
}

/// The icon's square in its cell.
pub fn icon_rect(cell: Rect) -> Rect {
    Rectangle::new((cell.loc.x + (CELL_W - ICON) / 2, cell.loc.y + 6).into(), (ICON, ICON).into())
}

/// The name's two lines under the icon.
pub fn label_rect(cell: Rect) -> Rect {
    Rectangle::new((cell.loc.x + (CELL_W - LABEL_W) / 2, cell.loc.y + 6 + ICON + 6).into(), (LABEL_W, 2 * LABEL_LINE).into())
}

/// Cells for `names`: a saved one where it is on the grid and free, the rest in the first free cells, down each
/// column, then leftwards; more icons than cells pile up in the last one rather than run off the screen.
// ponytail: macOS shrinks the grid instead. Add when a desktop gets that full.
pub fn place(names: &[&str], saved: &HashMap<String, Cell>, grid: &Grid) -> Vec<Cell> {
    let fits = |&(c, r): &Cell| (0..grid.cols).contains(&c) && (0..grid.rows).contains(&r);
    let mut taken = std::collections::HashSet::new();
    let mut cells: Vec<Option<Cell>> = names.iter().map(|n| saved.get(*n).copied().filter(fits).filter(|c| taken.insert(*c))).collect();
    let mut next = 0;
    for cell in cells.iter_mut().filter(|c| c.is_none()) {
        while taken.contains(&(next / grid.rows, next % grid.rows)) {
            next += 1;
        }
        *cell = Some(if next < grid.rows * grid.cols { (next / grid.rows, next % grid.rows) } else { (grid.cols - 1, grid.rows - 1) });
        next += 1;
    }
    cells.into_iter().flatten().collect()
}

/// The Desktop folder: `XDG_DESKTOP_DIR` from `user-dirs.dirs` (or its env var), else `~/Desktop`. `None` when it is
/// the home folder itself, which the XDG spec says means no desktop.
pub fn desktop_dir(env: Option<&str>, user_dirs: &str, home: &Path) -> Option<PathBuf> {
    let value = env
        .map(str::to_owned)
        .or_else(|| user_dirs.lines().find_map(|l| l.trim().strip_prefix("XDG_DESKTOP_DIR=")).map(|v| v.trim().trim_matches('"').to_owned()));
    let dir = match value {
        Some(v) => match v.strip_prefix("$HOME") {
            Some(rest) => home.join(rest.trim_start_matches('/')),
            None if v.starts_with('/') => PathBuf::from(v),
            None => return None,
        },
        None => home.join("Desktop"),
    };
    (dir != home && dir != Path::new("/")).then_some(dir)
}

/// The user's Desktop folder, from the environment.
pub fn user_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home.join(".config"));
    let user_dirs = std::fs::read_to_string(config.join("user-dirs.dirs")).unwrap_or_default();
    desktop_dir(std::env::var("XDG_DESKTOP_DIR").ok().as_deref(), &user_dirs, &home)
}

fn store_path() -> PathBuf {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    state.join("noobsbot-desktop/desktop-icons")
}

/// The saved arrangement: one `col row name` line per icon.
pub fn parse_store(text: &str) -> HashMap<String, Cell> {
    text.lines()
        .filter_map(|l| {
            let mut parts = l.splitn(3, ' ');
            let (col, row, name) = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?);
            Some((name.to_owned(), (col, row)))
        })
        .collect()
}

fn format_store(saved: &HashMap<String, Cell>) -> String {
    let mut lines: Vec<String> = saved.iter().filter(|(n, _)| !n.contains('\n')).map(|(n, (c, r))| format!("{c} {r} {n}\n")).collect();
    lines.sort();
    lines.concat()
}

/// File type → icon name, from shared-mime-info's `globs2` and `generic-icons`.
#[derive(Debug, Default)]
pub struct Mime {
    /// (lowercase suffix after "*.", mime type), highest weight first.
    globs: Vec<(String, String)>,
    generic: HashMap<String, String>,
}

impl Mime {
    pub fn parse(globs2: &str, generic_icons: &str) -> Self {
        let globs = globs2
            .lines()
            .filter_map(|l| {
                let mut parts = l.split(':');
                let (_weight, mime, glob) = (parts.next()?, parts.next()?, parts.next()?);
                let suffix = glob.strip_prefix("*.").filter(|s| !s.contains(['*', '?', '[']))?;
                Some((suffix.to_lowercase(), mime.to_owned()))
            })
            .collect();
        let generic = generic_icons.lines().filter_map(|l| l.split_once(':')).map(|(m, i)| (m.to_owned(), i.to_owned())).collect();
        Self { globs, generic }
    }

    fn load() -> Self {
        // Every data dir's, the user's first (it usually holds only a few of its own).
        let read =
            |file: &str| crate::apps::data_dirs().iter().filter_map(|d| std::fs::read_to_string(d.join("mime").join(file)).ok()).collect::<Vec<_>>().join("\n");
        Self::parse(&read("globs2"), &read("generic-icons"))
    }

    /// Icon names for file `name`, best first: its type's own, the type's generic one, the media type's.
    pub fn icons(&self, name: &str) -> Vec<String> {
        let lower = name.to_lowercase();
        // The longest matching suffix wins (`.tar.gz` over `.gz`); among equals the higher weight, listed first.
        let mime = self
            .globs
            .iter()
            .filter(|(s, _)| lower.len() > s.len() && lower.ends_with(s.as_str()) && lower[..lower.len() - s.len()].ends_with('.'))
            .fold(None::<&(String, String)>, |best, g| if best.is_none_or(|b| g.0.len() > b.0.len()) { Some(g) } else { best });
        let Some((_, mime)) = mime else { return vec![FALLBACK_ICON.into()] };
        let media = mime.split('/').next().unwrap_or_default();
        let mut out = vec![mime.replace('/', "-")];
        out.extend(self.generic.get(mime).cloned());
        out.push(format!("{media}-x-generic"));
        out.push(FALLBACK_ICON.into());
        out
    }
}

/// The visible files of `dir`, by name (dotfiles and `~` backups hidden, as in Dolphin), with their icons.
fn scan(dir: &Path, mime: &Mime, found: &mut HashMap<String, bool>) -> Vec<Entry> {
    let mut has = |name: &str| *found.entry(name.to_owned()).or_insert_with(|| crate::apps::find_icon(name).is_some());
    let mut entries: Vec<Entry> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if name.starts_with('.') || name.ends_with('~') {
                return None;
            }
            let icon =
                if e.path().is_dir() { FOLDER.to_owned() } else { mime.icons(&name).into_iter().find(|i| has(i)).unwrap_or_else(|| FALLBACK_ICON.into()) };
            Some(Entry { name, icon })
        })
        .collect();
    entries.sort_by_key(|e| e.name.to_lowercase());
    entries
}

/// Lists `dir` on a worker thread, then again whenever something in it changes (after 100 ms of quiet). Stops when
/// `send` returns false.
fn watch(dir: PathBuf, mut send: impl FnMut(Vec<Entry>) -> bool + Send + 'static) {
    let spawned = std::thread::Builder::new().name("desktop".into()).spawn(move || {
        let mime = Mime::load();
        let mut found = HashMap::new();
        if !send(scan(&dir, &mime, &mut found)) {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let mut watcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(err) => return tracing::warn!("desktop icons: no live updates: {err}"),
        };
        if let Err(err) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
            return tracing::warn!("desktop icons: {}: {err}", dir.display());
        }
        while let Ok(event) = rx.recv() {
            if !event.is_ok_and(|e| !e.kind.is_access()) {
                continue;
            }
            while rx.recv_timeout(Duration::from_millis(100)).is_ok() {}
            if !send(scan(&dir, &mime, &mut found)) {
                return;
            }
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("desktop icons: {err}");
    }
}

/// A name a file can be renamed to.
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
}

/// Renames `from` to `to`, refusing to replace a file that is already there.
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let (from, to) = (CString::new(from.as_os_str().as_bytes())?, CString::new(to.as_os_str().as_bytes())?);
    // SAFETY: both are valid NUL-terminated paths for the duration of the call.
    let result = unsafe { libc::renameat2(libc::AT_FDCWD, from.as_ptr(), libc::AT_FDCWD, to.as_ptr(), libc::RENAME_NOREPLACE) };
    if result == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// "untitled folder", or "untitled folder 2", … — the first not taken in `taken`.
fn untitled(taken: impl Fn(&str) -> bool) -> String {
    (1..).map(|n| if n == 1 { "untitled folder".to_owned() } else { format!("untitled folder {n}") }).find(|n| !taken(n)).unwrap_or_default()
}

/// What one frame draws: each icon with its cell, which is selected, and the name being edited.
pub struct Frame {
    pub icons: Vec<(Entry, Rect)>,
    pub selected: Option<usize>,
    pub renaming: Option<(usize, TextField)>,
}

impl State {
    /// Starts showing the Desktop folder (once, and only with `[desktop] icons` on).
    pub fn desktop_start(&mut self) {
        if self.desktop.started || !self.config.desktop.icons {
            return;
        }
        if self.desktop.dir.as_os_str().is_empty() {
            let Some(dir) = user_dir() else { return };
            self.desktop.dir = dir;
            self.desktop.store = store_path();
        }
        self.desktop.started = true;
        self.desktop.saved = parse_store(&std::fs::read_to_string(&self.desktop.store).unwrap_or_default());
        let (tx, rx) = channel::channel();
        let inserted = self.loop_handle.insert_source(rx, |event, _, state| {
            if let channel::Event::Msg(entries) = event {
                state.desktop_update(entries);
            }
        });
        if let Err(err) = inserted {
            return tracing::warn!("desktop icons: {err}");
        }
        watch(self.desktop.dir.clone(), move |entries| tx.send(entries).is_ok());
    }

    fn desktop_update(&mut self, entries: Vec<Entry>) {
        // Icons on screen keep their cells; new files take free ones instead of reflowing the rest (as on macOS).
        if let Some((_, cells)) = self.desktop_cells() {
            for (e, cell) in self.desktop.entries.iter().zip(cells) {
                self.desktop.saved.entry(e.name.clone()).or_insert(cell);
            }
        }
        let d = &mut self.desktop;
        d.entries = entries;
        let has = |name: &Option<String>, entries: &[Entry]| name.as_ref().is_none_or(|n| entries.iter().any(|e| e.name == *n));
        if !has(&d.selected, &d.entries) {
            d.selected = None;
        }
        if !has(&d.renaming.as_ref().map(|(n, _)| n.clone()), &d.entries) {
            d.renaming = None;
        }
        self.schedule_redraw();
    }

    fn desktop_shown(&self) -> bool {
        self.config.desktop.icons && self.desktop.started
    }

    /// The grid on the first output, clear of the bar and the dock.
    fn desktop_grid(&self) -> Option<Grid> {
        let size = self.space.outputs().next().and_then(|o| self.space.output_geometry(o))?.size;
        Some(Grid::new(size, i32::from(self.config.dock.icon_size) + 24))
    }

    /// Every icon's cell, in `entries` order.
    fn desktop_cells(&self) -> Option<(Grid, Vec<Cell>)> {
        let grid = self.desktop_grid()?;
        let names: Vec<&str> = self.desktop.entries.iter().map(|e| e.name.as_str()).collect();
        Some((grid, place(&names, &self.desktop.saved, &grid)))
    }

    /// The icon (index) whose picture or name is under `p`, output-relative.
    fn desktop_hit(&self, p: Point<f64, Logical>) -> Option<usize> {
        let (grid, cells) = self.desktop_cells()?;
        cells.iter().position(|c| {
            let cell = grid.rect(*c);
            icon_rect(cell).to_f64().contains(p) || label_rect(cell).to_f64().contains(p)
        })
    }

    /// This frame's icons, or `None` when they are off.
    pub fn desktop_frame(&self) -> Option<Frame> {
        if !self.desktop_shown() || self.desktop.entries.is_empty() {
            return None;
        }
        let (grid, cells) = self.desktop_cells()?;
        let d = &self.desktop;
        let icons = d
            .entries
            .iter()
            .zip(cells)
            .map(|(e, c)| {
                let mut rect = grid.rect(c);
                // The dragged icon follows the pointer.
                if let Some(drag) = d.drag.as_ref().filter(|g| g.moved && g.name == e.name) {
                    rect.loc += (drag.at - drag.start).to_i32_round();
                }
                (e.clone(), rect)
            })
            .collect();
        let index = |name: &str| d.entries.iter().position(|e| e.name == name);
        Some(Frame { icons, selected: d.selected.as_deref().and_then(index), renaming: d.renaming.as_ref().and_then(|(n, f)| Some((index(n)?, f.clone()))) })
    }

    /// Any press (output-relative): one outside the name being edited finishes the rename.
    pub fn desktop_blur(&mut self, location: Point<f64, Logical>) {
        let Some((name, _)) = &self.desktop.renaming else { return };
        let on_it = self.desktop.entries.iter().position(|e| e.name == *name).is_some_and(|i| self.desktop_hit(location) == Some(i));
        if !on_it {
            self.desktop_rename_commit();
        }
    }

    /// A press on the bare desktop (no window or layer there): selects, opens on a double click, starts a drag, or
    /// opens the right-click menu.
    pub fn desktop_press(&mut self, location: Point<f64, Logical>, button: u32) {
        if !self.desktop_shown() {
            return;
        }
        let hit = self.desktop_hit(location).map(|i| self.desktop.entries[i].name.clone());
        let d = &mut self.desktop;
        d.selected = hit.clone();
        match (button, hit) {
            (crate::input::BTN_LEFT, Some(name)) => {
                let now = Instant::now();
                let double = d.last_click.as_ref().is_some_and(|(n, at)| *n == name && now - *at < DOUBLE_CLICK);
                if double {
                    d.last_click = None;
                    self.desktop_open(&name);
                } else {
                    d.last_click = Some((name.clone(), now));
                    d.drag = Some(Drag { name, start: location, at: location, moved: false });
                }
            }
            (crate::input::BTN_RIGHT, name) => {
                let at = location.to_i32_round();
                d.menu = Some((name, at));
                self.popup = Some(Popup::Menu(Menu::Desktop));
            }
            _ => {}
        }
        self.schedule_redraw();
    }

    pub fn desktop_motion(&mut self, to: Point<f64, Logical>) {
        let Some(drag) = self.desktop.drag.as_mut() else { return };
        drag.at = to;
        let (dx, dy) = (to.x - drag.start.x, to.y - drag.start.y);
        drag.moved |= dx.hypot(dy) >= DRAG_START;
        if drag.moved {
            self.schedule_redraw();
        }
    }

    /// The button came up: a dragged icon lands in the cell under its centre, swapping with any icon there.
    pub fn desktop_release(&mut self) {
        let Some(drag) = self.desktop.drag.take().filter(|d| d.moved) else { return };
        let Some((grid, cells)) = self.desktop_cells() else { return };
        let names: Vec<String> = self.desktop.entries.iter().map(|e| e.name.clone()).collect();
        let Some(from) = names.iter().position(|n| *n == drag.name) else { return };
        let centre = grid.rect(cells[from]).to_f64();
        let centre = centre.loc + centre.size.downscale(2.0).to_point() + (drag.at - drag.start);
        let target = grid.at(centre);
        // Every icon keeps the cell it shows in now, so the rest stay put as files come and go.
        self.desktop.saved = names.iter().cloned().zip(cells.iter().copied()).collect();
        if let Some(other) = cells.iter().position(|c| *c == target) {
            self.desktop.saved.insert(names[other].clone(), cells[from]);
        }
        self.desktop.saved.insert(drag.name, target);
        self.desktop_save();
        self.schedule_redraw();
    }

    fn desktop_save(&self) {
        let store = &self.desktop.store;
        let saved = store.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| crate::config::save_atomic(store, &format_store(&self.desktop.saved)));
        if let Err(err) = saved {
            tracing::warn!("desktop icons: {}: {err}", store.display());
        }
    }

    /// Keys with no window focused: the name being edited takes them all; Return renames the selected icon.
    pub fn desktop_key(&mut self, sym: Keysym, ctrl: bool) -> bool {
        if let Some((_, field)) = self.desktop.renaming.as_mut() {
            let typed = sym.key_char().map(String::from);
            match field.key(sym, typed.as_deref(), ctrl) {
                Edit::Submit => self.desktop_rename_commit(),
                Edit::Cancel => self.desktop.renaming = None,
                _ => {}
            }
            self.schedule_redraw();
            return true;
        }
        let unfocused = self.seat.get_keyboard().is_some_and(|k| k.current_focus().is_none());
        match (&self.desktop.selected, sym) {
            (Some(name), Keysym::Return | Keysym::KP_Enter) if unfocused && self.desktop_shown() => {
                let name = name.clone();
                self.desktop_rename(name);
                true
            }
            _ => false,
        }
    }

    fn desktop_rename(&mut self, name: String) {
        let field = TextField { caret: name.len(), all: true, text: name.clone(), ..Default::default() };
        self.desktop.renaming = Some((name, field));
        self.schedule_redraw();
    }

    /// Applies the edited name; an invalid or taken one leaves the file as it was.
    fn desktop_rename_commit(&mut self) {
        let Some((old, field)) = self.desktop.renaming.take() else { return };
        let new = field.text.trim().to_owned();
        self.schedule_redraw();
        if new == old || !valid_name(&new) {
            return;
        }
        let dir = &self.desktop.dir;
        match rename_noreplace(&dir.join(&old), &dir.join(&new)) {
            Ok(()) => {
                if let Some(cell) = self.desktop.saved.remove(&old) {
                    self.desktop.saved.insert(new.clone(), cell);
                    self.desktop_save();
                }
                // Shown at once; the watcher's rescan confirms it.
                if let Some(e) = self.desktop.entries.iter_mut().find(|e| e.name == old) {
                    e.name = new.clone();
                }
                self.desktop.selected = Some(new);
            }
            Err(err) => tracing::warn!("rename {old} → {new}: {err}"),
        }
    }

    fn desktop_open(&mut self, name: &str) {
        let path = self.desktop.dir.join(name).to_string_lossy().into_owned();
        self.spawn(&["xdg-open".into(), path]);
    }

    /// The right-click menu's rows: an icon's, or the desktop's.
    // ponytail: Change Wallpaper… stays disabled until System Settings (E7.1) has a wallpaper pane to open.
    pub fn desktop_menu_rows(&self) -> Vec<Option<Row>> {
        let row = |label: &str, command: Option<Command>| Some(Row { label: label.to_owned(), shortcut: "", action: command.map(Action::Desktop) });
        match &self.desktop.menu {
            Some((Some(_), _)) => {
                vec![row("Open", Some(Command::Open)), None, row("Rename", Some(Command::Rename)), row("Move to Trash", Some(Command::Trash))]
            }
            _ => vec![row("New Folder", Some(Command::NewFolder)), None, row("Change Wallpaper…", None)],
        }
    }

    /// Where the right-click menu opens: its top-left at the click, flipped up when it would run off the bottom.
    pub fn desktop_menu_anchor(&self, rows: &[Option<Row>]) -> Option<Anchor> {
        let (_, at) = self.desktop.menu.as_ref()?;
        let height = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map_or(i32::MAX, |g| g.size.h);
        let tall = super::menu_height(rows);
        let y = if at.y + tall > height - MARGIN { at.y - tall } else { at.y };
        Some(Anchor::At((at.x, y.max(menubar::HEIGHT)).into()))
    }

    pub fn desktop_command(&mut self, command: Command) {
        let Some((target, at)) = self.desktop.menu.take() else { return };
        let dir = self.desktop.dir.clone();
        match (command, target) {
            (Command::Open, Some(name)) => self.desktop_open(&name),
            (Command::Rename, Some(name)) => self.desktop_rename(name),
            (Command::Trash, Some(name)) => {
                self.spawn(&["gio".into(), "trash".into(), dir.join(&name).to_string_lossy().into_owned()]);
            }
            (Command::NewFolder, _) => {
                let name = untitled(|n| dir.join(n).symlink_metadata().is_ok());
                if let Err(err) = std::fs::create_dir(dir.join(&name)) {
                    return tracing::warn!("new folder in {}: {err}", dir.display());
                }
                // In the cell clicked if it is free, else the first free one; shown at once and renamed.
                if let Some((grid, cells)) = self.desktop_cells() {
                    let cell = grid.at(at.to_f64());
                    if !cells.contains(&cell) {
                        self.desktop.saved.insert(name.clone(), cell);
                        self.desktop_save();
                    }
                }
                if !self.desktop.entries.iter().any(|e| e.name == name) {
                    self.desktop.entries.push(Entry { name: name.clone(), icon: FOLDER.into() });
                }
                self.desktop.selected = Some(name.clone());
                self.desktop_rename(name);
            }
            _ => {}
        }
    }
}

/// `--show desktop`: sample files in a scratch Desktop folder (under the temp dir), one selected and its menu open
/// once listed. Never the user's own Desktop.
pub fn sample(state: &mut State) {
    let root = std::env::temp_dir().join("noobsbot-desktop-sample");
    let dir = root.join("Desktop");
    let made = std::fs::create_dir_all(dir.join("Projects")).and_then(|()| {
        for file in ["Invoice March 2026.pdf", "holiday.jpg", "notes.txt", "Quarterly report with a long name.odt", "song.mp3", "backup.tar.gz", "script.sh"] {
            std::fs::write(dir.join(file), b"sample")?;
        }
        Ok(())
    });
    if let Err(err) = made {
        return tracing::warn!("--show desktop: {}: {err}", dir.display());
    }
    state.desktop.dir = dir;
    state.desktop.store = root.join("desktop-icons");
    state.desktop.selected = Some("notes.txt".into());
}

impl State {
    /// `--show desktop-menu` / `desktop-rename`, once the sample folder is listed.
    pub fn desktop_show(&mut self, show: &str) {
        let name = "notes.txt".to_owned();
        let at =
            self.desktop_frame().and_then(|f| f.icons.into_iter().find(|(e, _)| e.name == name)).map(|(_, cell)| icon_rect(cell).loc + Point::from((40, 40)));
        match (show, at) {
            ("desktop-menu", Some(at)) => {
                self.desktop.menu = Some((Some(name), at));
                self.popup = Some(Popup::Menu(Menu::Desktop));
            }
            ("desktop-rename", _) => self.desktop_rename(name),
            _ => {}
        }
        self.schedule_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Grid {
        Grid::new((1280, 800).into(), 72)
    }

    #[test]
    fn grid_fills_from_the_top_right() {
        let g = grid();
        // (800 - 32 - 72) / 110 = 6 rows.
        assert_eq!((g.rows, g.cols, g.top), (6, 12, 32));
        assert_eq!(g.rect((0, 0)), Rectangle::new((1280 - 12 - 100, 32).into(), (100, 110).into()));
        assert_eq!(g.rect((1, 2)).loc, (1280 - 12 - 200, 32 + 220).into());
        // Back from a point to its cell, clamped onto the grid.
        assert_eq!(g.at((1200.0, 40.0).into()), (0, 0));
        assert_eq!(g.at((1100.0, 300.0).into()), (1, 2));
        assert_eq!(g.at((-50.0, 5000.0).into()), (11, 5));
        let cell = g.rect((0, 0));
        assert!(cell.contains_rect(icon_rect(cell)) && cell.contains_rect(label_rect(cell)));
        assert!(!icon_rect(cell).overlaps(label_rect(cell)));
    }

    #[test]
    fn saved_cells_win_and_the_rest_flow_around_them() {
        let g = grid();
        let saved = HashMap::from([("b".to_owned(), (0, 0)), ("c".to_owned(), (3, 1)), ("d".to_owned(), (40, 0)), ("e".to_owned(), (3, 1))]);
        // b and c keep theirs; d's is off the grid and e's is c's, so they flow into the free cells like a.
        assert_eq!(place(&["a", "b", "c", "d", "e"], &saved, &g), vec![(0, 1), (0, 0), (3, 1), (0, 2), (0, 3)]);
        // Columns wrap after `rows` icons.
        let many: Vec<String> = (0..8).map(|i| i.to_string()).collect();
        let names: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(place(&names, &HashMap::new(), &g)[6..], [(1, 0), (1, 1)]);
        // A hundred icons on a 6×12 grid: the 73rd onwards share the last cell, none leaves the screen.
        let many: Vec<String> = (0..100).map(|i| i.to_string()).collect();
        let names: Vec<&str> = many.iter().map(String::as_str).collect();
        let cells = place(&names, &HashMap::new(), &g);
        assert!(cells.iter().all(|&(c, r)| (0..g.cols).contains(&c) && (0..g.rows).contains(&r)));
        assert_eq!(cells[71], (11, 5));
        assert!(cells[72..].iter().all(|&c| c == (11, 5)));
    }

    #[test]
    fn store_round_trips() {
        let saved = HashMap::from([("a file.txt".to_owned(), (2, 3)), ("x".to_owned(), (0, 0))]);
        assert_eq!(format_store(&saved), "0 0 x\n2 3 a file.txt\n");
        assert_eq!(parse_store(&format_store(&saved)), saved);
        assert_eq!(parse_store("junk\n1 x y\n"), HashMap::new());
    }

    #[test]
    fn finds_the_desktop_folder() {
        let home = Path::new("/home/u");
        assert_eq!(desktop_dir(None, "", home), Some("/home/u/Desktop".into()));
        let dirs = "# comment\nXDG_DOWNLOAD_DIR=\"$HOME/Downloads\"\nXDG_DESKTOP_DIR=\"$HOME/Schreibtisch\"\n";
        assert_eq!(desktop_dir(None, dirs, home), Some("/home/u/Schreibtisch".into()));
        assert_eq!(desktop_dir(None, "XDG_DESKTOP_DIR=\"/data/desk\"", home), Some("/data/desk".into()));
        assert_eq!(desktop_dir(Some("/tmp/scratch"), dirs, home), Some("/tmp/scratch".into()));
        // Desktop = home means no desktop.
        assert_eq!(desktop_dir(None, "XDG_DESKTOP_DIR=\"$HOME/\"", home), None);
    }

    #[test]
    fn file_types_pick_icons() {
        let globs = "50:application/gzip:*.gz\n50:application/x-compressed-tar:*.tar.gz\n50:image/jpeg:*.jpg\n50:text/x-c:*.[ch]\n";
        let mime = Mime::parse(globs, "application/x-compressed-tar:package-x-generic\n");
        assert_eq!(mime.icons("a.tar.gz"), ["application-x-compressed-tar", "package-x-generic", "application-x-generic", FALLBACK_ICON]);
        assert_eq!(mime.icons("b.gz")[0], "application-gzip");
        assert_eq!(mime.icons("Photo.JPG")[..2], ["image-jpeg", "image-x-generic"]);
        assert_eq!(mime.icons("jpg"), [FALLBACK_ICON], "a bare name is no extension");
        assert_eq!(mime.icons("README"), [FALLBACK_ICON]);
    }

    #[test]
    fn names_and_renames_are_safe() {
        assert!(valid_name("report.pdf") && !valid_name("") && !valid_name("..") && !valid_name("a/b"));
        assert_eq!(untitled(|n| n == "untitled folder" || n == "untitled folder 2"), "untitled folder 3");
        let dir = std::env::temp_dir().join(format!("noobsbot-desktop-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a"), "a").unwrap();
        std::fs::write(dir.join("b"), "b").unwrap();
        assert!(rename_noreplace(&dir.join("a"), &dir.join("b")).is_err(), "never replaces a file");
        assert_eq!(std::fs::read_to_string(dir.join("b")).unwrap(), "b");
        rename_noreplace(&dir.join("a"), &dir.join("c")).unwrap();
        let listed: Vec<String> = scan(&dir, &Mime::default(), &mut HashMap::new()).into_iter().map(|e| e.name).collect();
        assert_eq!(listed, ["b", "c"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
