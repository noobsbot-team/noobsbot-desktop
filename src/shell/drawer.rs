//! The app drawer: a classic application menu in our glass. A category sidebar (from the entries' freedesktop
//! `Categories`) beside a list of apps with their icon, name and one-line description; a search field on top that
//! every typed key goes to; Lock, Log Out and Shut Down in a strip on the left. Right-click an app to add it to
//! Favorites, the dock or the desktop. Opens with Super+A, a lone tap of Super (`[app_drawer] super_tap`) or the
//! dock's `app-drawer` icon; sits bottom-left above the dock, or over that icon.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use smithay::{
    input::keyboard::Keysym,
    reexports::calloop::timer::{TimeoutAction, Timer},
    utils::{Logical, Point, Rectangle, Size},
};

use super::{Menu, Popup, Row};
use crate::{
    apps::App,
    render::menubar,
    shell::alert::Power,
    shortcuts::Action,
    state::State,
    symbols::Symbol,
    text::ts,
    ui::field::{Edit, TextField},
    wm::Rect,
};

/// The dock item that opens the drawer (`[dock] items`).
pub const DOCK_KEY: &str = "app-drawer";
/// Its icon in the icon theme.
pub const ICON: &str = "view-app-grid";

/// Recent apps kept.
pub const RECENT: usize = 12;

/// The sidebar, in the usual freedesktop order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Favorites,
    All,
    Recent,
    Accessories,
    Development,
    Education,
    Games,
    Graphics,
    Internet,
    Office,
    Multimedia,
    Science,
    Settings,
    System,
    Other,
}

impl Category {
    pub const ALL: [Category; 15] = [
        Category::Favorites,
        Category::All,
        Category::Recent,
        Category::Accessories,
        Category::Development,
        Category::Education,
        Category::Games,
        Category::Graphics,
        Category::Internet,
        Category::Office,
        Category::Multimedia,
        Category::Science,
        Category::Settings,
        Category::System,
        Category::Other,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Category::Favorites => "Favorites",
            Category::All => "All Applications",
            Category::Recent => "Recent",
            Category::Accessories => "Accessories",
            Category::Development => "Development",
            Category::Education => "Education",
            Category::Games => "Games",
            Category::Graphics => "Graphics",
            Category::Internet => "Internet",
            Category::Office => "Office",
            Category::Multimedia => "Sound & Video",
            Category::Science => "Science",
            Category::Settings => "Preferences",
            Category::System => "System Tools",
            Category::Other => "Other",
        }
    }

    pub fn symbol(self) -> Symbol {
        match self {
            Category::Favorites => Symbol::Star,
            Category::All => Symbol::SquaresFour,
            Category::Recent => Symbol::ClockCounterClockwise,
            Category::Accessories => Symbol::Wrench,
            Category::Development => Symbol::Code,
            Category::Education => Symbol::GraduationCap,
            Category::Games => Symbol::GameController,
            Category::Graphics => Symbol::PaintBrush,
            Category::Internet => Symbol::Globe,
            Category::Office => Symbol::Briefcase,
            Category::Multimedia => Symbol::FilmStrip,
            Category::Science => Symbol::Flask,
            Category::Settings => Symbol::Gear,
            Category::System => Symbol::Cpu,
            Category::Other => Symbol::DotsThreeCircle,
        }
    }

    /// The category a freedesktop main category files under.
    fn of_main(category: &str) -> Option<Category> {
        Some(match category {
            "AudioVideo" | "Audio" | "Video" => Category::Multimedia,
            "Development" => Category::Development,
            "Education" => Category::Education,
            "Game" => Category::Games,
            "Graphics" => Category::Graphics,
            "Network" => Category::Internet,
            "Office" => Category::Office,
            "Science" => Category::Science,
            "Settings" => Category::Settings,
            "System" => Category::System,
            "Utility" => Category::Accessories,
            _ => return None,
        })
    }
}

/// The categories `app` is listed under: every main category it names, else Other.
pub fn categories(app: &App) -> Vec<Category> {
    let mut out: Vec<Category> = Vec::new();
    for c in app.categories.iter().filter_map(|c| Category::of_main(c)) {
        if !out.contains(&c) {
            out.push(c);
        }
    }
    if out.is_empty() {
        out.push(Category::Other);
    }
    out
}

/// The sidebar for `apps`: Favorites, All and Recent always, the rest when they list something.
pub fn shown(apps: &[App]) -> Vec<Category> {
    let used: Vec<Category> = apps.iter().filter(|a| !a.no_display).flat_map(categories).collect();
    Category::ALL.into_iter().filter(|c| matches!(c, Category::Favorites | Category::All | Category::Recent) || used.contains(c)).collect()
}

/// The launchable apps `category` lists: by name, Favorites and Recent in their own order.
pub fn list<'a>(apps: &'a [App], category: Category, favorites: &[String], recent: &[String]) -> Vec<&'a App> {
    let by_ids = |ids: &[String]| ids.iter().filter_map(|id| apps.iter().find(|a| a.id == *id && !a.no_display)).collect();
    match category {
        Category::Favorites => by_ids(favorites),
        Category::Recent => by_ids(recent),
        Category::All => apps.iter().filter(|a| !a.no_display).collect(),
        c => apps.iter().filter(|a| !a.no_display && categories(a).contains(&c)).collect(),
    }
}

/// How well `app` matches `query` (lower case): its name and program as Spotlight ranks them, then the start of a
/// keyword or of a word in its generic name ("browser" finds Helium), a little below a name match.
pub fn rank(app: &App, query: &str) -> Option<i32> {
    let score = crate::shell::spotlight::score;
    let words = app.keywords.iter().chain([&app.generic_name]).filter_map(|k| score(k, query).filter(|s| *s >= 800)).max().map(|s| s - 250);
    crate::shell::spotlight::app_score(app, query).max(words)
}

/// Every launchable app matching `query`, best first (shorter names win ties, as in Spotlight).
// ponytail: a linear scan per keystroke, as Spotlight does (a few hundred apps, well under a millisecond).
pub fn search<'a>(apps: &'a [App], query: &str) -> Vec<&'a App> {
    let query = crate::shell::spotlight::fold(query.trim());
    if query.is_empty() {
        return Vec::new();
    }
    let mut ranked: Vec<(i32, &App)> = apps.iter().filter(|a| !a.no_display).filter_map(|a| Some((rank(a, &query)?, a))).collect();
    ranked.sort_by(|(sa, a), (sb, b)| sb.cmp(sa).then(a.name.len().cmp(&b.name.len())).then_with(|| a.name.cmp(&b.name)));
    ranked.into_iter().map(|(_, a)| a).collect()
}

/// `$XDG_STATE_HOME/noobsbot-desktop/<name>`: favorites and recent apps live there, one desktop-file id per line.
/// State, not config.toml: they change with every click, and rewriting the owner's hand-edited (often symlinked)
/// config for that would be noisy; the emoji picker's recents live there too.
pub fn state_file(name: &str) -> PathBuf {
    crate::config::xdg_dir("XDG_STATE_HOME", ".local/state").join("noobsbot-desktop").join(name)
}

/// The ids in `path`; none when it is missing.
pub fn load(path: &Path) -> Vec<String> {
    // Lossy: a damaged line costs that entry, not the whole file (which would then be overwritten empty).
    let text = std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from).collect()
}

pub fn save(path: &Path, ids: &[String]) {
    let text: String = ids.iter().map(|id| format!("{id}\n")).collect();
    // Off the main loop: a launch shouldn't wait for the sync.
    crate::config::save_later(path.to_owned(), move |_| text);
}

/// `list` with `id` added at the end, or taken out if it is in.
pub fn toggle(list: &[String], id: &str) -> Vec<String> {
    if list.iter().any(|i| i == id) { list.iter().filter(|i| *i != id).cloned().collect() } else { list.iter().cloned().chain([id.to_owned()]).collect() }
}

/// `recent` with `id` in front, once, at most `RECENT` long.
pub fn remember(recent: &[String], id: &str) -> Vec<String> {
    std::iter::once(id.to_owned()).chain(recent.iter().filter(|i| *i != id).cloned()).take(RECENT).collect()
}

/// Logical px. The width, the field, the sidebar, its rows, the header and the list rows grow with the text size
/// (`text::ts`); icons, buttons and radii don't.
pub mod layout {
    pub const WIDTH: i32 = 720;
    pub const HEIGHT: i32 = 560;
    pub const RADIUS: f64 = 14.0;
    pub const PAD: i32 = 12;
    pub const FIELD: i32 = 32;
    pub const MAGNIFIER: i32 = 16;
    /// The session strip on the left, and its round buttons.
    pub const STRIP: i32 = 52;
    pub const BUTTON: i32 = 32;
    pub const SIDEBAR: i32 = 192;
    pub const CATEGORY: i32 = 30;
    pub const CATEGORY_MIN: i32 = 22;
    pub const HEADER: i32 = 30;
    pub const ROW: i32 = 52;
    pub const ICON: i32 = 36;
    /// From the dock, the dock icon and the screen's edges.
    pub const GAP: i32 = 8;
}

/// Where the drawer opens: the bottom-left corner above the dock, or centred over a dock icon (its centre x and top).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    Corner,
    Dock(Point<i32, Logical>),
}

/// Where everything is, output-relative.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub rect: Rect,
    /// The Noobsbot mark at the top of the strip.
    pub logo: Rect,
    pub field: Rect,
    pub magnifier: Rect,
    /// Lock, Log Out, Shut Down, top to bottom.
    pub session: [Rect; 3],
    /// One per shown category that fits.
    pub categories: Vec<Rect>,
    /// The hairline between the sidebar and the list.
    pub divider: Rect,
    pub header: Rect,
    /// One per row that fits.
    pub rows: Vec<Rect>,
}

/// What a point in the drawer is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Field,
    Category(usize),
    /// A visible row, from the top one.
    Row(usize),
    Session(usize),
}

impl Layout {
    pub fn hit(&self, p: Point<f64, Logical>) -> Option<Target> {
        let at = |rects: &[Rect]| rects.iter().position(|r| r.to_f64().contains(p));
        if self.field.to_f64().contains(p) {
            return Some(Target::Field);
        }
        at(&self.categories).map(Target::Category).or_else(|| at(&self.rows).map(Target::Row)).or_else(|| at(&self.session).map(Target::Session))
    }

    /// The list's area: the header and the rows.
    pub fn list(&self) -> Rect {
        let bottom = self.rows.last().map_or(self.header.loc.y + self.header.size.h, |r| r.loc.y + r.size.h);
        Rectangle::new(self.header.loc, (self.header.size.w, bottom - self.header.loc.y).into())
    }
}

/// The drawer at `anchor` on an `output`-sized screen with `dock` px kept for the dock, with `categories` in its
/// sidebar: 720 × 560 (wider with the text size) or what fits under the menu bar.
pub fn layout(anchor: Anchor, output: Size<i32, Logical>, dock: i32, categories: usize) -> Layout {
    use layout::*;
    let (field_h, sidebar, category, category_min, header_h, row_h) = (ts(FIELD), ts(SIDEBAR), ts(CATEGORY), ts(CATEGORY_MIN), ts(HEADER), ts(ROW));
    let w = ts(WIDTH).min(output.w - 2 * GAP).max(STRIP + sidebar + 120);
    let bottom = match anchor {
        Anchor::Corner => output.h - dock - GAP,
        Anchor::Dock(p) => p.y - GAP,
    };
    let top = menubar::height() + GAP;
    let h = HEIGHT.min(bottom - top).max(PAD + field_h + PAD + header_h + row_h + PAD);
    let x = match anchor {
        Anchor::Corner => GAP,
        Anchor::Dock(p) => p.x - w / 2,
    };
    let x = x.min(output.w - GAP - w).max(GAP);
    let y = (bottom - h).max(top);
    let rect = Rectangle::new((x, y).into(), (w, h).into());
    let end = y + h - PAD;
    let left = x + STRIP;
    let field = Rectangle::new((left, y + PAD).into(), (x + w - PAD - left, field_h).into());
    let magnifier = Rectangle::new((left + 10, y + PAD + (field_h - MAGNIFIER) / 2).into(), (MAGNIFIER, MAGNIFIER).into());
    let strip_x = x + (STRIP - BUTTON) / 2;
    let logo = Rectangle::new((strip_x, y + PAD).into(), (BUTTON, BUTTON).into());
    let session = std::array::from_fn(|i| Rectangle::new((strip_x, end - (3 - i as i32) * (BUTTON + 8) + 8).into(), (BUTTON, BUTTON).into()));
    let body = y + PAD + field_h + PAD;
    // Short screen: the sidebar rows tighten (to the text's height) before any category is left out.
    let row = ((end - body) / categories.max(1) as i32).clamp(category_min, category);
    let fit = ((end - body) / row).max(0) as usize;
    let categories = (0..categories.min(fit) as i32).map(|i| Rectangle::new((left, body + i * row).into(), (sidebar - 12, row).into())).collect();
    let divider = Rectangle::new((left + sidebar - 1, body).into(), (1, end - body).into());
    let list_x = left + sidebar + 8;
    let list_w = x + w - PAD - list_x;
    let header = Rectangle::new((list_x, body).into(), (list_w, header_h).into());
    let rows_top = body + header_h;
    let rows = (0..((end - rows_top) / row_h).max(0)).map(|i| Rectangle::new((list_x, rows_top + i * row_h).into(), (list_w, row_h).into())).collect();
    Layout { rect, logo, field, magnifier, session, categories, divider, header, rows }
}

/// The session buttons' actions and tooltips, as laid out.
pub const SESSION: [(Symbol, &str); 3] = [(Symbol::Lock, "Lock Screen"), (Symbol::SignOut, "Log Out…"), (Symbol::Power, "Shut Down…")];

fn session_action(i: usize) -> Action {
    match i {
        0 => Action::Lock,
        1 => Action::Power(Power::LogOut),
        _ => Action::Power(Power::ShutDown),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Drawer {
    pub field: TextField,
    pub category: Category,
    /// The highlighted app, an index into the list shown.
    pub selected: usize,
    /// The first row shown.
    pub scroll: usize,
    /// Arrow keys move through the sidebar rather than the list.
    pub sidebar: bool,
    pub favorites: Vec<String>,
    pub recent: Vec<String>,
    pub anchor: Anchor,
    /// What the pointer is over.
    pub hover: Option<Target>,
    /// The app the right-click menu is for, and where it opens (output-relative).
    pub menu: Option<(String, Point<i32, Logical>)>,
    /// Touchpad scrolling that hasn't made a whole row yet, in px.
    pub scroll_px: f64,
    /// The category the pointer rests on, and since when: shown after `HOVER_DELAY` (crossing the sidebar on the
    /// way to the list shouldn't flip through every one).
    pub pending: Option<(Category, Instant)>,
}

/// How long the pointer rests on a category before it shows.
pub const HOVER_DELAY: Duration = Duration::from_millis(150);

/// What a key did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Nothing,
    Redraw,
    /// Open the n-th app of the list.
    Launch(usize),
    Close,
}

impl Drawer {
    pub fn new(anchor: Anchor, favorites: Vec<String>, recent: Vec<String>) -> Self {
        let category = if favorites.is_empty() { Category::All } else { Category::Favorites };
        Self {
            field: TextField::default(),
            category,
            selected: 0,
            scroll: 0,
            sidebar: false,
            favorites,
            recent,
            anchor,
            hover: None,
            menu: None,
            scroll_px: 0.0,
            pending: None,
        }
    }

    pub fn searching(&self) -> bool {
        !self.field.text.trim().is_empty()
    }

    /// Shows `category` from its top.
    pub fn show(&mut self, category: Category) {
        (self.category, self.selected, self.scroll, self.scroll_px, self.pending) = (category, 0, 0, 0.0, None);
        self.field = TextField::default();
    }

    /// The pointer is over `category` at `now`: shown once it has rested there; `true` when it did.
    pub fn rest_on(&mut self, category: Category, now: Instant) -> bool {
        if category == self.category {
            self.pending = None;
            return false;
        }
        let since = match self.pending {
            Some((c, since)) if c == category => since,
            _ => now,
        };
        if now.duration_since(since) >= HOVER_DELAY {
            self.show(category);
            return true;
        }
        self.pending = Some((category, since));
        false
    }

    /// Keeps the selection in view with `rows` rows showing.
    fn follow(&mut self, rows: usize) {
        self.scroll = crate::shell::emoji::scrolled(self.scroll, self.selected, rows.max(1));
    }

    /// Scrolls by `by` rows, within a list of `count` with `rows` showing.
    pub fn scroll_by(&mut self, by: isize, count: usize, rows: usize) {
        self.scroll = self.scroll.saturating_add_signed(by).min(count.saturating_sub(rows));
    }

    /// A key press: ↑/↓ (and Page Up/Down) move through the list, or the sidebar after ← or Tab; → and Tab go back to
    /// the list; Return opens the selected app; Esc clears the search, then closes; anything else types into the
    /// search. `shown`: the sidebar, `count`: the apps listed, `rows`: how many fit.
    pub fn key(&mut self, sym: Keysym, typed: Option<&str>, ctrl: bool, shown: &[Category], count: usize, rows: usize) -> Outcome {
        let step = match sym {
            Keysym::Up | Keysym::KP_Up => Some(-1),
            Keysym::Down | Keysym::KP_Down => Some(1),
            Keysym::Page_Up | Keysym::KP_Page_Up => Some(-(rows.max(1) as isize)),
            Keysym::Page_Down | Keysym::KP_Page_Down => Some(rows.max(1) as isize),
            _ => None,
        };
        let empty = self.field.text.is_empty();
        match (sym, step) {
            (_, Some(by)) if self.sidebar && !self.searching() => {
                let at = shown.iter().position(|c| *c == self.category).unwrap_or(0);
                let to = at.saturating_add_signed(by).min(shown.len().saturating_sub(1));
                if let Some(&c) = shown.get(to) {
                    self.show(c);
                }
            }
            (_, Some(by)) => {
                self.sidebar = false;
                self.selected = self.selected.saturating_add_signed(by).min(count.saturating_sub(1));
                self.follow(rows);
            }
            (Keysym::Left | Keysym::KP_Left, _) if empty => self.sidebar = true,
            (Keysym::Right | Keysym::KP_Right, _) if empty => self.sidebar = false,
            (Keysym::Tab | Keysym::ISO_Left_Tab, _) => self.sidebar = !self.sidebar && !self.searching(),
            _ => match self.field.key(sym, typed, ctrl) {
                Edit::Changed => {
                    (self.selected, self.scroll, self.sidebar) = (0, 0, false);
                }
                Edit::Moved => {}
                Edit::Ignored => return Outcome::Nothing,
                Edit::Cancel if self.field.text.is_empty() => return Outcome::Close,
                Edit::Cancel => self.field = TextField::default(),
                Edit::Submit if count > 0 => return Outcome::Launch(self.selected.min(count - 1)),
                Edit::Submit => return Outcome::Nothing,
            },
        }
        Outcome::Redraw
    }
}

/// Everything one frame of the drawer draws.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub layout: Layout,
    pub categories: Vec<Category>,
    /// The highlighted category; none while searching.
    pub current: Option<Category>,
    pub sidebar: bool,
    pub field: TextField,
    pub hover: Option<Target>,
    /// The visible apps, and whether each is a favorite.
    pub rows: Vec<(App, bool)>,
    /// The selected row among `rows`.
    pub selected: Option<usize>,
    /// Apps listed in all; the first visible one.
    pub count: usize,
    pub first: usize,
    /// Shown over an empty list.
    pub empty: &'static str,
}

/// The super-tap detector: Super pressed and released on its own (no other key, click, scroll or gesture in
/// between, no other modifier held, within `TAP`) is a tap, like a classic menu key. So Super+key shortcuts, Super+drag
/// and holding Super to read the shortcuts never open the drawer.
#[derive(Debug, Default)]
pub struct SuperTap {
    down: Option<Instant>,
    /// A tap just ended: open the drawer once the key event is done.
    pub fired: bool,
}

/// Longer than this is a hold, not a tap.
const TAP: Duration = Duration::from_millis(500);

impl SuperTap {
    /// A key went down or up. `logo`: it is Super; `alone`: no other modifier is held.
    pub fn key(&mut self, logo: bool, pressed: bool, alone: bool, now: Instant) {
        match (logo, pressed) {
            (true, true) => self.down = alone.then_some(now),
            (true, false) => self.fired = self.down.take().is_some_and(|t| now.saturating_duration_since(t) <= TAP),
            (false, true) => self.down = None,
            (false, false) => {}
        }
    }

    /// When Super went down alone, while it is still held with nothing else pressed.
    pub fn down(&self) -> Option<Instant> {
        self.down
    }

    /// A click, scroll or gesture while Super is held: not a tap.
    pub fn cancel(&mut self) {
        self.down = None;
    }
}

/// A row of an app's right-click menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Open,
    Favorite,
    Dock,
    Desktop,
}

// ponytail: no Uninstall (it needs the package manager behind an admin prompt) and no Properties / Open With; add
// rows here when Software (M7) can remove packages.
pub fn menu_rows(favorite: bool, docked: bool, desktop: bool) -> Vec<Option<Row>> {
    let row = |label: &str, command| Some(Row { sub: Vec::new(), label: label.to_owned(), shortcut: "", action: Some(Action::Drawer(command)) });
    let mut rows = vec![
        row("Open", Command::Open),
        None,
        row(if favorite { "Remove from Favorites" } else { "Add to Favorites" }, Command::Favorite),
        row(if docked { "Remove from Dock" } else { "Add to Dock" }, Command::Dock),
    ];
    if desktop {
        rows.push(row("Add to Desktop", Command::Desktop));
    }
    rows
}

impl State {
    pub fn drawer_open(&self) -> bool {
        self.drawer.is_some()
    }

    /// Opens the drawer at `anchor`, or closes it when open.
    pub fn toggle_drawer(&mut self, anchor: Anchor) {
        if !self.close_drawer() {
            self.open_drawer(anchor);
        }
    }

    pub fn open_drawer(&mut self, anchor: Anchor) {
        self.popup = None;
        self.close_spotlight();
        self.close_launchpad();
        self.emoji = None;
        // With the dock hidden there's nothing to sit above: the corner is the screen's.
        let height = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map_or(0, |g| g.size.h);
        let anchor = if anchor == Anchor::Corner && !self.dock_shown() { Anchor::Dock((0, height).into()) } else { anchor };
        self.drawer = Some(Drawer::new(anchor, load(&state_file("favorites")), load(&state_file("recent-apps"))));
        self.schedule_redraw();
    }

    /// Closes it (it fades out, see `render::DrawerView`), and its menu. `true` if it was open.
    pub fn close_drawer(&mut self) -> bool {
        let open = self.drawer.take().is_some();
        if open {
            if self.popup == Some(Popup::Menu(Menu::Drawer)) {
                self.popup = None;
            }
            self.schedule_redraw();
        }
        open
    }

    /// The apps the drawer lists now: the search's results, or the category's.
    pub fn drawer_apps(&self) -> Vec<&App> {
        let Some(d) = self.drawer.as_ref() else { return Vec::new() };
        if d.searching() { search(self.apps.list(), &d.field.text) } else { list(self.apps.list(), d.category, &d.favorites, &d.recent) }
    }

    pub fn drawer_layout(&self) -> Option<Layout> {
        let d = self.drawer.as_ref()?;
        let size = self.space.outputs().next().and_then(|o| self.space.output_geometry(o))?.size;
        Some(layout(d.anchor, size, super::dock::reserve(&self.config.dock), shown(self.apps.list()).len()))
    }

    /// What to draw this frame.
    pub fn drawer_frame(&self) -> Option<Frame> {
        let d = self.drawer.as_ref()?;
        let layout = self.drawer_layout()?;
        let apps = self.drawer_apps();
        // The list may have shrunk under the scroll (an app uninstalled, a favorite removed): never past its end.
        let first = d.scroll.min(apps.len().saturating_sub(layout.rows.len()));
        let rows: Vec<(App, bool)> = apps.iter().skip(first).take(layout.rows.len()).map(|a| ((*a).clone(), d.favorites.contains(&a.id))).collect();
        let selected = d.selected.checked_sub(first).filter(|i| *i < rows.len() && !d.sidebar);
        let empty = match (d.searching(), d.category) {
            (true, _) => "No Results",
            (false, Category::Favorites) => "Right-click an app to add it to Favorites",
            (false, Category::Recent) => "Apps you open appear here",
            _ => "No Apps",
        };
        Some(Frame {
            categories: shown(self.apps.list()),
            current: (!d.searching()).then_some(d.category),
            sidebar: d.sidebar,
            field: d.field.clone(),
            hover: d.hover,
            count: apps.len(),
            first,
            rows,
            selected,
            empty,
            layout,
        })
    }

    /// While open the drawer takes every key but bound shortcuts (see input.rs).
    pub fn drawer_key(&mut self, sym: Keysym, ctrl: bool) {
        let shown = shown(self.apps.list());
        let count = self.drawer_apps().len();
        let rows = self.drawer_layout().map_or(1, |l| l.rows.len());
        let Some(d) = self.drawer.as_mut() else { return };
        let typed = sym.key_char().map(String::from);
        match d.key(sym, typed.as_deref(), ctrl, &shown, count, rows) {
            Outcome::Nothing => {}
            Outcome::Redraw => self.schedule_redraw(),
            Outcome::Launch(i) => {
                let app = self.drawer_apps().get(i).map(|a| (*a).clone());
                self.drawer_launch(app);
            }
            Outcome::Close => {
                self.close_drawer();
            }
        }
    }

    /// Starts `app`, remembers it in Recent and closes the drawer.
    fn drawer_launch(&mut self, app: Option<App>) {
        self.close_drawer();
        if let Some(app) = app {
            tracing::info!(app = app.id, "app drawer: launch");
            if self.launch(Some(&app)) {
                self.remember_launch(&app.id);
            }
        }
    }

    /// Recent apps: `id` was just launched (from the drawer, Spotlight or Launchpad).
    pub fn remember_launch(&mut self, id: &str) {
        let path = state_file("recent-apps");
        let recent = remember(&load(&path), id);
        save(&path, &recent);
        if let Some(d) = self.drawer.as_mut() {
            d.recent = recent;
        }
    }

    /// A press at `location` (output-relative): an app opens (a right click opens its menu), a category shows, a
    /// session button acts; outside, the drawer closes. `true`: taken. With its menu open the menu gets the press.
    pub fn drawer_press(&mut self, location: Point<f64, Logical>, secondary: bool) -> bool {
        if self.drawer.is_none() || self.popup == Some(Popup::Menu(Menu::Drawer)) {
            return false;
        }
        let Some(l) = self.drawer_layout() else { return false };
        if !l.rect.to_f64().contains(location) {
            self.close_drawer();
            return true;
        }
        let shown = shown(self.apps.list());
        let Some(d) = self.drawer.as_mut() else { return true };
        match l.hit(location) {
            Some(Target::Category(i)) => {
                if let Some(&c) = shown.get(i) {
                    d.show(c);
                    d.sidebar = true;
                }
            }
            Some(Target::Row(i)) => {
                let index = d.scroll + i;
                let app = self.drawer_apps().get(index).map(|a| (*a).clone());
                match app {
                    Some(app) if secondary => {
                        let Some(d) = self.drawer.as_mut() else { return true };
                        (d.selected, d.sidebar, d.menu) = (index, false, Some((app.id.clone(), location.to_i32_round())));
                        self.popup = Some(Popup::Menu(Menu::Drawer));
                    }
                    Some(app) => self.drawer_launch(Some(app)),
                    None => {}
                }
            }
            Some(Target::Session(i)) => {
                self.close_drawer();
                self.run_action(session_action(i));
            }
            Some(Target::Field) | None => {}
        }
        self.schedule_redraw();
        true
    }

    /// The pointer moved to `to`: hovering a row selects it, hovering a category shows it.
    pub fn drawer_motion(&mut self, to: Point<f64, Logical>) {
        if self.popup == Some(Popup::Menu(Menu::Drawer)) {
            return;
        }
        let Some(l) = self.drawer_layout() else { return };
        let shown = shown(self.apps.list());
        let Some(d) = self.drawer.as_mut() else { return };
        let before = d.clone();
        d.hover = l.hit(to);
        let mut arm = false;
        match d.hover {
            Some(Target::Row(i)) => {
                (d.selected, d.sidebar, d.pending) = (d.scroll + i, false, None);
            }
            Some(Target::Category(i)) if !d.searching() => {
                if let Some(&c) = shown.get(i) {
                    arm = !d.rest_on(c, Instant::now()) && d.pending.is_some();
                }
            }
            _ => d.pending = None,
        }
        if arm {
            // Shows the category if the pointer stays put (no motion event would come to do it).
            let _ = self.loop_handle.insert_source(Timer::from_duration(HOVER_DELAY), |_, _, state| {
                state.drawer_settle();
                TimeoutAction::Drop
            });
        }
        if *d != before {
            self.schedule_redraw();
        }
    }

    /// The hover delay passed: shows the category the pointer still rests on.
    fn drawer_settle(&mut self) {
        let Some(d) = self.drawer.as_mut() else { return };
        if let Some((c, _)) = d.pending
            && d.rest_on(c, Instant::now())
        {
            self.schedule_redraw();
        }
    }

    /// Scrolling over the list: `dy` px, or `wheel` notches in v120 units (one is a row). `true`: taken.
    pub fn drawer_scroll(&mut self, location: Point<f64, Logical>, dy: f64, wheel: f64) -> bool {
        let Some(l) = self.drawer_layout().filter(|l| l.rect.to_f64().contains(location)) else { return false };
        let count = self.drawer_apps().len();
        let Some(d) = self.drawer.as_mut() else { return false };
        // A wheel notch (120 in v120 units) is one row; a finger moves by pixels.
        let row = f64::from(l.rows.first().map_or(layout::ROW, |r| r.size.h));
        d.scroll_px += if wheel != 0.0 { wheel / 120.0 * row } else { dy };
        let rows = (d.scroll_px / row).trunc();
        if rows != 0.0 {
            d.scroll_px -= rows * row;
            let before = d.scroll;
            d.scroll_by(rows as isize, count, l.rows.len());
            if d.scroll != before {
                self.schedule_redraw();
            }
        }
        true
    }

    /// The open menu's app.
    fn drawer_menu_app(&self) -> Option<App> {
        let id = &self.drawer.as_ref()?.menu.as_ref()?.0;
        self.apps.list().iter().find(|a| a.id == *id).cloned()
    }

    fn docked(&self, id: &str) -> bool {
        self.dock_pinned().iter().any(|(_, key)| key == id)
    }

    /// The right-click menu's rows (empty without one).
    pub fn drawer_menu_rows(&self) -> Vec<Option<Row>> {
        let Some(app) = self.drawer_menu_app() else { return Vec::new() };
        let favorite = self.drawer.as_ref().is_some_and(|d| d.favorites.contains(&app.id));
        let desktop = crate::shell::desktop::user_dir().is_some() && app.path.is_file();
        menu_rows(favorite, self.docked(&app.id), desktop)
    }

    /// Where the menu opens: its top-left at the click, or its bottom-left there when it wouldn't fit below.
    pub fn drawer_menu_anchor(&self, rows: &[Option<Row>]) -> Option<super::Anchor> {
        let at = self.drawer.as_ref()?.menu.as_ref()?.1;
        let height = self.space.outputs().next().and_then(|o| self.space.output_geometry(o))?.size.h;
        let menu = super::menu_height(rows);
        Some(super::Anchor::At(if at.y + menu > height - layout::GAP { (at.x, at.y - menu).into() } else { at }))
    }

    pub fn drawer_command(&mut self, command: Command) {
        let Some(app) = self.drawer_menu_app() else { return };
        match command {
            Command::Open => return self.drawer_launch(Some(app)),
            Command::Favorite => {
                let Some(d) = self.drawer.as_mut() else { return };
                d.favorites = toggle(&d.favorites, &app.id);
                save(&state_file("favorites"), &d.favorites);
            }
            Command::Dock => {
                let to = (!self.docked(&app.id)).then(|| self.dock_pinned().len());
                self.dock_repin(&app.id, to);
            }
            Command::Desktop => {
                let Some(dir) = crate::shell::desktop::user_dir() else { return };
                let Some(name) = app.path.file_name() else { return };
                let to = dir.join(name);
                // Executable: file managers only run launchers their owner marked trusted. Never over something
                // already there (the owner's own edited copy).
                let copied = std::fs::read(&app.path).and_then(|bytes| {
                    use std::{io::Write, os::unix::fs::OpenOptionsExt};
                    std::fs::File::options().write(true).create_new(true).mode(0o755).open(&to)?.write_all(&bytes)
                });
                if let Err(err) = copied {
                    tracing::warn!("app drawer: {} to {}: {err}", app.path.display(), to.display());
                }
            }
        }
        self.schedule_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, categories: &str) -> App {
        App {
            id: name.to_lowercase().replace(' ', "-"),
            name: name.into(),
            exec: name.to_lowercase().replace(' ', "-"),
            categories: categories.split(';').filter(|c| !c.is_empty()).map(String::from).collect(),
            ..Default::default()
        }
    }

    fn names(apps: &[&App]) -> Vec<String> {
        apps.iter().map(|a| a.name.clone()).collect()
    }

    #[test]
    fn categories_from_freedesktop() {
        assert_eq!(categories(&app("Kate", "Qt;Utility;TextEditor;")), [Category::Accessories]);
        assert_eq!(categories(&app("VLC", "AudioVideo;Player;Recorder;Audio;Video")), [Category::Multimedia], "once");
        assert_eq!(categories(&app("Helium", "Network;WebBrowser")), [Category::Internet]);
        assert_eq!(categories(&app("Stellarium", "Education;Science;Astronomy")), [Category::Education, Category::Science]);
        assert_eq!(categories(&app("Settings", "Settings;System")), [Category::Settings, Category::System]);
        assert_eq!(categories(&app("Odd", "X-Custom")), [Category::Other]);
        assert_eq!(categories(&app("None", "")), [Category::Other]);
        let mut hidden = app("Daemon", "Game");
        hidden.no_display = true;
        let apps = [app("Kate", "Utility"), app("Code", "Development;IDE"), app("Odd", ""), hidden];
        assert_eq!(
            shown(&apps),
            [Category::Favorites, Category::All, Category::Recent, Category::Accessories, Category::Development, Category::Other],
            "empty categories hide; hidden apps count for nothing"
        );
    }

    #[test]
    fn lists_by_category_favorites_and_recent() {
        let mut hidden = app("Hidden", "Development");
        hidden.no_display = true;
        let apps = [app("Ark", "Utility"), app("Code", "Development"), app("Kate", "Utility;Development"), hidden];
        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(names(&list(&apps, Category::All, &[], &[])), ["Ark", "Code", "Kate"]);
        assert_eq!(names(&list(&apps, Category::Development, &[], &[])), ["Code", "Kate"]);
        assert_eq!(names(&list(&apps, Category::Favorites, &ids(&["kate", "gone", "ark"]), &[])), ["Kate", "Ark"], "their order; uninstalled skipped");
        assert_eq!(names(&list(&apps, Category::Recent, &[], &ids(&["code", "hidden"]))), ["Code"]);
    }

    #[test]
    fn search_ranks_name_then_keywords_and_program() {
        let mut helium = app("Helium", "Network");
        (helium.exec, helium.generic_name) = ("helium-browser %U".into(), "Web Browser".into());
        let mut kate = app("Kate", "Utility");
        kate.keywords = vec!["editor".into(), "text".into()];
        let mut kwrite = app("KWrite", "Utility");
        kwrite.generic_name = "Text Editor".into();
        let apps = [app("Kate Sessions", ""), helium, kate, kwrite, app("Konsole", "System")];
        assert_eq!(names(&search(&apps, "kate")), ["Kate", "Kate Sessions"]);
        assert_eq!(names(&search(&apps, "browser")), ["Helium"], "the generic name's word");
        assert_eq!(names(&search(&apps, "helium-b")), ["Helium"], "the program");
        assert_eq!(names(&search(&apps, "edit")), ["Kate", "KWrite"], "keyword prefix over a generic name's word");
        assert_eq!(names(&search(&apps, "TEXT")), ["Kate", "KWrite"]);
        assert_eq!(names(&search(&apps, "k")).first().map(String::as_str), Some("Kate"), "names first");
        assert!(search(&apps, "  ").is_empty());
        assert!(rank(&apps[2], "kate").is_some_and(|k| rank(&apps[2], "editor").is_some_and(|e| e < k)));
    }

    #[test]
    fn favorites_and_recent_persist() {
        let dir = std::env::temp_dir().join(format!("noobsbot-drawer-{}", std::process::id()));
        let path = dir.join("noobsbot-desktop/favorites");
        assert!(load(&path).is_empty(), "missing: none");
        let favs = toggle(&toggle(&[], "org.kde.kate"), "helium");
        assert_eq!(favs, ["org.kde.kate", "helium"]);
        // Saves land on the config-save thread after its quiet time.
        let written = |p: &Path| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !p.is_file() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };
        save(&path, &favs);
        written(&path);
        assert_eq!(load(&path), favs);
        assert_eq!(toggle(&load(&path), "org.kde.kate"), ["helium"], "toggling again removes");
        let mut recent = Vec::new();
        for id in ["a", "b", "a", "c"] {
            recent = remember(&recent, id);
        }
        assert_eq!(recent, ["c", "a", "b"]);
        let many: Vec<String> = (0..RECENT + 5).map(|i| i.to_string()).fold(Vec::new(), |r, id| remember(&r, &id));
        assert_eq!((many.len(), many[0].as_str()), (RECENT, "16"));
        let recent_path = dir.join("noobsbot-desktop/recent-apps");
        save(&recent_path, &recent);
        written(&recent_path);
        std::fs::write(&path, "# comment\n\nkate\n  dolphin \n").unwrap();
        assert_eq!(load(&path), ["kate", "dolphin"]);
        std::fs::write(&path, b"kate\n\xff\xfe\ndolphin\n").unwrap();
        assert_eq!(load(&path).len(), 3, "a damaged line doesn't empty the list");
        assert_eq!(load(&recent_path), ["c", "a", "b"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn categories_show_after_resting() {
        let mut d = Drawer::new(Anchor::Corner, Vec::new(), Vec::new());
        let t0 = Instant::now();
        assert!(!d.rest_on(Category::Games, t0), "not yet");
        assert!(!d.rest_on(Category::Office, t0 + Duration::from_millis(100)), "moved on: the clock restarts");
        assert!(!d.rest_on(Category::Office, t0 + Duration::from_millis(200)));
        assert!(d.rest_on(Category::Office, t0 + Duration::from_millis(260)));
        assert_eq!((d.category, d.pending), (Category::Office, None));
        assert!(!d.rest_on(Category::Office, t0 + Duration::from_millis(500)), "already shown");
    }

    #[test]
    fn super_tap() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut tap = SuperTap::default();
        tap.key(true, true, true, t0);
        tap.key(true, false, true, ms(120));
        assert!(tap.fired, "press and release");
        tap.fired = false;
        // Super+A: another key in between.
        tap.key(true, true, true, t0);
        tap.key(false, true, true, ms(50));
        tap.key(false, false, true, ms(80));
        tap.key(true, false, true, ms(100));
        assert!(!tap.fired, "a shortcut");
        // Super+drag: a click in between.
        tap.key(true, true, true, t0);
        tap.cancel();
        tap.key(true, false, true, ms(100));
        assert!(!tap.fired, "a drag");
        // Held too long, with another modifier, or a release without a press.
        tap.key(true, true, true, t0);
        tap.key(true, false, true, ms(900));
        assert!(!tap.fired, "a hold");
        tap.key(true, true, false, t0);
        tap.key(true, false, true, ms(100));
        assert!(!tap.fired, "Ctrl+Super");
        tap.key(true, false, true, ms(100));
        assert!(!tap.fired, "release alone");
        // A key released (pressed before Super went down) doesn't spoil it.
        tap.key(true, true, true, t0);
        tap.key(false, false, true, ms(30));
        tap.key(true, false, true, ms(60));
        assert!(tap.fired);
    }

    #[test]
    fn keyboard_moves_through_list_and_sidebar() {
        let shown = [Category::Favorites, Category::All, Category::Recent, Category::Development];
        let mut d = Drawer::new(Anchor::Corner, Vec::new(), Vec::new());
        assert_eq!(d.category, Category::All, "no favorites: All");
        let key = |d: &mut Drawer, sym, typed: Option<&str>| d.key(sym, typed, false, &shown, 30, 8);
        assert_eq!(key(&mut d, Keysym::Down, None), Outcome::Redraw);
        assert_eq!(d.selected, 1);
        for _ in 0..10 {
            key(&mut d, Keysym::Down, None);
        }
        assert_eq!((d.selected, d.scroll), (11, 4), "scrolled to keep it in view");
        key(&mut d, Keysym::Page_Down, None);
        key(&mut d, Keysym::Page_Down, None);
        key(&mut d, Keysym::Page_Down, None);
        assert_eq!((d.selected, d.scroll), (29, 22), "clamped to the last");
        assert_eq!(key(&mut d, Keysym::Return, None), Outcome::Launch(29));
        // ← to the sidebar: ↑/↓ change the category, from the top.
        key(&mut d, Keysym::Left, None);
        assert!(d.sidebar);
        key(&mut d, Keysym::Down, None);
        assert_eq!((d.category, d.selected, d.scroll), (Category::Recent, 0, 0));
        key(&mut d, Keysym::Down, None);
        key(&mut d, Keysym::Down, None);
        assert_eq!(d.category, Category::Development, "stops at the last");
        key(&mut d, Keysym::Tab, None);
        assert!(!d.sidebar, "Tab back to the list");
        // Typing searches; Esc clears, then closes.
        assert_eq!(key(&mut d, Keysym::k, Some("k")), Outcome::Redraw);
        assert!(d.searching() && !d.sidebar);
        key(&mut d, Keysym::Left, None);
        assert!(!d.sidebar, "← moves the caret while searching");
        assert_eq!(key(&mut d, Keysym::Escape, None), Outcome::Redraw);
        assert!(!d.searching());
        assert_eq!(key(&mut d, Keysym::Escape, None), Outcome::Close);
        assert_eq!(d.key(Keysym::Return, None, false, &shown, 0, 8), Outcome::Nothing, "nothing to open");
        let mut fav = Drawer::new(Anchor::Corner, vec!["kate".into()], Vec::new());
        assert_eq!(fav.category, Category::Favorites);
        fav.scroll_by(5, 3, 8);
        assert_eq!(fav.scroll, 0, "a short list doesn't scroll");
        fav.scroll_by(5, 30, 8);
        fav.scroll_by(-2, 30, 8);
        assert_eq!(fav.scroll, 3);
    }

    #[test]
    fn layout_and_hits() {
        let size = Size::from((1920, 1080));
        let l = layout(Anchor::Corner, size, 101, 12);
        assert_eq!(l.rect, Rectangle::new((8, 1080 - 101 - 8 - 560).into(), (720, 560).into()), "bottom-left, above the dock");
        let parts = l.categories.iter().chain(&l.rows).chain(&l.session).chain([&l.field, &l.magnifier, &l.header, &l.divider, &l.logo]);
        assert!(parts.clone().all(|r| l.rect.contains_rect(*r)), "{l:?}");
        assert_eq!(l.categories.len(), 12);
        assert_eq!(l.rows.len(), 8);
        assert!(l.categories.iter().all(|c| c.loc.x + c.size.w <= l.divider.loc.x && l.header.loc.x > l.divider.loc.x));
        assert!(l.session[2].loc.y + l.session[2].size.h <= l.rect.loc.y + l.rect.size.h - layout::PAD + 1);
        assert!(!l.session[0].overlaps(l.session[1]) && !l.logo.overlaps(l.session[0]));
        let centre = |r: Rect| r.to_f64().loc + r.to_f64().size.downscale(2.0).to_point();
        assert_eq!(l.hit(centre(l.rows[3])), Some(Target::Row(3)));
        assert_eq!(l.hit(centre(l.categories[5])), Some(Target::Category(5)));
        assert_eq!(l.hit(centre(l.session[1])), Some(Target::Session(1)));
        assert_eq!(l.hit(centre(l.field)), Some(Target::Field));
        assert_eq!(l.hit(centre(l.divider)), None);
        assert_eq!(l.list(), Rectangle::new(l.header.loc, (l.header.size.w, layout::HEADER + 8 * layout::ROW).into()));
        // Over a dock icon: centred on it, kept on screen.
        let at = layout(Anchor::Dock((960, 990).into()), size, 101, 12);
        assert_eq!((at.rect.loc.x, at.rect.loc.y + at.rect.size.h), (960 - 360, 982));
        assert_eq!(layout(Anchor::Dock((30, 990).into()), size, 101, 12).rect.loc.x, 8);
        assert_eq!(layout(Anchor::Dock((1900, 990).into()), size, 101, 12).rect.loc.x, 1920 - 8 - 720);
        // The dock hidden (`open_drawer` anchors at the screen's corner): down to the edge, the dock's height not kept.
        let hidden = layout(Anchor::Dock((0, 1080).into()), size, 101, 12);
        assert_eq!((hidden.rect.loc.x, hidden.rect.loc.y + hidden.rect.size.h), (8, 1080 - 8));
        // A laptop screen: every category gets a row in the sidebar.
        for (size, dock) in [((1366, 768), 101), ((1280, 720), 150)] {
            let l = layout(Anchor::Corner, Size::from(size), dock, Category::ALL.len());
            assert_eq!(l.categories.len(), Category::ALL.len(), "{size:?}");
            assert!(
                l.categories
                    .iter()
                    .all(|c| l.rect.contains_rect(*c) && c.loc.x + c.size.w <= l.divider.loc.x && c.loc.y + c.size.h <= l.divider.loc.y + l.divider.size.h)
            );
        }
        // A short screen: it shrinks below the menu bar, with fewer rows and tighter categories; shorter still, fewer.
        let short = layout(Anchor::Corner, Size::from((1280, 600)), 101, 15);
        assert_eq!(short.rect.loc.y, menubar::height() + layout::GAP);
        assert!(short.rows.len() < 8 && short.categories.len() == 15 && short.categories[0].size.h < layout::CATEGORY);
        assert!(short.categories.iter().chain(&short.rows).all(|r| short.rect.contains_rect(*r)));
        let tiny = layout(Anchor::Corner, Size::from((1280, 400)), 101, 15);
        assert!(tiny.categories.len() < 15 && tiny.categories[0].size.h == layout::CATEGORY_MIN);
        assert!(tiny.categories.iter().all(|r| tiny.rect.contains_rect(*r)));
        // A narrow screen: as wide as fits.
        assert_eq!(layout(Anchor::Corner, Size::from((640, 800)), 101, 3).rect.size.w, 624);
    }

    #[test]
    fn layout_grows_with_text_size() {
        for (size, dock, n) in [((1920, 1080), 101, 12), ((1366, 768), 101, Category::ALL.len()), ((1280, 600), 101, 15)] {
            let at = |k| crate::text::with_scale(k, || layout(Anchor::Corner, Size::from(size), dock, n));
            assert_eq!(at(1.0), layout(Anchor::Corner, Size::from(size), dock, n), "1.0 is the default");
            let big = at(1.5);
            assert_eq!((big.field.size.h, big.header.size.h), (48, 45), "{size:?}");
            assert!(big.rows.iter().all(|r| r.size.h == 78) && !big.rows.is_empty(), "{size:?}");
            // A Body line (16 px) at 1.5 fits every sidebar row, even tightened.
            assert!(big.categories.iter().all(|c| c.size.h >= 24) && !big.categories.is_empty(), "{size:?}");
            let parts = big.categories.iter().chain(&big.rows).chain(&big.session).chain([&big.field, &big.header, &big.divider]);
            assert!(parts.clone().all(|r| big.rect.contains_rect(*r)), "{size:?} {big:?}");
            assert!(Rectangle::from_size(Size::from(size)).contains_rect(big.rect));
        }
        assert_eq!(crate::text::with_scale(1.5, || layout(Anchor::Corner, Size::from((1920, 1080)), 101, 12)).rect.size.w, 1080);
    }

    #[test]
    fn menu_rows_follow_state() {
        let labels = |rows: Vec<Option<Row>>| rows.into_iter().map(|r| r.map(|r| r.label)).collect::<Vec<_>>();
        let s = |t: &str| Some(t.to_owned());
        assert_eq!(labels(menu_rows(false, false, true)), [s("Open"), None, s("Add to Favorites"), s("Add to Dock"), s("Add to Desktop")]);
        assert_eq!(labels(menu_rows(true, true, false)), [s("Open"), None, s("Remove from Favorites"), s("Remove from Dock")]);
        assert_eq!(menu_rows(true, true, false)[2].as_ref().and_then(|r| r.action.clone()), Some(Action::Drawer(Command::Favorite)));
    }
}
