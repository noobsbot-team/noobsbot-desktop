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
            Popup::Menu(Menu::Battery(i) | Menu::Network(i)) => BarItem::Status(i),
            Popup::Menu(Menu::Dock | Menu::Desktop | Menu::Drawer | Menu::Window) => return None,
            Popup::Control => BarItem::ControlCenter,
        })
    }
}

/// How long a submenu stays open while the pointer heads for it across other rows of its menu.
const AIM_GRACE: std::time::Duration = std::time::Duration::from_millis(300);

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

/// What a click on `item` opens.
// ponytail: the status items' own menus (Wi-Fi list, Bluetooth devices) come with module expansion; until then they
// open nothing. The battery's opens in `open_bar_item`, so sliding along the bar and Left/Right skip it.
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
        BarItem::Status(_) | BarItem::Tray(_) => None,
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
    /// The network item's menu behind a captive portal, from `Content::status[i]`: `network_rows`.
    Network(usize),
    /// A desktop icon's or the desktop's right-click menu (E7.2): `State::desktop_menu_rows`.
    Desktop,
    /// An app's right-click menu in the app drawer: `State::drawer_menu_rows`.
    Drawer,
    /// A title bar's right-click menu (E8.5): `State::window_menu_rows`.
    Window,
}

/// Where a menu opens: under the bar with its left edge at x, centred over (x, bottom) like the dock's, or with its
/// top-left corner at a point (a right click on the desktop), or as a submenu beside a row: the rect spans the parent
/// menu's width at the row's height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Anchor {
    Below(i32),
    Above(Point<i32, Logical>),
    At(Point<i32, Logical>),
    Beside(Rect),
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
    let live = |i: &usize| rows[*i].as_ref().is_some_and(Row::live);
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

/// Left or Right from bar item `open` among `items` (left to right): the neighbouring one that opens a menu, going
/// round; `tray_menu(i)`: tray item i has one.
pub fn bar_step(items: &[BarItem], open: BarItem, right: bool, tray_menu: impl Fn(usize) -> bool) -> Option<BarItem> {
    let menus: Vec<_> = items
        .iter()
        .copied()
        .filter(|item| match item {
            BarItem::Tray(i) => tray_menu(*i),
            _ => matches!(popup_for(*item), Some(Popup::Menu(_))),
        })
        .collect();
    let at = menus.iter().position(|i| *i == open)?;
    let n = menus.len();
    Some(menus[if right { (at + 1) % n } else { (at + n - 1) % n }])
}

/// The rows of `menu`; `app` is the focused app's name, `tray` the open tray menu's rows, `update`: a verified
/// update is waiting (E6.13), `recent` the Recent Items submenu (`recent_rows`).
pub fn menu_rows(menu: Menu, app: &str, tray: &[Option<MenuRow>], update: bool, recent: &[Option<Row>]) -> Vec<Option<Row>> {
    let row = |label: String, shortcut, action| Some(Row { sub: Vec::new(), label, shortcut, action });
    let settings = |page: &str| Some(Action::Spawn(format!("noobsbot-desktop-settings --page {page}")));
    match menu {
        Menu::Logo => vec![
            row("About This Computer".into(), "", settings("about")),
            row("System Settings…".into(), "", settings("appearance")),
            // The row itself is the badge (E6.13).
            if update {
                row("Restart to Update…".into(), "", Some(Action::InstallUpdate))
            } else {
                row("Software Update…".into(), "", settings("general"))
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
        Menu::Tray(_) => tray_rows(tray, 0),
        // Built from state: `State::dock_menu_rows`, `State::input_menu_rows`, `State::desktop_menu_rows`.
        Menu::Dock | Menu::Input | Menu::Desktop | Menu::Drawer | Menu::Window | Menu::Overflow | Menu::Battery(_) | Menu::Network(_) => Vec::new(),
    }
}

/// The network item's menu behind a captive portal at `portal` (the check URL, which leads to the sign-in page).
pub fn network_rows(portal: Option<&str>) -> Vec<Option<Row>> {
    let row = |label: &str, action| Some(Row { label: label.to_owned(), shortcut: "", action, sub: Vec::new() });
    vec![
        row("Sign in to network…", portal.map(|url| Action::Spawn(format!("xdg-open {url}")))),
        None,
        row("Wi-Fi Settings…", Some(Action::Spawn("noobsbot-desktop-settings --page wi-fi".into()))),
    ]
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

/// A dbusmenu's rows: submenus open beside the menu, and from `depth` 1 on (a submenu's submenu) are listed inline
/// under their row, indented. An empty submenu shows "Empty".
// ponytail: one level of side submenus; deeper ones inline.
fn tray_rows(tray: &[Option<MenuRow>], depth: usize) -> Vec<Option<Row>> {
    let mut rows = Vec::new();
    for r in tray {
        let Some(r) = r else {
            rows.push(None);
            continue;
        };
        let indent = "    ".repeat(depth.saturating_sub(1));
        let action = (r.enabled && r.submenu.is_none()).then_some(Action::TrayMenu(r.id));
        let mut sub = match &r.submenu {
            Some(kids) if depth == 0 => tray_rows(kids, 1),
            _ => Vec::new(),
        };
        if r.submenu.is_some() && depth == 0 && sub.is_empty() {
            sub.push(Some(Row { label: "Empty".into(), shortcut: "", action: None, sub: Vec::new() }));
        }
        rows.push(Some(Row { label: format!("{indent}{}", r.label), shortcut: "", action, sub }));
        if let (Some(kids), 1..) = (&r.submenu, depth) {
            rows.extend(tray_rows(kids, depth + 1));
        }
    }
    rows
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
        // To the right of the parent, its first row level with the parent's row; flipped left at the screen edge.
        Anchor::Beside(r) if r.loc.x + r.size.w + width > output.w - MARGIN => (r.loc.x - width, r.loc.y - PAD),
        Anchor::Beside(r) => (r.loc.x + r.size.w, r.loc.y - PAD),
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

impl State {
    /// `location` relative to the first output's top-left corner, where the shell lays out.
    pub fn to_output(&self, location: Point<f64, Logical>) -> Point<f64, Logical> {
        let origin = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map_or((0, 0).into(), |g| g.loc);
        location - origin.to_f64()
    }

    /// The display the open popup shows on: a bar popup on its bar's (`bar_output`), the others on the first.
    pub fn popup_output(&self) -> Option<smithay::output::Output> {
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
        let origin = |o: Option<&smithay::output::Output>| o.and_then(|o| self.space.output_geometry(o)).map_or((0, 0).into(), |g| g.loc);
        location + origin(self.space.outputs().next()).to_f64() - origin(self.popup_output().as_ref()).to_f64()
    }

    /// `output`'s menu bar view: the first display's is `menu_bar`, the others' are in `bars`.
    pub fn bar_view(&self, output: &smithay::output::Output) -> Option<&menubar::MenuBar> {
        let b = self.backend.as_ref()?;
        if self.space.outputs().next() == Some(output) { Some(&b.menu_bar) } else { b.bars.get(&output.name()) }
    }

    /// The display whose menu bar is under `location` (first-output-relative), with `location` relative to it.
    pub fn menu_bar_at(&self, location: Point<f64, Logical>) -> Option<(smithay::output::Output, Point<f64, Logical>)> {
        let at = location + self.space.output_geometry(self.space.outputs().next()?)?.loc.to_f64();
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
            Popup::Control => Some(self.control_layout().rect),
        }
    }

    /// The open popup or its open submenu is under `location` (first-output-relative).
    pub fn in_popup(&self, location: Point<f64, Logical>) -> bool {
        let location = self.to_popup(location);
        let sub = self.open_submenu().and(self.backend.as_ref()).and_then(|b| b.submenu.rect());
        self.popup_rect().into_iter().chain(sub).any(|r| r.to_f64().contains(location))
    }

    /// Whether the shell, not an app, owns the pointer at `location` (output-relative): the bar, an open popup or the dock.
    pub fn shell_covers(&self, location: Point<f64, Logical>) -> bool {
        let on_alert = self.alert.is_some() && self.backend.as_ref().and_then(|b| b.alert.rect()).is_some_and(|r| r.to_f64().contains(location));
        if on_alert || self.on_banner(location) || self.mission_open() || self.osk_panel().is_some_and(|r| r.to_f64().contains(location)) {
            return true;
        }
        if self.spotlight.is_some() && self.spotlight_layout(self.spotlight_results().len()).is_some_and(|l| l.rect.to_f64().contains(location)) {
            return true;
        }
        if self.emoji.as_ref().is_some_and(|e| e.layout().rect.to_f64().contains(location)) {
            return true;
        }
        if self.force_quit.is_some() && self.force_quit_layout(0).is_some_and(|l| l.rect.to_f64().contains(location)) {
            return true;
        }
        if self.launchpad_open() || self.drawer_layout().is_some_and(|l| l.rect.to_f64().contains(location)) {
            return true;
        }
        self.menu_bar_at(location).is_some() || self.in_popup(location) || self.dock_covers(location)
    }

    /// A press at `location` (output-relative); `secondary`: the right button. `true`: the shell took it, so no
    /// window gets focus or the click.
    pub fn shell_press(&mut self, location: Point<f64, Logical>, secondary: bool) -> bool {
        // The alert is above everything else.
        if self.alert.is_some() && self.alert_press(location) {
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
            self.schedule_redraw();
        }
        taken
    }

    /// Opens bar `item`'s popup; a tray item's menu is asked for first (`tray_press`).
    fn open_bar_item(&mut self, item: BarItem, location: Point<f64, Logical>, secondary: bool) {
        self.popup = popup_for(item);
        match item {
            BarItem::Tray(i) => self.tray_press(i, location, secondary),
            // fcitx5 tells nobody when its method changes: ask now.
            BarItem::Input => self.services.send(crate::services::Command::FcitxRead),
            BarItem::ControlCenter => self.services.send(crate::services::Command::Inhibitors),
            BarItem::Status(i) if self.menu_bar().is_some_and(|c| c.status.get(i).is_some_and(|s| menubar::is_battery(s.symbol))) => {
                self.popup = Some(Popup::Menu(Menu::Battery(i)));
            }
            // Only the network's item has a badge, and only behind a captive portal; otherwise it opens nothing.
            BarItem::Status(i) if self.menu_bar().is_some_and(|c| c.status.get(i).is_some_and(|s| s.badge)) => {
                self.popup = Some(Popup::Menu(Menu::Network(i)));
            }
            _ => {}
        }
    }

    /// A press at `location`, relative to the popup's display.
    fn popup_press(&mut self, popup: Popup, location: Point<f64, Logical>) {
        match popup {
            Popup::Notifications { .. } => self.center_press(location),
            Popup::Menu(menu) => {
                let rows = self.open_menu_rows(menu);
                let Some(backend) = self.backend.as_ref() else { return };
                let main = backend.menu.row_at(location);
                let row = match self.open_submenu() {
                    Some(i) if backend.submenu.rect().is_some_and(|r| r.to_f64().contains(location)) => {
                        backend.submenu.row_at(location).and_then(|j| rows.get(i)?.as_ref()?.sub.get(j).cloned()).flatten()
                    }
                    _ => main.and_then(|i| rows.into_iter().nth(i)).flatten(),
                };
                // Separators and disabled rows keep the menu open; a submenu row opens its submenu.
                if let Some(action) = row.and_then(|row| row.action) {
                    self.popup = None;
                    self.run_action(action);
                } else if let Some(i) = main {
                    self.menu_hover(menu, i);
                }
            }
            Popup::Control => self.control_press(location),
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
        let Some(backend) = self.backend.as_ref() else { return };
        if let Some(next) = bar.and_then(|b| b.hit(to)).filter(|item| open.item().is_some_and(|o| o != *item)).and_then(popup_for) {
            self.popup = Some(next);
            self.schedule_redraw();
        } else if let Popup::Menu(menu) = open {
            let (row, moved) = (backend.menu.row_at(to), backend.menu.row_at(from) != backend.menu.row_at(to));
            let sub = backend.submenu.row_at(from) != backend.submenu.row_at(to);
            // Heading for the open submenu across other rows keeps it open a moment.
            let open = self.open_submenu();
            let aiming = open.is_some() && row != open && backend.submenu.rect().is_some_and(|r| aims_at(from, to, r));
            if aiming && row.is_some() {
                self.aim_wait();
            } else if let Some(i) = row.filter(|_| moved || self.aim_timer.is_some()) {
                // A row of the parent opens its submenu or closes the open one.
                self.aim_cancel();
                self.menu_hover(menu, i);
            }
            if moved || (sub && self.open_submenu().is_some()) {
                self.menu_cursor = None;
                self.schedule_redraw();
            }
        }
    }

    /// The rows of the open `menu`.
    pub fn open_menu_rows(&mut self, menu: Menu) -> Vec<Option<Row>> {
        let tray = self.tray_menu.as_ref().map_or(&[][..], |(_, _, rows)| rows);
        match menu {
            Menu::Dock => self.dock_menu_rows(),
            Menu::Input => self.input_menu_rows(),
            Menu::Desktop => self.desktop_menu_rows(),
            Menu::Drawer => self.drawer_menu_rows(),
            Menu::Window => self.window_menu_rows(),
            Menu::Overflow => self.overflow_menu_rows(),
            Menu::Battery(_) => self.battery_menu_rows(),
            Menu::Network(_) => network_rows(self.network.as_ref().and_then(|n| n.portal.as_deref())),
            _ => menu_rows(menu, "", tray, self.update_ready(), self.recent_menu.as_deref().unwrap_or_default()),
        }
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
        // ponytail: five of each, files that no longer exist skipped; the recent-files age setting comes later.
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

    /// The row of the open menu whose submenu is open.
    pub fn open_submenu(&self) -> Option<usize> {
        self.submenu.filter(|(popup, _)| Some(*popup) == self.popup).map(|(_, i)| i)
    }

    /// Starts the aim grace timer unless it runs: then the row under the pointer gets its submenu (`menu_hover`).
    fn aim_wait(&mut self) {
        if self.aim_timer.is_some() {
            return;
        }
        let timer = self.loop_handle.insert_source(Timer::from_duration(AIM_GRACE), |_, _, state| {
            state.aim_timer = None;
            let at = state.seat.get_pointer().map(|p| state.to_popup(state.to_output(p.current_location())));
            if let (Some(Popup::Menu(menu)), Some(i)) = (state.popup, at.and_then(|at| state.backend.as_ref()?.menu.row_at(at))) {
                state.menu_hover(menu, i);
            }
            TimeoutAction::Drop
        });
        self.aim_timer = timer.inspect_err(|err| tracing::warn!("menu aim timer: {err}")).ok();
    }

    fn aim_cancel(&mut self) {
        if let Some(token) = self.aim_timer.take() {
            self.loop_handle.remove(token);
        }
    }

    /// The pointer or a click is on row `i` of the open `menu`: its submenu opens, any other closes.
    fn menu_hover(&mut self, menu: Menu, i: usize) {
        let has_sub = self.open_menu_rows(menu).get(i).and_then(Option::as_ref).is_some_and(|r| !r.sub.is_empty());
        let next = has_sub.then_some(i);
        if next == self.open_submenu() {
            return;
        }
        self.submenu = next.zip(self.popup).map(|(i, popup)| (popup, i));
        // The app may fill a submenu in only when asked (AboutToShow).
        if let (Some(i), Menu::Tray(_), Some((service, path, rows))) = (next, menu, &self.tray_menu)
            && let Some(Some(row)) = rows.get(i)
        {
            self.services.send(Command::TrayMenu { service: service.clone(), menu: path.clone(), parent: row.id });
        }
        self.schedule_redraw();
    }

    /// The keys of an open menu: Up/Down light the next live row, Right or Return open its submenu or run it, Left
    /// or Esc back out of a submenu, a letter (`letters`: no shortcut has it) lights the next row starting with it.
    /// Left/Right on a bar menu with no submenu in the way go to the neighbouring bar menu. `true`: used.
    pub fn menu_key(&mut self, sym: smithay::input::keyboard::Keysym, letters: bool) -> bool {
        use smithay::input::keyboard::Keysym;
        let Some(popup @ Popup::Menu(menu)) = self.popup else { return false };
        let rows = self.open_menu_rows(menu);
        let open = self.open_submenu();
        let (in_sub, at) = self.menu_at();
        let level = match open.filter(|_| in_sub) {
            Some(i) => rows.get(i).cloned().flatten().map(|r| r.sub).unwrap_or_default(),
            None => rows,
        };
        let row = at.and_then(|i| level.get(i).cloned().flatten());
        let has_sub = row.as_ref().is_some_and(|r| !r.sub.is_empty());
        let bar = popup.item().filter(|_| !in_sub);
        match sym {
            Keysym::Up | Keysym::Down => self.menu_cursor_to(popup, in_sub, menu_step(&level, at, sym == Keysym::Down)),
            Keysym::Left | Keysym::Escape if open.is_some() => {
                self.submenu = None;
                self.menu_cursor = open.map(|i| (popup, false, i));
                self.schedule_redraw();
            }
            // Esc with no submenu closes the menu (`close_popup`).
            Keysym::Escape => return false,
            Keysym::Left | Keysym::Right if bar.is_some() && !(sym == Keysym::Right && has_sub) => {
                let items: Vec<_> = self.popup_output().and_then(|o| self.bar_view(&o)).map(|b| b.items().collect()).unwrap_or_default();
                let next = bar.and_then(|b| bar_step(&items, b, sym == Keysym::Right, |i| self.tray.get(i).is_some_and(|t| t.menu.is_some())));
                if let Some(next) = next {
                    self.menu_cursor = None;
                    // A tray item's menu: `secondary` asks for it rather than activating the item.
                    self.open_bar_item(next, Point::default(), true);
                    self.schedule_redraw();
                }
            }
            Keysym::Right | Keysym::Return | Keysym::KP_Enter => match (row, at) {
                (Some(row), Some(i)) if has_sub && !in_sub => {
                    self.menu_hover(menu, i);
                    // Into its first live row; a tray submenu still being filled in keeps the parent lit.
                    self.menu_cursor = Some(match menu_step(&row.sub, None, true) {
                        Some(j) => (popup, true, j),
                        None => (popup, false, i),
                    });
                    self.schedule_redraw();
                }
                (Some(Row { action: Some(action), .. }), _) => {
                    self.popup = None;
                    self.menu_cursor = None;
                    self.run_action(action);
                    self.schedule_redraw();
                }
                _ => {}
            },
            Keysym::Left => {}
            _ => match sym.key_char().filter(|c| letters && c.is_alphanumeric()) {
                Some(c) => self.menu_cursor_to(popup, in_sub, menu_jump(&level, at, c)),
                None => return false,
            },
        }
        true
    }

    /// The level the keyboard is in (`true`: the open submenu) and its lit row: the keyboard's, else the pointer's.
    fn menu_at(&self) -> (bool, Option<usize>) {
        let open = self.open_submenu().is_some();
        match self.menu_cursor {
            Some((popup, sub, i)) if Some(popup) == self.popup && (open || !sub) => return (sub, Some(i)),
            _ => {}
        }
        let Some(backend) = self.backend.as_ref() else { return (false, None) };
        let at = self.seat.get_pointer().map(|p| self.to_popup(self.to_output(p.current_location()))).unwrap_or_default();
        match backend.submenu.row_at(at).filter(|_| open) {
            Some(j) => (true, Some(j)),
            None => (false, backend.menu.row_at(at)),
        }
    }

    /// Lights row `to` (if any) of the menu (`in_sub` false; its open submenu closes) or of its submenu, scrolled
    /// into view.
    fn menu_cursor_to(&mut self, popup: Popup, in_sub: bool, to: Option<usize>) {
        let Some(i) = to else { return };
        if !in_sub {
            self.submenu = None;
        }
        self.menu_cursor = Some((popup, in_sub, i));
        if let Some(b) = self.backend.as_mut() {
            if in_sub { b.submenu.reveal(i) } else { b.menu.reveal(i) }
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
                let origin = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map_or((0, 0).into(), |g| g.loc);
                let at = location.to_i32_round() + origin;
                Command::TrayActivate { service: item.service.clone(), path: item.path.clone(), x: at.x, y: at.y }
            }
        };
        self.services.send(command);
    }

    /// A tray menu's rows arrived: opens it under its item, or (`parent` ≠ 0) fills in that submenu of the open one.
    pub fn show_tray_menu(&mut self, service: String, menu: String, parent: i32, rows: Vec<Option<MenuRow>>) {
        if parent != 0 {
            let open = self.tray_menu.as_mut().filter(|(s, m, _)| *s == service && *m == menu);
            if let Some(row) = open.and_then(|(_, _, all)| all.iter_mut().flatten().find(|r| r.id == parent)) {
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

    /// Shows `osd` for 1.5 s after the latest call.
    // ponytail: fades in (render::motion::Entrances) but goes at once; a fade out needs its last elements kept.
    pub fn show_osd(&mut self, osd: Osd) {
        if let Some(token) = self.osd_timer.take() {
            self.loop_handle.remove(token);
        }
        self.osd = Some(osd);
        let hide = self.loop_handle.insert_source(Timer::from_duration(OSD_TIME), |_, _, state| {
            state.osd = None;
            state.osd_timer = None;
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
    fn portal_menu_opens_the_check_url() {
        let rows = network_rows(Some("http://check.example/"));
        assert_eq!(rows[0].as_ref().unwrap().action, Some(Action::Spawn("xdg-open http://check.example/".into())));
        assert_eq!(network_rows(None)[0].as_ref().unwrap().action, None);
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
        let logo = menu_rows(Menu::Logo, "", &[], false, &rows);
        assert_eq!(logo.iter().flatten().find(|r| r.label == "Recent Items").map(|r| r.sub.len()), Some(rows.len()));
    }

    #[test]
    fn menus_stay_on_screen_and_stack_rows() {
        let rows = menu_rows(Menu::App, "Kate", &[], false, &[]);
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
        let rows = menu_rows(Menu::Logo, "", &[], false, &[]);
        let second = Size::from((1920, 1080));
        let (rect, cells) = menu_layout(Anchor::Below(1900), second, 240, &rows, 0);
        assert_eq!((rect.loc.x, rect.loc.y), (1920 - 8 - 240, menubar::height() + DROP));
        assert!(cells.iter().all(|c| rect.contains_rect(*c)));
        // Its submenu flips left of the menu rather than off the display.
        let row = Rectangle::new((rect.loc.x, 60).into(), (240, 22).into());
        assert_eq!(menu_layout(Anchor::Beside(row), second, 200, &rows, 0).0.loc.x, rect.loc.x - 200);
        let panel = control::layout(second, &control::Status::default()).rect;
        assert_eq!(panel.loc.x + panel.size.w, 1920 - MARGIN);
        assert_eq!(osd_rect(second.w).loc.x + 280, 1920 - MARGIN);
    }

    #[test]
    fn menu_rows_grow_with_the_text_size() {
        let rows = menu_rows(Menu::App, "Kate", &[], false, &[]);
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
        let rows = menu_rows(Menu::App, "Kate", &[], false, &[]);
        let output = Size::from((1280, 800));
        // The parent spans x 100..300; its row is at y 60.
        let row = Rectangle::new((100, 60).into(), (200, 22).into());
        let (rect, cells) = menu_layout(Anchor::Beside(row), output, 200, &rows, 0);
        assert_eq!((rect.loc.x, cells[0].loc.y), (300, 60), "to the right, first row level with the parent's");
        // No room on the right: to the left of the parent instead of over it.
        let row = Rectangle::new((1000, 60).into(), (200, 22).into());
        assert_eq!(menu_layout(Anchor::Beside(row), output, 200, &rows, 0).0.loc.x, 800);
    }

    #[test]
    fn tray_submenus_nest_one_level() {
        let row = |id, label: &str, submenu| Some(MenuRow { id, label: label.into(), enabled: true, submenu });
        let deep = row(3, "Deep", Some(vec![row(4, "Leaf", None)]));
        let tray = [row(1, "Open", None), row(2, "More", Some(vec![deep])), row(5, "Lazy", Some(Vec::new()))];
        let rows = tray_rows(&tray, 0);
        let labels = |rows: &[Option<Row>]| rows.iter().flatten().map(|r| r.label.clone()).collect::<Vec<_>>();
        assert_eq!(rows[0].as_ref().unwrap().action, Some(Action::TrayMenu(1)));
        let more = rows[1].as_ref().unwrap();
        assert_eq!((more.action.clone(), labels(&more.sub)), (None, vec!["Deep".to_owned(), "    Leaf".to_owned()]));
        assert_eq!(labels(&rows[2].as_ref().unwrap().sub), ["Empty"]);
    }

    #[test]
    fn keys_skip_separators_and_disabled_rows_and_stop_at_the_ends() {
        // About Kate (disabled), —, Hide Kate, Hide Others, Show All, —, Quit Kate.
        let rows = menu_rows(Menu::App, "Kate", &[], false, &[]);
        assert_eq!(menu_step(&rows, None, true), Some(2), "Down from nothing: the first live row");
        assert_eq!(menu_step(&rows, None, false), Some(6), "Up from nothing: the last");
        assert_eq!(menu_step(&rows, Some(4), true), Some(6), "over the separator");
        assert_eq!(menu_step(&rows, Some(2), false), Some(2), "the disabled first row is skipped; the top stays");
        assert_eq!(menu_step(&rows, Some(6), true), Some(6), "the bottom stays");
        // A submenu row is live though it has no action.
        let logo = menu_rows(Menu::Logo, "", &[], false, &recent_rows(&[], &[]));
        assert_eq!(menu_step(&logo, Some(2), true), Some(4));
        assert_eq!(menu_step(&[None, None], None, true), None);
    }

    #[test]
    fn letters_jump_to_the_next_row_starting_with_them() {
        let rows = menu_rows(Menu::App, "Kate", &[], false, &[]);
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
        // Tray 1 only activates; status items, Control Center and the clock open no menu.
        let menu = |i| i == 0;
        assert_eq!(bar_step(&items, BarItem::Logo, true, menu), Some(BarItem::App));
        assert_eq!(bar_step(&items, BarItem::Tray(0), true, menu), Some(BarItem::Input));
        assert_eq!(bar_step(&items, BarItem::Input, true, menu), Some(BarItem::Logo), "round");
        assert_eq!(bar_step(&items, BarItem::Logo, false, menu), Some(BarItem::Input));
        assert_eq!(bar_step(&items, BarItem::Tray(5), false, menu), None, "an item not on the bar");
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
