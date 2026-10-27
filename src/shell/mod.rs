//! Shell popups opened from the menu bar (E2.2): which one is open, where it sits, and what a click on the bar or in
//! a popup does. Layout lives here as plain rectangles so drawing (render/) and clicks share it.

pub mod alert;
pub mod control;
pub mod desktop;
pub mod disks;
pub mod dock;
pub mod dock_fx;
pub mod drawer;
pub mod emoji;
pub mod force_quit;
pub mod greeter;
pub mod launchpad;
pub mod lock;
pub mod notify;
pub mod osk;
pub mod plank;
pub mod screenshot;
pub mod spotlight;
pub mod switcher;
pub mod trash;

use chrono::{Datelike, Local, NaiveDate, Weekday};
use smithay::utils::{Logical, Point, Rectangle, Size};

use smithay::reexports::calloop::timer::{TimeoutAction, Timer};

use crate::{
    render::menubar::{self, BarItem},
    services::{Command, tray::MenuRow},
    shortcuts::Action,
    state::State,
    symbols::Symbol,
    text::ts,
    wm::Rect,
};

/// The on-screen display (E2.7): a symbol with a level bar (volume, brightness) or a line of text.
#[derive(Debug, Clone, PartialEq)]
pub struct Osd {
    pub symbol: Symbol,
    /// Percent.
    pub level: Option<u8>,
    /// Shown instead of the bar when there's no level.
    pub text: Option<String>,
}

/// How long the OSD stays after the last key press.
const OSD_TIME: std::time::Duration = std::time::Duration::from_millis(1500);

/// E2.7: 280 × 48 at the top right, under the bar.
pub fn osd_rect(output_width: i32) -> Rect {
    Rectangle::new(((output_width - MARGIN - 280).max(MARGIN), menubar::height() + DROP).into(), (280, 48).into())
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Popup {
    /// Notification Center (E4.2); its calendar shows (year, month 1–12).
    Notifications {
        year: i32,
        month: u32,
    },
    Menu(Menu),
    Control,
    /// One Control Center module under menu bar status item i: Wi-Fi, Bluetooth or Sound.
    Module(control::Module, usize),
}

impl Popup {
    /// The bar item it hangs from, which shows the pressed pill; the dock's menu hangs from no item.
    pub fn item(self) -> Option<BarItem> {
        Some(match self {
            Popup::Notifications { .. } => BarItem::Clock,
            Popup::Menu(Menu::Logo) => BarItem::Logo,
            Popup::Menu(Menu::App) => BarItem::App,
            Popup::Menu(Menu::Tray(i)) => BarItem::Tray(i),
            Popup::Menu(Menu::Overflow) => BarItem::Overflow,
            Popup::Menu(Menu::Input) => BarItem::Input,
            Popup::Menu(Menu::Battery(i)) | Popup::Module(_, i) => BarItem::Status(i),
            Popup::Menu(Menu::Dock | Menu::Desktop | Menu::Drawer | Menu::Window) => return None,
            Popup::Control => BarItem::ControlCenter,
        })
    }
}

/// How long a submenu stays open while the pointer heads for it across other rows of its menu.
// ponytail: a fixed grace while the pointer aims at it; a speed-aware one if it feels sticky.
const AIM_GRACE: std::time::Duration = std::time::Duration::from_millis(300);
/// How long the pointer rests on a row before its submenu opens (or the open one closes); a click opens it at once.
const OPEN_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

/// The rows of the submenu at the end of `path` (the row of `rows` whose submenu is open, then that submenu's, …);
/// an empty path: `rows`.
pub fn sub_rows<'a>(rows: &'a [Option<Row>], path: &[usize]) -> &'a [Option<Row>] {
    path.iter().try_fold(rows, |rows, &i| Some(&rows.get(i)?.as_ref()?.sub[..])).unwrap_or_default()
}

/// Fits `popup`'s open submenus and keyboard row to its `rows` as rebuilt: the path ends before the first row that is
/// gone or has no submenu any more, and a keyboard row past its level's end goes to the level's last live row.
pub fn fit_menu(rows: &[Option<Row>], popup: Option<Popup>, submenu: &mut Option<(Popup, Vec<usize>)>, cursor: &mut Option<(Popup, usize, usize)>) {
    let path = match submenu {
        Some((p, path)) if Some(*p) == popup => {
            let opens = |d: usize| sub_rows(rows, &path[..d]).get(path[d]).and_then(Option::as_ref).is_some_and(|r| !r.sub.is_empty());
            let keep = (0..path.len()).take_while(|&d| opens(d)).count();
            path.truncate(keep);
            &path[..]
        }
        _ => &[],
    };
    if let Some((p, d, i)) = *cursor
        && Some(p) == popup
    {
        let level = path.get(..d).map(|at| sub_rows(rows, at));
        if level.is_none_or(|level| i >= level.len()) {
            *cursor = level.and_then(|level| menu_step(level, None, false)).map(|j| (p, d, j));
        }
    }
}

/// The tray menu row at the end of `path` (as `sub_rows`).
fn tray_row<'a>(rows: &'a [Option<MenuRow>], path: &[usize]) -> Option<&'a MenuRow> {
    let (&last, init) = path.split_last()?;
    let rows = init.iter().try_fold(rows, |rows, &i| rows.get(i)?.as_ref()?.submenu.as_deref())?;
    rows.get(last)?.as_ref()
}

/// The tray menu row with dbusmenu id `id`, at any depth.
fn tray_find(rows: &mut [Option<MenuRow>], id: i32) -> Option<&mut MenuRow> {
    rows.iter_mut().flatten().find_map(|r| if r.id == id { Some(r) } else { tray_find(r.submenu.as_deref_mut()?, id) })
}

/// The pointer going from `from` to `to` heads for submenu `sub`: `to` is in the triangle between `from` and the
/// submenu's near edge, so a diagonal move across the parent's other rows keeps it open.
pub fn aims_at(from: Point<f64, Logical>, to: Point<f64, Logical>, sub: Rect) -> bool {
    let sub = sub.to_f64();
    let x = if sub.loc.x >= from.x { sub.loc.x } else { sub.loc.x + sub.size.w };
    let (a, b) = (Point::from((x, sub.loc.y)), Point::from((x, sub.loc.y + sub.size.h)));
    let side = |p: Point<f64, Logical>, q: Point<f64, Logical>| (q.x - p.x) * (to.y - p.y) - (q.y - p.y) * (to.x - p.x);
    let (d1, d2, d3) = (side(from, a), side(a, b), side(b, from));
    to != from && !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
}

/// What a click on status item `i` of `kind` opens: its Control Center module, or the battery menu. The recording's
/// item opens nothing: it stops the recording.
pub fn status_popup(kind: menubar::Kind, i: usize) -> Option<Popup> {
    Some(match kind {
        menubar::Kind::Recording => return None,
        menubar::Kind::Network => Popup::Module(control::Module::Wifi, i),
        menubar::Kind::Bluetooth => Popup::Module(control::Module::Bluetooth, i),
        menubar::Kind::Volume => Popup::Module(control::Module::Sound, i),
        menubar::Kind::Battery => Popup::Menu(Menu::Battery(i)),
    })
}

/// What a click on `item` opens; a status item's needs its kind (`State::bar_popup`).
fn popup_for(item: BarItem) -> Option<Popup> {
    match item {
        BarItem::Logo => Some(Popup::Menu(Menu::Logo)),
        BarItem::App => Some(Popup::Menu(Menu::App)),
        BarItem::Input => Some(Popup::Menu(Menu::Input)),
        BarItem::Clock => {
            let (year, month) = this_month();
            Some(Popup::Notifications { year, month })
        }
        BarItem::ControlCenter => Some(Popup::Control),
        BarItem::Overflow => Some(Popup::Menu(Menu::Overflow)),
        // Its menu comes over D-Bus first: see `tray_press`.
        BarItem::Status(_) | BarItem::Tray(_) | BarItem::Space => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Menu {
    Logo,
    App,
    /// `State::tray[i]`'s dbusmenu, rows in `State::tray_menu`.
    Tray(usize),
    /// The tray items that didn't fit the bar: `State::overflow_menu_rows`.
    Overflow,
    /// A dock icon's right-click menu: `State::dock_menu_rows`, above `State::dock_ui.menu`.
    Dock,
    /// Keyboard layouts and input methods (E6.12): `State::input_menu_rows`.
    Input,
    /// The battery's menu, from `Content::status[i]`: `State::battery_menu_rows`.
    Battery(usize),
    /// A desktop icon's or the desktop's right-click menu (E7.2): `State::desktop_menu_rows`.
    Desktop,
    /// An app's right-click menu in the app drawer: `State::drawer_menu_rows`.
    Drawer,
    /// A title bar's right-click menu (E8.5): `State::window_menu_rows`.
    Window,
}

/// Where a menu opens: under the bar with its left edge at x, centred over (x, bottom) like the dock's, or with its
/// top-left corner at a point (a right click on the desktop), or as a submenu beside a row: the rect spans the parent
/// menu's width at the row's height, `true` when the parent itself opened to the left (`submenu_x`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Anchor {
    Below(i32),
    Above(Point<i32, Logical>),
    At(Point<i32, Logical>),
    Beside(Rect, bool),
}

/// A menu row (D8); `None` in a row list is a separator. No action: drawn disabled.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub label: String,
    /// Modifier glyphs; ⌘ is the desktop key.
    pub shortcut: &'static str,
    pub action: Option<Action>,
    /// A submenu's rows: non-empty makes the row open them to the side instead of running an action.
    pub sub: Vec<Option<Row>>,
}

impl Row {
    /// The keyboard and the hover pill land on it: it runs something or opens a submenu.
    pub fn live(&self) -> bool {
        self.action.is_some() || !self.sub.is_empty()
    }
}

/// Up (`down` false) or Down from row `from` (`None`: from outside the menu, so the first or last): the next live
/// row, past separators and disabled rows; at either end it stays.
pub fn menu_step(rows: &[Option<Row>], from: Option<usize>, down: bool) -> Option<usize> {
    let live = |i: &usize| rows.get(*i).and_then(Option::as_ref).is_some_and(Row::live);
    match (from, down) {
        (None, true) => (0..rows.len()).find(live),
        (None, false) => (0..rows.len()).rev().find(live),
        (Some(i), true) => (i + 1..rows.len()).find(live).or(Some(i)),
        (Some(i), false) => (0..i).rev().find(live).or(Some(i)),
    }
}

/// A typed letter: the next live row after `from`, going round, whose label starts with `c` (any case).
pub fn menu_jump(rows: &[Option<Row>], from: Option<usize>, c: char) -> Option<usize> {
    let start = from.map_or(0, |i| i + 1);
    let starts = |r: &Row| r.label.trim_start().chars().next().is_some_and(|f| f.to_lowercase().eq(c.to_lowercase()));
    (0..rows.len()).map(|k| (start + k) % rows.len()).find(|&i| rows[i].as_ref().is_some_and(|r| r.live() && starts(r)))
}

/// Left or Right from bar item `open` among `items` (left to right): the neighbouring one that opens a menu or a
/// status module, going round; `opens(item)`: a tray or status item has one.
pub fn bar_step(items: &[BarItem], open: BarItem, right: bool, opens: impl Fn(BarItem) -> bool) -> Option<BarItem> {
    let menus: Vec<_> = items
        .iter()
        .copied()
        .filter(|item| match item {
            BarItem::Tray(_) | BarItem::Status(_) => opens(*item),
            _ => matches!(popup_for(*item), Some(Popup::Menu(_))),
        })
        .collect();
    let at = menus.iter().position(|i| *i == open)?;
    let n = menus.len();
    Some(menus[if right { (at + 1) % n } else { (at + n - 1) % n }])
}

/// The rows of `menu`; `app` is the focused app's name, `tray` the open tray menu's rows, `update`: the pending
/// updates' count and whether a verified one is waiting (E6.13), `recent` the Recent Items submenu (`recent_rows`).
pub fn menu_rows(menu: Menu, app: &str, tray: &[Option<MenuRow>], update: (usize, bool), recent: &[Option<Row>]) -> Vec<Option<Row>> {
    let row = |label: String, shortcut, action| Some(Row { sub: Vec::new(), label, shortcut, action });
    let settings = |page: &str| Some(Action::Spawn(format!("noobsbot-desktop-settings --page {page}")));
    match menu {
        Menu::Logo => vec![
            row("About This Computer".into(), "", settings("about")),
            row("System Settings…".into(), "", settings("appearance")),
            // The pending count in the shortcut column (E6.13); a verified download installs from here.
            if update.1 {
                row("Restart to Update…".into(), ["", "1", "2"][update.0.min(2)], Some(Action::InstallUpdate))
            } else {
                row("Software Update…".into(), ["", "1", "2"][update.0.min(2)], settings("general"))
            },
            None,
            Some(Row { label: "Recent Items".into(), shortcut: "", action: None, sub: recent.to_vec() }),
            None,
            row("Force Quit…".into(), "⌥⌘⎋", Some(Action::ForceQuit)),
            None,
            row("Sleep".into(), "", Some(Action::Sleep)),
            row("Restart…".into(), "", Some(Action::Power(alert::Power::Restart))),
            row("Shut Down…".into(), "", Some(Action::Power(alert::Power::ShutDown))),
            None,
            row("Lock Screen".into(), "⌃⌘Q", Some(Action::Lock)),
            row("Log Out…".into(), "", Some(Action::Power(alert::Power::LogOut))),
        ],
        Menu::App => vec![
            row(format!("About {app}"), "", None),
            None,
            row(format!("Hide {app}"), "⌘H", Some(Action::Hide)),
            row("Hide Others".into(), "⌥⌘H", Some(Action::HideOthers)),
            row("Show All".into(), "", Some(Action::ShowAll)),
            None,
            row(format!("Quit {app}"), "⌘Q", Some(Action::QuitApp)),
        ],
        Menu::Tray(_) => tray_rows(tray),
        // Built from state: `State::dock_menu_rows`, `State::input_menu_rows`, `State::desktop_menu_rows`.
        Menu::Dock | Menu::Input | Menu::Desktop | Menu::Drawer | Menu::Window | Menu::Overflow | Menu::Battery(_) => Vec::new(),
    }
}

/// The logo menu's Recent Items: recent `apps` (name, desktop-file id) and `docs` (name, URI), then Clear Menu.
pub fn recent_rows(apps: &[(String, String)], docs: &[(String, String)]) -> Vec<Option<Row>> {
    let row = |label: &str, action| Some(Row { label: label.to_owned(), shortcut: "", action, sub: Vec::new() });
    let mut rows = vec![row("Applications", None)];
    rows.extend(apps.iter().map(|(name, id)| row(name, Some(Action::Launch(id.clone())))));
    rows.extend([None, row("Documents", None)]);
    rows.extend(docs.iter().map(|(name, uri)| row(name, Some(Action::Spawn(format!("xdg-open {uri}"))))));
    let empty = apps.is_empty() && docs.is_empty();
    rows.extend([None, row("Clear Menu", (!empty).then_some(Action::ClearRecent))]);
    rows
}

/// A dbusmenu's rows, one per entry, submenus nested; an empty submenu shows "Empty".
fn tray_rows(tray: &[Option<MenuRow>]) -> Vec<Option<Row>> {
    let row = |r: &MenuRow| {
        let action = (r.enabled && r.submenu.is_none()).then_some(Action::TrayMenu(r.id));
        let mut sub = r.submenu.as_deref().map(tray_rows).unwrap_or_default();
        if r.submenu.is_some() && sub.is_empty() {
            sub.push(Some(Row { label: "Empty".into(), shortcut: "", action: None, sub: Vec::new() }));
        }
        Row { label: r.label.clone(), shortcut: "", action, sub }
    };
    tray.iter().map(|r| r.as_ref().map(row)).collect()
}

/// A menu's height with `rows`.
pub fn menu_height(rows: &[Option<Row>]) -> i32 {
    2 * menu::PAD + rows.iter().map(|r| if r.is_some() { ts(menu::ROW) } else { menu::SEPARATOR }).sum::<i32>()
}

/// Menus (D8), in logical px; `ROW`, `SIDE` and `MIN_WIDTH` grow with the text size (`text::ts`).
pub mod menu {
    pub const PAD: i32 = 5;
    pub const ROW: i32 = 22;
    /// 1 px line with 5 px above and below.
    pub const SEPARATOR: i32 = 11;
    pub const SIDE: i32 = 12;
    pub const MIN_WIDTH: i32 = 180;
}

/// A `width`-wide menu at `anchor`, kept on screen; its rect and one rect per row. A menu taller than the screen
/// shows whole rows from the `scroll`th (clamped so the last row can show) and gives the rest zero-height rects,
/// which are neither drawn nor hit.
pub fn menu_layout(anchor: Anchor, output: Size<i32, Logical>, width: i32, rows: &[Option<Row>], scroll: usize) -> (Rect, Vec<Rect>) {
    use menu::*;
    let row_h = |row: &Option<Row>| if row.is_some() { ts(ROW) } else { SEPARATOR };
    let height = menu_height(rows);
    let (left, top) = match anchor {
        Anchor::Below(x) => (x, menubar::height() + DROP),
        Anchor::Above(p) => (p.x - width / 2, p.y - DROP - height),
        Anchor::At(p) => (p.x, p.y),
        // Its first row level with the parent's row; clamped onto the screen below.
        Anchor::Beside(r, left) => (submenu_x(r, width, output.w, left), r.loc.y - PAD),
    };
    let x = left.min(output.w - MARGIN - width).max(MARGIN);
    let top = top.min(output.h - MARGIN - height).max(menubar::height() + DROP);
    let room = output.h - MARGIN - top - 2 * PAD;
    // The first row that still lets the last one show.
    let mut tail = 0;
    let first = rows.iter().rposition(|row| {
        tail += row_h(row);
        tail > room
    });
    let scroll = scroll.min(first.map_or(0, |i| i + 1));
    let mut y = top + PAD;
    let rects = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let h = row_h(row);
            let shown = i >= scroll && y + h <= top + PAD + room;
            let rect = Rectangle::new((x + PAD, y).into(), (width - 2 * PAD, if shown { h } else { 0 }).into());
            if shown {
                y += h;
            }
            rect
        })
        .collect();
    (Rectangle::new((x, top).into(), (width, y + PAD - top).into()), rects)
}

/// A `width`-wide submenu's left edge beside parent `row` (spanning the parent's width): the way its parent went
/// (`left`: to the left, else right), or the other way when that runs off a `screen`-wide display.
pub fn submenu_x(row: Rect, width: i32, screen: i32, left: bool) -> i32 {
    let (right_x, left_x) = (row.loc.x + row.size.w, row.loc.x - width);
    let (fits_right, fits_left) = (right_x + width <= screen - MARGIN, left_x >= MARGIN);
    if (left && fits_left) || (!left && !fits_right && fits_left) { left_x } else { right_x }
}

/// Popups hang this far below the bar and keep this far from the screen's edge.
const DROP: i32 = 4;
const MARGIN: i32 = 8;

/// The calendar (E2.5), in logical px relative to its top-left corner.
pub mod calendar {
    pub const WIDTH: i32 = 280;
    pub const PAD: i32 = 16;
    pub const TITLE_Y: i32 = 12;
    pub const TITLE_H: i32 = 20;
    pub const ARROW: i32 = 24;
    pub const HEADER_Y: i32 = 42;
    pub const HEADER_H: i32 = 14;
    pub const GRID_Y: i32 = 60;
    pub const CELL_W: i32 = 36;
    pub const CELL_H: i32 = 32;
    pub const BOTTOM: i32 = 8;
    /// The grid is centred: 7 cells of 36 px in 280.
    pub const GRID_X: i32 = (WIDTH - 7 * CELL_W) / 2;
    /// With week numbers: a column this wide at the padding, and narrower cells beside it.
    pub const WEEK_W: i32 = 24;
    pub const WEEK_CELL_W: i32 = (WIDTH - 2 * PAD - WEEK_W) / 7;

    /// The card's height with `rows` weeks; rows grow with the text size, the width stays.
    pub fn height(rows: usize) -> i32 {
        use crate::text::ts;
        ts(GRID_Y) + rows as i32 * ts(CELL_H) + BOTTOM
    }
}

/// Where each part of the calendar is, output-relative.
#[derive(Debug, Clone, PartialEq)]
pub struct CalendarLayout {
    pub rect: Rect,
    pub title: Rect,
    pub prev: Rect,
    pub next: Rect,
    /// Column headers, then one rect per grid cell, row by row.
    pub header: [Rect; 7],
    pub cells: Vec<Rect>,
    /// One week-number rect per row, at the left; empty when they are off.
    pub weeks: Vec<Rect>,
}

/// The calendar with its top-left corner at (`x`, `y`), output-relative, with a week-number column when `weeks`.
pub fn calendar_layout(x: i32, y: i32, rows: usize, weeks: bool) -> CalendarLayout {
    use crate::text::ts;
    use calendar::*;
    let at = |dx: i32, dy: i32, w: i32, h: i32| Rectangle::new((x + dx, y + dy).into(), (w, h).into());
    // The arrows are icons: their size stays, they centre on the taller title row.
    let arrow_y = ts(TITLE_Y) + (ts(TITLE_H) - ARROW) / 2;
    let next = at(WIDTH - PAD + 4 - ARROW, arrow_y, ARROW, ARROW);
    let prev = at(WIDTH - PAD + 4 - 2 * ARROW, arrow_y, ARROW, ARROW);
    let (grid_y, cell_h) = (ts(GRID_Y), ts(CELL_H));
    let (grid_x, cell_w) = if weeks { (PAD + WEEK_W, WEEK_CELL_W) } else { (GRID_X, CELL_W) };
    CalendarLayout {
        rect: at(0, 0, WIDTH, height(rows)),
        title: at(PAD, ts(TITLE_Y), WIDTH - 2 * PAD - 2 * ARROW, ts(TITLE_H)),
        prev,
        next,
        header: std::array::from_fn(|i| at(grid_x + i as i32 * cell_w, ts(HEADER_Y), cell_w, ts(HEADER_H))),
        cells: (0..rows * 7).map(|i| at(grid_x + (i % 7) as i32 * cell_w, grid_y + (i / 7) as i32 * cell_h, cell_w, cell_h)).collect(),
        weeks: (0..rows).filter(|_| weeks).map(|row| at(PAD, grid_y + row as i32 * cell_h, WEEK_W, cell_h)).collect(),
    }
}

/// The month after (`by` = 1) or before (`by` = -1) `(year, month)`.
pub fn shift_month(year: i32, month: u32, by: i32) -> (i32, u32) {
    let index = year * 12 + month as i32 - 1 + by;
    (index.div_euclid(12), index.rem_euclid(12) as u32 + 1)
}

pub fn this_month() -> (i32, u32) {
    let today = Local::now().date_naive();
    (today.year(), today.month())
}

/// The grid of the popup's month; empty for a year chrono can't represent.
pub fn grid(year: i32, month: u32, week_start: Weekday) -> Vec<[NaiveDate; 7]> {
    crate::calendar::month_grid(year, month, week_start).unwrap_or_default()
}

/// `location` relative to a display at `from` made relative to one at `to` (global origins).
pub fn rebase(location: Point<f64, Logical>, from: Point<i32, Logical>, to: Point<i32, Logical>) -> Point<f64, Logical> {
    location + from.to_f64() - to.to_f64()
}

impl State {
    /// The first display's area, global: the shell's coordinates are relative to its top-left corner.
    pub fn first_geometry(&self) -> Option<Rectangle<i32, Logical>> {
        self.space.outputs().next().and_then(|o| self.space.output_geometry(o))
    }

    /// The first display's top-left corner, global; the origin when there is no display.
    pub fn first_origin(&self) -> Point<i32, Logical> {
        self.first_geometry().unwrap_or_default().loc
    }

    /// The display an overlay remembered (`output`), while it is still connected; else the first.
    pub fn overlay_output(&self, output: Option<&smithay::output::Output>) -> Option<smithay::output::Output> {
        output.filter(|o| self.space.outputs().any(|x| x == *o)).or_else(|| self.space.outputs().next()).cloned()
    }

    /// `overlay_output`'s area, global.
    pub fn overlay_geometry(&self, output: Option<&smithay::output::Output>) -> Option<Rectangle<i32, Logical>> {
        self.overlay_output(output).and_then(|o| self.space.output_geometry(&o))
    }

    /// `location` (first-output-relative, as the shell gets it) relative to `overlay_output(output)`.
    pub fn to_display(&self, output: Option<&smithay::output::Output>, location: Point<f64, Logical>) -> Point<f64, Logical> {
        let origin = |o| self.overlay_geometry(o).unwrap_or_default().loc;
        rebase(location, origin(None), origin(output))
    }

    /// `location` relative to `overlay_output(output)` made first-output-relative again.
    pub fn display_to_output(&self, output: Option<&smithay::output::Output>, location: Point<f64, Logical>) -> Point<f64, Logical> {
        let origin = |o| self.overlay_geometry(o).unwrap_or_default().loc;
        rebase(location, origin(output), origin(None))
    }

    /// The focused window's display, else the pointer's (D13): where alerts and Spotlight show.
    pub fn focus_output(&self) -> Option<smithay::output::Output> {
        let window = self.focused_window().and_then(|w| self.space.element_geometry(&w));
        self.display_with(window.map(|g| g.loc + Point::from((g.size.w / 2, g.size.h / 2))))
    }

    /// `location` relative to the first output's top-left corner, where the shell lays out.
    pub fn to_output(&self, location: Point<f64, Logical>) -> Point<f64, Logical> {
        location - self.first_origin().to_f64()
    }

    /// The display the open popup shows on: a bar popup on its bar's (`bar_output`), the dock's menu on its dock's
    /// (`dock_output`), the app drawer's menu on the drawer's, the others on the first.
    pub fn popup_output(&self) -> Option<smithay::output::Output> {
        if self.popup == Some(Popup::Menu(Menu::Dock)) {
            return self.dock_output();
        }
        if self.popup == Some(Popup::Menu(Menu::Drawer)) {
            return self.drawer_output();
        }
        let first = self.space.outputs().next();
        let bar = self.bar_output.as_ref().filter(|o| self.space.outputs().any(|x| x == *o));
        if self.popup.is_some_and(|p| p.item().is_none()) { first } else { bar.or(first) }.cloned()
    }

    /// The size of the popup's display, which Notification Center and Control Center are laid out in.
    pub fn popup_size(&self) -> Size<i32, Logical> {
        self.popup_output().and_then(|o| self.space.output_geometry(&o)).map_or_else(Default::default, |g| g.size)
    }

    /// `location` relative to the first output (as the shell gets it) made relative to the popup's display.
    pub fn to_popup(&self, location: Point<f64, Logical>) -> Point<f64, Logical> {
        self.to_display(self.popup_output().as_ref(), location)
    }

    /// `output`'s menu bar view: the first display's is `menu_bar`, the others' are in `bars`.
    pub fn bar_view(&self, output: &smithay::output::Output) -> Option<&menubar::MenuBar> {
        let b = self.backend.as_ref()?;
        if self.space.outputs().next() == Some(output) { Some(&b.menu_bar) } else { b.bars.get(&output.name()) }
    }

    /// The display whose menu bar is under `location` (first-output-relative), with `location` relative to it.
    pub fn menu_bar_at(&self, location: Point<f64, Logical>) -> Option<(smithay::output::Output, Point<f64, Logical>)> {
        let at = location + self.first_geometry()?.loc.to_f64();
        let (output, local) = self.space.outputs().find_map(|o| Some((o.clone(), menubar::on_bar(self.space.output_geometry(o)?, at)?)))?;
        self.bar_view(&output).is_some_and(|b| b.visible()).then_some((output, local))
    }

    /// The wheel over an open menu scrolls it when it is taller than the screen. `true`: the menu took it.
    pub fn menu_scroll(&mut self, location: Point<f64, Logical>, dy: f64) -> bool {
        let location = self.to_popup(location);
        if !matches!(self.popup, Some(Popup::Menu(_))) || !self.popup_rect().is_some_and(|r| r.to_f64().contains(location)) {
            return false;
        }
        if self.backend.as_mut().is_some_and(|b| b.menu.scroll_by(dy)) {
            self.schedule_redraw();
        }
        true
    }

    /// The open popup's area, output-relative.
    pub fn popup_rect(&self) -> Option<Rect> {
        match self.popup? {
            Popup::Notifications { .. } => self.center_layout().map(|c| c.rect),
            // A menu's width follows its text, so the last frame's layout is the one to hit.
            Popup::Menu(_) => self.backend.as_ref()?.menu.rect(),
            Popup::Control | Popup::Module(..) => Some(self.control_layout().rect),
        }
    }

    /// The open popup or one of its open submenus is under `location` (first-output-relative).
    pub fn in_popup(&self, location: Point<f64, Logical>) -> bool {
        let location = self.to_popup(location);
        let subs = (1..=self.open_path().len()).filter_map(|d| self.level_view(d)?.rect());
        self.popup_rect().into_iter().chain(subs).any(|r| r.to_f64().contains(location))
    }

    /// Whether the shell, not an app, owns the pointer at `location` (output-relative): the bar, an open popup or the dock.
    pub fn shell_covers(&self, location: Point<f64, Logical>) -> bool {
        let on_alert = self.alert.is_some()
            && self.backend.as_ref().and_then(|b| b.alert.rect()).is_some_and(|r| r.to_f64().contains(self.to_display(self.focus_output().as_ref(), location)));
        if on_alert || self.on_banner(location) || self.mission_open() || self.osk_panel().is_some_and(|r| r.to_f64().contains(location)) {
            return true;
        }
        let on_spotlight = self.spotlight.as_ref().map(|s| self.to_display(s.output.as_ref(), location));
        if on_spotlight.is_some_and(|at| self.spotlight_layout(self.spotlight_results().len()).is_some_and(|l| l.rect.to_f64().contains(at))) {
            return true;
        }
        if self.emoji.as_ref().is_some_and(|e| e.layout().rect.to_f64().contains(location)) {
            return true;
        }
        if self.force_quit.is_some() && self.force_quit_layout(0).is_some_and(|l| l.rect.to_f64().contains(location)) {
            return true;
        }
        if self.launchpad_open() || self.drawer_layout().is_some_and(|l| l.rect.to_f64().contains(self.to_drawer(location))) {
            return true;
        }
        // The screenshot thumbnail, unless it is being dragged (then the app under it is the drop target).
        let on_thumbnail = !self.screenshots.dragging
            && self.thumbnail_rect().is_some_and(|r| r.to_f64().contains(self.to_display(self.thumbnail_output().as_ref(), location)));
        on_thumbnail || self.menu_bar_at(location).is_some() || self.in_popup(location) || self.dock_covers(self.to_dock(location))
    }

    /// A press at `location` (output-relative); `secondary`: the right button. `true`: the shell took it, so no
    /// window gets focus or the click.
    pub fn shell_press(&mut self, location: Point<f64, Logical>, secondary: bool) -> bool {
        // The alert is above everything else.
        if self.alert.is_some() && self.alert_press(self.to_display(self.focus_output().as_ref(), location)) {
            return true;
        }
        if self.banner_press(location)
            || self.force_quit_press(location)
            || self.spotlight_press(location)
            || self.emoji_press(location)
            || self.launchpad_press(location, secondary)
        {
            return true;
        }
        // A click hands the menus back to the pointer.
        self.menu_cursor = None;
        // Any display's bar: its popups open on that display.
        let bar = self.menu_bar_at(location);
        let item = bar.as_ref().and_then(|(output, at)| self.bar_view(output)?.hit(*at));
        let open = self.popup;
        let in_popup = self.in_popup(location);
        let taken = match (open, item) {
            (Some(popup), _) if in_popup => {
                self.popup_press(popup, self.to_popup(location));
                true
            }
            // Its own item again closes the popup.
            (_, Some(item)) => {
                let output = bar.map(|(output, _)| output);
                let again = open.and_then(Popup::item) == Some(item) && self.popup_output() == output;
                self.bar_output = output;
                if again {
                    self.popup = None;
                } else {
                    self.open_bar_item(item, location, secondary);
                }
                true
            }
            // Outside: closes the popup and the click goes no further.
            (Some(_), None) => {
                self.popup = None;
                true
            }
            (None, None) => self.shell_covers(location),
        };
        if self.popup != open {
            self.submenu = None;
            self.schedule_redraw();
        }
        taken
    }

    /// What a click on bar `item` opens.
    fn bar_popup(&self, item: BarItem) -> Option<Popup> {
        match item {
            BarItem::Status(i) => status_popup(self.menu_bar()?.status.get(i)?.kind, i),
            _ => popup_for(item),
        }
    }

    /// Opens bar `item`'s popup; a tray item's menu is asked for first (`tray_press`).
    fn open_bar_item(&mut self, item: BarItem, location: Point<f64, Logical>, secondary: bool) {
        self.popup = self.bar_popup(item);
        match item {
            BarItem::Tray(i) => self.tray_press(i, location, secondary),
            // fcitx5 tells nobody when its method changes: ask now.
            BarItem::Input => self.services.send(crate::services::Command::ImeRead { ibus: self.config.keyboard.engine() == "ibus" }),
            BarItem::ControlCenter => self.services.send(crate::services::Command::Inhibitors),
            BarItem::Status(i) if self.menu_bar().and_then(|c| c.status.get(i).map(|s| s.kind)) == Some(menubar::Kind::Recording) => {
                self.toggle_recording();
            }
            BarItem::Status(_) => self.module_opened(),
            BarItem::Space => self.run_action(crate::shortcuts::Action::MissionControl),
            _ => {}
        }
    }

    /// A status item's module just opened, by click or sliding along the bar: its list starts at the top, and Wi-Fi
    /// scans (NetworkManager refuses scans too close together).
    fn module_opened(&mut self) {
        self.control_scroll = 0.0;
        if matches!(self.popup, Some(Popup::Module(control::Module::Wifi, _))) {
            self.services.send(crate::services::Command::WifiScan);
        }
    }

    /// A press at `location`, relative to the popup's display.
    fn popup_press(&mut self, popup: Popup, location: Point<f64, Logical>) {
        match popup {
            Popup::Notifications { .. } => self.center_press(location),
            Popup::Menu(menu) => {
                let rows = self.open_menu_rows(menu);
                let Some((d, Some(i))) = self.menu_level_at(location) else { return };
                let row = sub_rows(&rows, &self.open_path()[..d]).get(i).cloned().flatten();
                // Separators and disabled rows keep the menu open; a submenu row opens its submenu at once.
                if let Some(action) = row.and_then(|row| row.action) {
                    self.popup = None;
                    self.run_action(action);
                } else {
                    self.menu_hover(menu, d, i);
                }
            }
            Popup::Control | Popup::Module(..) => self.control_press(location),
        }
    }

    /// The pointer moved from `from` to `to` (output-relative). With a popup open, sliding along the bar opens the
    /// next item's popup; in a menu, the hovered row follows.
    pub fn shell_motion(&mut self, from: Point<f64, Logical>, to: Point<f64, Logical>) {
        if self.shell_drag.is_some() {
            return self.control_drag(self.to_popup(to));
        }
        self.notify_motion(to);
        self.drawer_motion(to);
        self.launchpad_motion(to);
        let Some(open) = self.popup else { return };
        let (from, to) = (self.to_popup(from), self.to_popup(to));
        let bar = self.popup_output().and_then(|o| self.bar_view(&o));
        if let Some(next) = bar.and_then(|b| b.hit(to)).filter(|item| open.item().is_some_and(|o| o != *item)).and_then(|item| self.bar_popup(item)) {
            self.popup = Some(next);
            if matches!(next, Popup::Module(..)) {
                self.module_opened();
            }
            self.schedule_redraw();
        } else if let Popup::Menu(_) = open {
            let (here, before) = (self.menu_level_at(to), self.menu_level_at(from));
            // Heading for an open submenu across other rows of its parent keeps it open a moment.
            let path = self.open_path();
            let aiming = here.is_some_and(|(d, row)| {
                path.get(d).is_some_and(|&o| row != Some(o)) && self.level_view(d + 1).and_then(|v| v.rect()).is_some_and(|r| aims_at(from, to, r))
            });
            match here {
                Some((_, Some(_))) if aiming => self.hover_later(AIM_GRACE, false),
                // Resting on a row opens its submenu or closes the open one (`menu_hover`).
                Some((_, Some(_))) if here != before => self.hover_later(OPEN_DELAY, true),
                _ => {}
            }
            if here != before {
                self.menu_cursor = None;
                self.schedule_redraw();
            }
        }
    }

    /// The rows of the open `menu`; its open submenus and keyboard row are fitted to them (`fit_menu`).
    pub fn open_menu_rows(&mut self, menu: Menu) -> Vec<Option<Row>> {
        let tray = self.tray_menu.as_ref().map_or(&[][..], |(_, _, rows)| rows);
        let rows = match menu {
            Menu::Dock => self.dock_menu_rows(),
            Menu::Input => self.input_menu_rows(),
            Menu::Desktop => self.desktop_menu_rows(),
            Menu::Drawer => self.drawer_menu_rows(),
            Menu::Window => self.window_menu_rows(),
            Menu::Overflow => self.overflow_menu_rows(),
            Menu::Battery(_) => self.battery_menu_rows(),
            _ => menu_rows(menu, "", tray, (self.update_count(), self.update_ready()), self.recent_menu.as_deref().unwrap_or_default()),
        };
        fit_menu(&rows, self.popup, &mut self.submenu, &mut self.menu_cursor);
        rows
    }

    /// Reads the Recent Items when the logo menu opens and drops them when it closes, so it reads the file once.
    pub fn refresh_recent(&mut self) {
        if self.popup != Some(Popup::Menu(Menu::Logo)) {
            self.recent_menu = None;
            return;
        }
        if self.recent_menu.is_some() {
            return;
        }
        // ponytail: five of each, files that no longer exist skipped; entries past `[housekeeping] recent_days` go
        // in the daily cleanup, not here.
        let (names, uris) = dock::recent_items(&std::fs::read_to_string(dock::recent_path()).unwrap_or_default());
        let mut apps: Vec<(String, String)> = Vec::new();
        for app in names.iter().filter_map(|n| self.apps.find(n)).filter(|a| !a.no_display) {
            if apps.len() < 5 && !apps.iter().any(|(_, id)| *id == app.id) {
                apps.push((app.name.clone(), app.id.clone()));
            }
        }
        let docs: Vec<(String, String)> = uris
            .into_iter()
            .filter_map(|uri| {
                let path = crate::apps::file_path(&uri)?;
                let name = std::path::Path::new(&path).file_name()?.to_string_lossy().into_owned();
                std::path::Path::new(&path).exists().then_some((name, uri))
            })
            .take(5)
            .collect();
        self.recent_menu = Some(recent_rows(&apps, &docs));
    }

    /// The battery menu: time left and power source, the peripherals' batteries, then Battery Settings.
    pub fn battery_menu_rows(&self) -> Vec<Option<Row>> {
        let row = |label: String, action| Some(Row { label, shortcut: "", action, sub: Vec::new() });
        let mut rows = Vec::new();
        if let Some(b) = &self.battery {
            let source = if b.on_battery { "Battery" } else { "Power Adapter" };
            rows.extend([row(crate::services::power::time_left(b), None), row(format!("Power Source: {source}"), None), None]);
        }
        if !self.peripherals.is_empty() {
            rows.extend(self.peripherals.iter().map(|p| row(format!("{}  {}%", p.name, p.percent), None)));
            rows.push(None);
        }
        rows.push(row("Battery Settings…".into(), Some(Action::Spawn("noobsbot-desktop-settings --page battery".into()))));
        rows
    }

    /// The tray items hidden under "»", by title; a click opens the item's menu or activates it.
    pub fn overflow_menu_rows(&self) -> Vec<Option<Row>> {
        let hidden = self.popup_output().and_then(|o| self.bar_view(&o)).map_or(0, |b| b.overflow());
        let title = |t: &crate::services::tray::TrayItem| if t.title.is_empty() { t.service.clone() } else { t.title.clone() };
        self.tray
            .iter()
            .take(hidden)
            .enumerate()
            .map(|(i, t)| Some(Row { label: title(t), shortcut: "", action: Some(Action::TrayItem(i)), sub: Vec::new() }))
            .collect()
    }

    /// The open submenus (`State::submenu`) while their menu is the popup.
    pub fn open_path(&self) -> &[usize] {
        self.submenu.as_ref().filter(|(popup, _)| Some(*popup) == self.popup).map_or(&[], |(_, path)| path)
    }

    /// Level `d` of the open menu as last drawn: the menu, or its d-th open submenu.
    fn level_view(&self, d: usize) -> Option<&crate::render::MenuPopover> {
        let b = self.backend.as_ref()?;
        if d == 0 { Some(&b.menu) } else { b.submenu.get(d - 1) }
    }

    /// The deepest open level of the menu under `at` (popup-relative) and its row there.
    fn menu_level_at(&self, at: Point<f64, Logical>) -> Option<(usize, Option<usize>)> {
        (0..=self.open_path().len()).rev().find_map(|d| {
            let view = self.level_view(d)?;
            view.rect()?.to_f64().contains(at).then(|| (d, view.row_at(at)))
        })
    }

    /// After `delay` the row under the pointer gets its submenu (`menu_hover`); `restart`: a running timer starts
    /// over, else it is left to run.
    fn hover_later(&mut self, delay: std::time::Duration, restart: bool) {
        if self.aim_timer.is_some() && !restart {
            return;
        }
        self.aim_cancel();
        let timer = self.loop_handle.insert_source(Timer::from_duration(delay), |_, _, state| {
            state.aim_timer = None;
            let at = state.to_popup(state.to_output(state.pointer_at));
            if let (Some(Popup::Menu(menu)), Some((d, Some(i)))) = (state.popup, state.menu_level_at(at)) {
                state.menu_hover(menu, d, i);
            }
            TimeoutAction::Drop
        });
        self.aim_timer = timer.inspect_err(|err| tracing::warn!("menu hover timer: {err}")).ok();
    }

    fn aim_cancel(&mut self) {
        if let Some(token) = self.aim_timer.take() {
            self.loop_handle.remove(token);
        }
    }

    /// The pointer or a click is on row `i` of level `d` of the open `menu`: its submenu opens, any other on that
    /// level or deeper closes.
    fn menu_hover(&mut self, menu: Menu, d: usize, i: usize) {
        self.aim_cancel();
        let rows = self.open_menu_rows(menu);
        let mut path = self.open_path().to_vec();
        path.truncate(d);
        let opens = sub_rows(&rows, &path).get(i).and_then(Option::as_ref).is_some_and(|r| !r.sub.is_empty());
        if opens {
            path.push(i);
        }
        if path == self.open_path() {
            return;
        }
        // The app may fill a submenu in only when asked (AboutToShow).
        if let (true, Menu::Tray(_), Some((service, menu, all))) = (opens, menu, &self.tray_menu)
            && let Some(row) = tray_row(all, &path)
        {
            self.services.send(Command::TrayMenu { service: service.clone(), menu: menu.clone(), parent: row.id });
        }
        self.submenu = self.popup.map(|popup| (popup, path));
        self.schedule_redraw();
    }

    /// The keys of an open menu: Up/Down light the next live row, Right or Return open its submenu or run it, Left
    /// or Esc back out of one submenu, a letter (`letters`: no shortcut has it) lights the next row starting with it.
    /// Left/Right on a bar menu with no submenu in the way go to the neighbouring bar menu; in Notification Center
    /// (unless a shortcut has them) they turn the calendar's month. `true`: used.
    pub fn menu_key(&mut self, sym: smithay::input::keyboard::Keysym, letters: bool) -> bool {
        use smithay::input::keyboard::Keysym;
        if let Some(Popup::Notifications { year, month }) = self.popup {
            // ←/→ turn the calendar's month.
            let by = match sym {
                Keysym::Left if letters => -1,
                Keysym::Right if letters => 1,
                _ => return false,
            };
            return self.calendar_turn(shift_month(year, month, by));
        }
        if let Some(Popup::Module(_, i)) = self.popup {
            // A status module takes no other keys; Esc closes it (`close_popup`).
            return matches!(sym, Keysym::Left | Keysym::Right) && self.bar_key(BarItem::Status(i), sym == Keysym::Right);
        }
        let Some(popup @ Popup::Menu(menu)) = self.popup else { return false };
        let rows = self.open_menu_rows(menu);
        let path = self.open_path().to_vec();
        let (d, at) = self.menu_at();
        let level = sub_rows(&rows, &path[..d]).to_vec();
        let row = at.and_then(|i| level.get(i).cloned().flatten());
        let has_sub = row.as_ref().is_some_and(|r| !r.sub.is_empty());
        let bar = popup.item().filter(|_| d == 0);
        match sym {
            Keysym::Up | Keysym::Down => self.menu_cursor_to(popup, d, menu_step(&level, at, sym == Keysym::Down)),
            // One level back: the deepest open submenu closes and the row that opened it is lit.
            Keysym::Left | Keysym::Escape if !path.is_empty() => {
                let t = d.min(path.len() - 1);
                self.submenu = Some((popup, path[..t].to_vec()));
                self.menu_cursor = Some((popup, t, path[t]));
                self.schedule_redraw();
            }
            // Esc with no submenu closes the menu (`close_popup`).
            Keysym::Escape => return false,
            Keysym::Left | Keysym::Right if bar.is_some() && !(sym == Keysym::Right && has_sub) => {
                if let Some(from) = bar {
                    self.bar_key(from, sym == Keysym::Right);
                }
            }
            Keysym::Right | Keysym::Return | Keysym::KP_Enter => match (row, at) {
                (Some(row), Some(i)) if has_sub => {
                    self.menu_hover(menu, d, i);
                    // Into its first live row; a tray submenu still being filled in keeps the parent lit.
                    self.menu_cursor = Some(match menu_step(&row.sub, None, true) {
                        Some(j) => (popup, d + 1, j),
                        None => (popup, d, i),
                    });
                    self.schedule_redraw();
                }
                (Some(Row { action: Some(action), .. }), _) => {
                    self.popup = None;
                    self.submenu = None;
                    self.menu_cursor = None;
                    self.run_action(action);
                    self.schedule_redraw();
                }
                _ => {}
            },
            Keysym::Left => {}
            _ => match sym.key_char().filter(|c| letters && c.is_alphanumeric()) {
                Some(c) => self.menu_cursor_to(popup, d, menu_jump(&level, at, c)),
                None => return false,
            },
        }
        true
    }

    /// Left or Right from bar item `from`'s open popup: the neighbouring item's opens. Always `true` (used).
    fn bar_key(&mut self, from: BarItem, right: bool) -> bool {
        let items: Vec<_> = self.popup_output().and_then(|o| self.bar_view(&o)).map(|b| b.items().collect()).unwrap_or_default();
        let opens = |item| match item {
            BarItem::Tray(i) => self.tray.get(i).is_some_and(|t| t.menu.is_some()),
            _ => self.bar_popup(item).is_some(),
        };
        if let Some(next) = bar_step(&items, from, right, opens) {
            self.menu_cursor = None;
            // A tray item's menu: `secondary` asks for it rather than activating the item.
            self.open_bar_item(next, Point::default(), true);
            self.schedule_redraw();
        }
        true
    }

    /// The level the keyboard is in (0: the menu, n: its n-th open submenu) and its lit row: the keyboard's, else the
    /// pointer's.
    fn menu_at(&self) -> (usize, Option<usize>) {
        match self.menu_cursor {
            Some((popup, d, i)) if Some(popup) == self.popup && d <= self.open_path().len() => return (d, Some(i)),
            _ => {}
        }
        let at = self.to_popup(self.to_output(self.pointer_at));
        self.menu_level_at(at).unwrap_or((0, None))
    }

    /// Lights row `to` (if any) of level `d` of the menu, scrolled into view; deeper submenus close.
    fn menu_cursor_to(&mut self, popup: Popup, d: usize, to: Option<usize>) {
        let Some(i) = to else { return };
        if let Some((_, path)) = &mut self.submenu {
            path.truncate(d);
        }
        self.menu_cursor = Some((popup, d, i));
        if let Some(b) = self.backend.as_mut() {
            if d == 0 {
                b.menu.reveal(i)
            } else if let Some(sub) = b.submenu.get_mut(d - 1) {
                sub.reveal(i)
            }
        }
        self.schedule_redraw();
    }

    /// An auto-hiding bar shows while the pointer touches the top edge of a display, and stays while it is on the bar.
    // ponytail: one flag for every display's bar, so they slide together.
    pub fn bar_motion(&mut self, at: Point<f64, Logical>) {
        if !self.config.menu_bar.auto_hide {
            return;
        }
        let output = self.space.output_under(at).next().and_then(|o| self.space.output_geometry(o));
        let y = output.map_or(f64::MAX, |g| at.y - f64::from(g.loc.y));
        let hover = menubar::reveals(self.bar_pointer || self.popup.is_some(), y);
        if hover != self.bar_pointer {
            self.bar_pointer = hover;
            self.schedule_redraw();
        }
    }

    /// How far an auto-hiding bar is out of sight this frame, 0 (shown) to 1 (gone); keeps frames coming while it
    /// slides. Open menus keep it shown.
    pub fn bar_hidden(&mut self) -> f64 {
        let now = self.frame_time;
        let hide = self.config.menu_bar.auto_hide && !self.bar_pointer && self.popup.is_none();
        if self.config.appearance.reduce_motion {
            return f64::from(u8::from(hide));
        }
        self.bar_hide.set(hide, now);
        if self.bar_hide.moving(now) {
            crate::anim::extend(&mut self.anim_until, now + crate::shell::dock_fx::HIDE_TIME);
        }
        self.bar_hide.value(now)
    }

    /// A click on tray item `i`: its menu (fetched, then shown by `show_tray_menu`) if it is a menu or this is a
    /// right click, else Activate at the click.
    pub fn tray_press(&mut self, i: usize, location: Point<f64, Logical>, secondary: bool) {
        let Some(item) = self.tray.get(i) else { return };
        let command = match &item.menu {
            Some(menu) if item.item_is_menu || secondary => Command::TrayMenu { service: item.service.clone(), menu: menu.clone(), parent: 0 },
            _ => {
                let at = location.to_i32_round() + self.first_origin();
                Command::TrayActivate { service: item.service.clone(), path: item.path.clone(), x: at.x, y: at.y }
            }
        };
        self.services.send(command);
    }

    /// A tray menu's rows arrived: opens it under its item, or (`parent` ≠ 0) fills in that submenu of the open one.
    pub fn show_tray_menu(&mut self, service: String, menu: String, parent: i32, rows: Vec<Option<MenuRow>>) {
        if parent != 0 {
            let open = self.tray_menu.as_mut().filter(|(s, m, _)| *s == service && *m == menu);
            if let Some(row) = open.and_then(|(_, _, all)| tray_find(all, parent)) {
                row.submenu = Some(rows);
                self.schedule_redraw();
            }
            return;
        }
        let Some(i) = self.tray.iter().position(|t| t.service == service) else { return };
        if !rows.is_empty() {
            self.popup = Some(Popup::Menu(Menu::Tray(i)));
            self.tray_menu = Some((service, menu, rows));
        }
    }

    /// Shows `osd` for 1.5 s after the latest call; the banners spring out of its way meanwhile.
    pub fn show_osd(&mut self, osd: Osd) {
        if let Some(token) = self.osd_timer.take() {
            self.loop_handle.remove(token);
        }
        self.osd = Some(osd);
        self.banners_dodge(true);
        let hide = self.loop_handle.insert_source(Timer::from_duration(OSD_TIME), |_, _, state| {
            state.osd = None;
            state.osd_timer = None;
            state.banners_dodge(false);
            state.schedule_redraw();
            TimeoutAction::Drop
        });
        self.osd_timer = hide.inspect_err(|err| tracing::warn!("OSD timer: {err}")).ok();
        self.schedule_redraw();
    }

    /// Esc: closes the open popup. `true` if there was one.
    pub fn close_popup(&mut self) -> bool {
        self.shell_drag = None;
        let open = self.popup.take().is_some();
        self.submenu = None;
        self.menu_cursor = None;
        if open {
            self.schedule_redraw();
        }
        open
    }
}

/// The month a click in the calendar goes to: ‹ and › step, the title returns to today's month.
fn calendar_click(layout: &CalendarLayout, location: Point<f64, Logical>, (year, month): (i32, u32)) -> Option<(i32, u32)> {
    if layout.prev.to_f64().contains(location) {
        Some(shift_month(year, month, -1))
    } else if layout.next.to_f64().contains(location) {
        Some(shift_month(year, month, 1))
    } else if layout.title.to_f64().contains(location) {
        Some(this_month())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::displays::display_for;

    /// The main display, and a second one left of and above it (negative origin).
    fn two_displays() -> [Rectangle<i32, Logical>; 2] {
        [Rectangle::new((0, 0).into(), (1920, 1080).into()), Rectangle::new((-1280, -200).into(), (1280, 1024).into())]
    }

    #[test]
    fn points_move_between_displays() {
        let [main, left] = two_displays();
        // Global (-600, 100) is first-output-relative (-600, 100); on the left display it is (680, 300).
        let on_left = rebase((-600.0, 100.0).into(), main.loc, left.loc);
        assert_eq!(on_left, (680.0, 300.0).into());
        assert_eq!(rebase(on_left, left.loc, main.loc), (-600.0, 100.0).into());
        // A main display that is not at the origin, the second at it.
        let (main, second) = (Point::from((1280, 0)), Point::from((0, 0)));
        assert_eq!(rebase((-1180.0, 50.0).into(), main, second), (100.0, 50.0).into());
        assert_eq!(rebase((1.0, 2.0).into(), main, main), (1.0, 2.0).into());
    }

    #[test]
    fn launchpad_opens_on_the_pointers_display() {
        let screens = two_displays();
        // The pointer just left of the main display: Launchpad is laid out in the left one's size, and a press there
        // lands inside it.
        let pointer = Point::from((-10.0, 500.0));
        let at = display_for(&screens, None, pointer);
        assert_eq!(at, 1);
        let local = rebase(pointer, screens[0].loc, screens[at].loc);
        assert_eq!(local, (1270.0, 700.0).into());
        assert!(Rectangle::from_size(screens[at].size).to_f64().contains(local));
    }

    #[test]
    fn drawer_sits_over_its_dock_icon_on_that_display() {
        use drawer::{Anchor, layout, layout::GAP};
        let [_, left] = two_displays();
        // The icon's centre and top, relative to the left display's dock (and so to that display).
        let l = layout(Anchor::Dock((1200, 960).into()), left.size, 80, 8);
        assert_eq!(l.rect.loc.y + l.rect.size.h, 960 - GAP);
        assert_eq!(l.rect.loc.x + l.rect.size.w, left.size.w - GAP, "pushed in from the display's right edge");
        let global = Rectangle::new(l.rect.loc + left.loc, l.rect.size);
        assert_eq!(global.intersection(left), Some(global), "wholly on the left display");
    }

    #[test]
    fn screenshots_are_of_the_pointers_display() {
        let [main, left] = two_displays();
        // The toolbar and a region drag on the left display, above the main one's top.
        assert_eq!(display_for(&[main, left], None, (-1000.0, -100.0).into()), 1);
        let a = rebase((-1000.0, -100.0).into(), main.loc, left.loc);
        let b = rebase((100.0, 0.0).into(), main.loc, left.loc);
        assert_eq!(a, (280.0, 100.0).into());
        // A drag past the display's edge is cut to it: a capture reads one display's frame.
        let rect = screenshot::region(a, b).intersection(Rectangle::from_size(left.size));
        assert_eq!(rect, Some(Rectangle::new((280, 100).into(), (1000, 100).into())));
        // Window: the focused window's display.
        assert_eq!(display_for(&[main, left], Some((-300, 400).into()), (500.0, 500.0).into()), 1);
    }

    #[test]
    fn thumbnail_follows_the_dragging_pointer_across_displays() {
        let [main, left] = two_displays();
        // Dragged onto the left display: drawn there, centred on the pointer in its coordinates.
        let pointer = Point::from((-1270.0, -190.0));
        assert_eq!(display_for(&[main, left], None, pointer), 1);
        assert_eq!(rebase(pointer, main.loc, left.loc), (10.0, 10.0).into());
    }

    #[test]
    fn spotlight_opens_on_the_focused_windows_display() {
        let screens = two_displays();
        // The focused window's centre on the left display, the pointer on the main one.
        assert_eq!(display_for(&screens, Some((-640, 300).into()), (100.0, 100.0).into()), 1);
        // No focused window: the pointer's.
        assert_eq!(display_for(&screens, None, (-600.0, -150.0).into()), 1);
        assert_eq!(display_for(&screens, None, (100.0, 100.0).into()), 0);
    }

    #[test]
    fn months_wrap_years() {
        assert_eq!(shift_month(2026, 12, 1), (2027, 1));
        assert_eq!(shift_month(2026, 1, -1), (2025, 12));
        assert_eq!(shift_month(2026, 10, 1), (2026, 11));
    }

    #[test]
    fn calendar_layout_holds_its_cells() {
        let layout = calendar_layout(992, 28, 5, false);
        assert_eq!(layout.rect, Rectangle::new((992, 28).into(), (280, 228).into()));
        // Every cell and both arrows sit inside the popover.
        assert!(layout.cells.iter().chain([&layout.prev, &layout.next, &layout.title]).all(|c| layout.rect.contains_rect(*c)));
        assert_eq!(layout.cells.len(), 35);
        assert!(!layout.prev.overlaps(layout.next) && !layout.title.overlaps(layout.prev));
    }

    #[test]
    fn calendar_rows_grow_with_text() {
        let big = crate::text::with_scale(1.5, || calendar_layout(992, 28, 5, false));
        let small = calendar_layout(992, 28, 5, false);
        assert_eq!(big.rect.size.w, small.rect.size.w);
        assert!(big.rect.size.h > small.rect.size.h && big.cells[0].size.h == 48 && big.title.size.h == 30 && big.header[0].size.h == 21);
        assert!(big.cells.iter().chain([&big.prev, &big.next, &big.title]).chain(&big.header).all(|c| big.rect.contains_rect(*c)));
        assert!(big.header[0].loc.y >= big.title.loc.y + big.title.size.h && big.cells[0].loc.y >= big.header[0].loc.y + big.header[0].size.h);
    }

    #[test]
    fn week_numbers_take_a_column_at_the_left() {
        for scale in [1.0, 1.5] {
            let (plain, weeks) = crate::text::with_scale(scale, || (calendar_layout(992, 28, 6, false), calendar_layout(992, 28, 6, true)));
            assert_eq!((weeks.rect, weeks.cells.len(), weeks.weeks.len()), (plain.rect, 42, 6));
            assert!(plain.weeks.is_empty());
            for (row, week) in weeks.weeks.iter().enumerate() {
                let first = weeks.cells[row * 7];
                // Level with its row, left of it, and the row still ends inside the card's padding.
                assert_eq!((week.loc.y, week.size.h), (first.loc.y, first.size.h));
                assert!(week.loc.x + week.size.w <= first.loc.x && week.loc.x >= weeks.rect.loc.x + calendar::PAD);
                assert!(weeks.cells[row * 7 + 6].loc.x + weeks.cells[row * 7 + 6].size.w <= weeks.rect.loc.x + calendar::WIDTH - calendar::PAD);
            }
            assert!(weeks.cells.iter().chain(&weeks.weeks).chain(&weeks.header).all(|c| weeks.rect.contains_rect(*c)));
            assert_eq!(weeks.header[0].loc.x, weeks.cells[0].loc.x);
        }
    }

    #[test]
    fn status_items_open_their_module() {
        use menubar::Kind;
        assert_eq!(status_popup(Kind::Network, 0), Some(Popup::Module(control::Module::Wifi, 0)));
        assert_eq!(status_popup(Kind::Bluetooth, 1), Some(Popup::Module(control::Module::Bluetooth, 1)));
        assert_eq!(status_popup(Kind::Volume, 3), Some(Popup::Module(control::Module::Sound, 3)));
        assert_eq!(status_popup(Kind::Battery, 2), Some(Popup::Menu(Menu::Battery(2))));
        assert_eq!(status_popup(Kind::Recording, 0), None, "the stop item stops, it opens nothing");
        assert_eq!(Popup::Module(control::Module::Sound, 3).item(), Some(BarItem::Status(3)));
    }

    #[test]
    fn recent_items_list_apps_and_documents() {
        let label = |r: &Option<Row>| r.as_ref().map(|r| r.label.clone());
        let rows = recent_rows(&[("Kate".into(), "org.kde.kate".into())], &[("a.txt".into(), "file:///a.txt".into())]);
        let labels: Vec<_> = rows.iter().map(label).collect();
        let s = |t: &str| Some(t.to_owned());
        assert_eq!(labels, [s("Applications"), s("Kate"), None, s("Documents"), s("a.txt"), None, s("Clear Menu")]);
        assert_eq!(rows[1].as_ref().unwrap().action, Some(Action::Launch("org.kde.kate".into())));
        assert_eq!(rows[4].as_ref().unwrap().action, Some(Action::Spawn("xdg-open file:///a.txt".into())));
        assert_eq!(rows[6].as_ref().unwrap().action, Some(Action::ClearRecent));
        // Nothing to clear: Clear Menu is greyed.
        assert_eq!(recent_rows(&[], &[]).last().unwrap().as_ref().unwrap().action, None);
        // The logo menu carries them as its Recent Items submenu.
        let logo = menu_rows(Menu::Logo, "", &[], (0, false), &rows);
        assert_eq!(logo.iter().flatten().find(|r| r.label == "Recent Items").map(|r| r.sub.len()), Some(rows.len()));
        // Pending updates: their count beside Software Update…, which a verified download makes Restart to Update….
        let update = |pending| menu_rows(Menu::Logo, "", &[], pending, &[])[2].clone().map(|r| (r.label, r.shortcut));
        assert_eq!(update((0, false)), Some(("Software Update…".into(), "")));
        assert_eq!(update((2, false)), Some(("Software Update…".into(), "2")));
        assert_eq!(update((1, true)), Some(("Restart to Update…".into(), "1")));
    }

    #[test]
    fn menus_stay_on_screen_and_stack_rows() {
        let rows = menu_rows(Menu::App, "Kate", &[], (0, false), &[]);
        let output = Size::from((1280, 1200));
        let (rect, cells) = menu_layout(Anchor::Below(6), output, 200, &rows, 0);
        // 5 rows of 22 + 2 separators of 11 + padding 5 at both ends.
        assert_eq!(rect, Rectangle::new((8, 28).into(), (200, 142).into()));
        assert_eq!(cells[0], Rectangle::new((13, 33).into(), (190, 22).into()));
        assert_eq!(cells[2].loc.y, 33 + 22 + 11);
        assert!(cells.iter().all(|c| rect.contains_rect(*c)));
        // Near the right edge it moves left to fit.
        assert_eq!(menu_layout(Anchor::Below(1200), output, 200, &rows, 0).0.loc.x, 1280 - 8 - 200);
        // Above a dock icon: centred on it, its bottom 4 px over the point.
        let (above, _) = menu_layout(Anchor::Above((640, 1100).into()), output, 200, &rows, 0);
        assert_eq!(above, Rectangle::new((540, 1100 - 4 - 142).into(), (200, 142).into()));
        assert_eq!(rows[2].as_ref().map(|r| (r.label.as_str(), r.action.clone())), Some(("Hide Kate", Some(Action::Hide))));
    }

    #[test]
    fn popups_fit_a_second_display_at_its_right_edge() {
        // Laid out in the second display's own size and coordinates, whatever its place in the layout.
        let rows = menu_rows(Menu::Logo, "", &[], (0, false), &[]);
        let second = Size::from((1920, 1080));
        let (rect, cells) = menu_layout(Anchor::Below(1900), second, 240, &rows, 0);
        assert_eq!((rect.loc.x, rect.loc.y), (1920 - 8 - 240, menubar::height() + DROP));
        assert!(cells.iter().all(|c| rect.contains_rect(*c)));
        // Its submenu flips left of the menu rather than off the display.
        let row = Rectangle::new((rect.loc.x, 60).into(), (240, 22).into());
        assert_eq!(menu_layout(Anchor::Beside(row, false), second, 200, &rows, 0).0.loc.x, rect.loc.x - 200);
        let panel = control::layout(second, &control::Status::default()).rect;
        assert_eq!(panel.loc.x + panel.size.w, 1920 - MARGIN);
        assert_eq!(osd_rect(second.w).loc.x + 280, 1920 - MARGIN);
    }

    #[test]
    fn menu_rows_grow_with_the_text_size() {
        let rows = menu_rows(Menu::App, "Kate", &[], (0, false), &[]);
        let output = Size::from((1280, 1200));
        let big = crate::text::with_scale(1.5, || menu_layout(Anchor::Below(6), output, 200, &rows, 0));
        let small = menu_layout(Anchor::Below(6), output, 200, &rows, 0);
        // 5 rows of 33 (a 13 px font at 1.5 shapes into 24 px lines) + the same separators and padding.
        assert_eq!(big.0.size.h, 5 * 33 + 2 * 11 + 2 * 5);
        assert!(big.1.iter().zip(&rows).all(|(c, r)| r.is_none() || c.size.h >= 24));
        assert_eq!(small.0.size.h, 142, "unchanged at 1.0");
        assert!(big.1.iter().all(|c| big.0.contains_rect(*c)));
    }

    #[test]
    fn tall_menus_stay_on_screen_and_scroll_by_whole_rows() {
        let rows: Vec<Option<Row>> = (0..40).map(|i| Some(Row { sub: Vec::new(), label: format!("Item {i}"), shortcut: "", action: None })).collect();
        let output = Size::from((1280, 768));
        let shown = |cells: &[Rect]| cells.iter().filter(|c| c.size.h > 0).count();
        // Under the bar: clipped at the bottom margin, whole rows only.
        let (rect, cells) = menu_layout(Anchor::Below(6), output, 200, &rows, 0);
        assert_eq!(rect.loc.y, 28);
        assert!(rect.loc.y + rect.size.h <= 768 - 8 && rect.size.h > 700);
        assert!(cells[0].size.h == 22 && shown(&cells) < 40 && cells.iter().all(|c| c.size.h == 0 || rect.contains_rect(*c)));
        // Scrolled past the end: clamped so the last row is the last one shown.
        let (_, cells) = menu_layout(Anchor::Below(6), output, 200, &rows, 100);
        assert!(cells[39].size.h == 22 && cells[0].size.h == 0 && shown(&cells) == shown(&menu_layout(Anchor::Below(6), output, 200, &rows, 0).1));
        // Scrolled one row: the first row goes, the next one shows up at the top.
        let (_, cells) = menu_layout(Anchor::Below(6), output, 200, &rows, 1);
        assert!(cells[0].size.h == 0 && cells[1].loc.y == 33);
        // A dock menu taller than the screen moves down under the bar instead of off the top; a short one is unmoved.
        assert_eq!(menu_layout(Anchor::Above((640, 740).into()), output, 200, &rows, 0).0.loc.y, 28);
        assert_eq!(menu_layout(Anchor::Above((640, 740).into()), output, 200, &rows[..3], 0).0.loc.y, 740 - 4 - 76);
        // A context menu near the bottom is pushed up to fit.
        let (rect, _) = menu_layout(Anchor::At((100, 700).into()), output, 200, &rows[..5], 0);
        assert_eq!(rect.loc.y + rect.size.h, 768 - 8);
    }

    #[test]
    fn submenus_open_beside_their_row_and_flip_at_the_edge() {
        let rows = menu_rows(Menu::App, "Kate", &[], (0, false), &[]);
        let output = Size::from((1280, 800));
        // The parent spans x 100..300; its row is at y 60.
        let row = Rectangle::new((100, 60).into(), (200, 22).into());
        let (rect, cells) = menu_layout(Anchor::Beside(row, false), output, 200, &rows, 0);
        assert_eq!((rect.loc.x, cells[0].loc.y), (300, 60), "to the right, first row level with the parent's");
        // No room on the right: to the left of the parent instead of over it.
        let row = Rectangle::new((1000, 60).into(), (200, 22).into());
        assert_eq!(menu_layout(Anchor::Beside(row, false), output, 200, &rows, 0).0.loc.x, 800);
        // A submenu of a submenu that went left goes left too, and right when the left has no room either.
        assert_eq!(submenu_x(Rectangle::new((800, 60).into(), (200, 22).into()), 200, 1280, true), 600);
        assert_eq!(submenu_x(Rectangle::new((100, 60).into(), (200, 22).into()), 200, 1280, true), 300);
        // Neither side fits: the preferred side, which `menu_layout` clamps onto the screen.
        assert_eq!(submenu_x(Rectangle::new((100, 60).into(), (1000, 22).into()), 400, 1280, false), 1100);
        assert_eq!(menu_layout(Anchor::Beside(Rectangle::new((100, 60).into(), (1000, 22).into()), false), output, 400, &rows, 0).0.loc.x, 1280 - 8 - 400);
        // Near the bottom: pushed up so it fits.
        let low = Rectangle::new((100, 780).into(), (200, 22).into());
        let (rect, _) = menu_layout(Anchor::Beside(low, false), output, 200, &rows, 0);
        assert_eq!(rect.loc.y + rect.size.h, 800 - 8);
    }

    #[test]
    fn tray_submenus_nest() {
        let row = |id, label: &str, submenu| Some(MenuRow { id, label: label.into(), enabled: true, submenu });
        let deep = row(3, "Deep", Some(vec![None, row(4, "Leaf", None)]));
        let tray = [row(1, "Open", None), row(2, "More", Some(vec![deep])), row(5, "Lazy", Some(Vec::new()))];
        let rows = tray_rows(&tray);
        let labels = |rows: &[Option<Row>]| rows.iter().flatten().map(|r| r.label.clone()).collect::<Vec<_>>();
        assert_eq!(rows[0].as_ref().unwrap().action, Some(Action::TrayMenu(1)));
        let more = rows[1].as_ref().unwrap();
        assert_eq!((more.action.clone(), labels(&more.sub)), (None, vec!["Deep".to_owned()]));
        // Two levels down, rows line up with the app's, so a path finds the same row in both.
        let leaf = sub_rows(&rows, &[1, 0])[1].as_ref().unwrap();
        assert_eq!((leaf.label.as_str(), &leaf.action), ("Leaf", &Some(Action::TrayMenu(4))));
        assert_eq!(tray_row(&tray, &[1, 0, 1]).map(|r| r.id), Some(4));
        assert_eq!(sub_rows(&rows, &[0, 3]), [], "a path off the rows: none");
        assert_eq!(labels(&rows[2].as_ref().unwrap().sub), ["Empty"]);
        // A lazy submenu two levels down is filled in where it is.
        let mut tray = tray.to_vec();
        tray_find(&mut tray, 3).unwrap().submenu = Some(vec![row(6, "New", None)]);
        assert_eq!(tray_row(&tray, &[1, 0, 0]).map(|r| r.id), Some(6));
    }

    #[test]
    fn rows_shrinking_under_an_open_path_and_cursor_cut_them_back() {
        let row = |id, label: &str, submenu| Some(MenuRow { id, label: label.into(), enabled: true, submenu });
        let leaves = |n| Some((10..10 + n).map(|id| row(id, "Leaf", None)).collect());
        let p = Popup::Menu(Menu::Tray(0));
        let open = |path: Vec<usize>, cursor| (Some((p, path)), Some((p, 2, cursor)));
        // Open, More › Deep › its fifth leaf lit by the keyboard.
        let (mut submenu, mut cursor) = open(vec![1, 0], 4);
        // Deep came back with two leaves: the last one is lit instead.
        let rows = tray_rows(&[row(1, "Open", None), row(2, "More", Some(vec![row(3, "Deep", leaves(2))]))]);
        fit_menu(&rows, Some(p), &mut submenu, &mut cursor);
        assert_eq!((submenu.clone().unwrap().1, cursor.unwrap()), (vec![1, 0], (p, 2, 1)));
        // More lost its submenu: the path stops before it and the cursor, two levels down, goes.
        let rows = tray_rows(&[row(1, "Open", None), row(2, "More", None)]);
        let (mut submenu, mut cursor) = open(vec![1, 0], 4);
        fit_menu(&rows, Some(p), &mut submenu, &mut cursor);
        assert_eq!((submenu.unwrap().1, cursor), (vec![], None));
        // More itself is gone (the window behind it closed); another popup's state is left alone.
        let (mut submenu, mut cursor) = open(vec![1, 0], 4);
        fit_menu(&rows[..1], Some(Popup::Control), &mut submenu, &mut cursor);
        assert_eq!(submenu.as_ref().unwrap().1, [1, 0]);
        fit_menu(&rows[..1], Some(p), &mut submenu, &mut cursor);
        assert_eq!((submenu.unwrap().1, cursor), (vec![], None));
        // A keyboard row past the end of the menu itself: its last live row.
        let (mut submenu, mut cursor) = (None, Some((p, 0, 9)));
        fit_menu(&rows, Some(p), &mut submenu, &mut cursor);
        assert_eq!(cursor, Some((p, 0, 1)));
        assert_eq!(menu_step(&rows, Some(9), false), Some(1), "Up from past the end: no panic");
    }

    #[test]
    fn keys_skip_separators_and_disabled_rows_and_stop_at_the_ends() {
        // About Kate (disabled), —, Hide Kate, Hide Others, Show All, —, Quit Kate.
        let rows = menu_rows(Menu::App, "Kate", &[], (0, false), &[]);
        assert_eq!(menu_step(&rows, None, true), Some(2), "Down from nothing: the first live row");
        assert_eq!(menu_step(&rows, None, false), Some(6), "Up from nothing: the last");
        assert_eq!(menu_step(&rows, Some(4), true), Some(6), "over the separator");
        assert_eq!(menu_step(&rows, Some(2), false), Some(2), "the disabled first row is skipped; the top stays");
        assert_eq!(menu_step(&rows, Some(6), true), Some(6), "the bottom stays");
        // A submenu row is live though it has no action.
        let logo = menu_rows(Menu::Logo, "", &[], (0, false), &recent_rows(&[], &[]));
        assert_eq!(menu_step(&logo, Some(2), true), Some(4));
        assert_eq!(menu_step(&[None, None], None, true), None);
    }

    #[test]
    fn letters_jump_to_the_next_row_starting_with_them() {
        let rows = menu_rows(Menu::App, "Kate", &[], (0, false), &[]);
        assert_eq!(menu_jump(&rows, None, 'h'), Some(2));
        assert_eq!(menu_jump(&rows, Some(2), 'H'), Some(3), "the next one, any case");
        assert_eq!(menu_jump(&rows, Some(3), 'h'), Some(2), "round to the top");
        assert_eq!(menu_jump(&rows, None, 'a'), None, "About Kate is disabled");
        assert_eq!(menu_jump(&rows, Some(2), 'z'), None);
    }

    #[test]
    fn left_and_right_go_round_the_bar_menus() {
        let items =
            [BarItem::Logo, BarItem::App, BarItem::Tray(0), BarItem::Tray(1), BarItem::Status(0), BarItem::Input, BarItem::ControlCenter, BarItem::Clock];
        // Tray 1 only activates; Control Center and the clock open no menu.
        let menu = |item| item == BarItem::Tray(0);
        assert_eq!(bar_step(&items, BarItem::Logo, true, menu), Some(BarItem::App));
        assert_eq!(bar_step(&items, BarItem::Tray(0), true, menu), Some(BarItem::Input));
        assert_eq!(bar_step(&items, BarItem::Input, true, menu), Some(BarItem::Logo), "round");
        assert_eq!(bar_step(&items, BarItem::Logo, false, menu), Some(BarItem::Input));
        assert_eq!(bar_step(&items, BarItem::Tray(5), false, menu), None, "an item not on the bar");
        // Status 0 opens its module (Status 1, the recording's stop item, opens nothing).
        let items = [BarItem::Logo, BarItem::Tray(0), BarItem::Status(0), BarItem::Status(1), BarItem::Status(2), BarItem::Input, BarItem::Clock];
        let opens = |item| matches!(item, BarItem::Tray(0) | BarItem::Status(0) | BarItem::Status(2));
        assert_eq!(bar_step(&items, BarItem::Tray(0), true, opens), Some(BarItem::Status(0)));
        assert_eq!(bar_step(&items, BarItem::Status(0), true, opens), Some(BarItem::Status(2)), "past the stop item");
        assert_eq!(bar_step(&items, BarItem::Status(2), true, opens), Some(BarItem::Input));
        assert_eq!(bar_step(&items, BarItem::Input, false, opens), Some(BarItem::Status(2)));
    }

    #[test]
    fn heading_for_a_submenu_keeps_it_open() {
        // A submenu to the right at x 200..300, y 40..240; the pointer leaves its row at (150, 60).
        let sub = Rectangle::new((200, 40).into(), (100, 200).into());
        let at = |x, y| Point::<f64, Logical>::from((x, y));
        assert!(aims_at(at(150.0, 60.0), at(156.0, 66.0), sub), "down and to the right, into the next row");
        assert!(aims_at(at(150.0, 60.0), at(156.0, 58.0), sub), "a little up");
        assert!(!aims_at(at(150.0, 60.0), at(151.0, 70.0), sub), "straight down the menu");
        assert!(!aims_at(at(150.0, 60.0), at(144.0, 64.0), sub), "away from it");
        assert!(!aims_at(at(150.0, 60.0), at(150.0, 60.0), sub), "not moving");
        // Flipped to the left of its menu: aimed at its right edge.
        let left = Rectangle::new((0, 40).into(), (100, 200).into());
        assert!(aims_at(at(150.0, 60.0), at(144.0, 66.0), left) && !aims_at(at(150.0, 60.0), at(156.0, 66.0), left));
    }

    #[test]
    fn clicks_step_months() {
        let layout = calendar_layout(992, 28, 5, false);
        let centre = |r: Rect| r.to_f64().loc + r.to_f64().size.downscale(2.0).to_point();
        assert_eq!(calendar_click(&layout, centre(layout.next), (2026, 12)), Some((2027, 1)));
        assert_eq!(calendar_click(&layout, centre(layout.prev), (2026, 1)), Some((2025, 12)));
        assert_eq!(calendar_click(&layout, centre(layout.cells[10]), (2026, 10)), None);
        assert_eq!(calendar_click(&layout, centre(layout.title), (2020, 3)), Some(this_month()));
    }
}
