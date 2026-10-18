//! Shell popups opened from the menu bar (E2.2): which one is open, where it sits, and what a click on the bar or in
//! a popup does. Layout lives here as plain rectangles so drawing (render/) and clicks share it.

pub mod alert;
pub mod control;
pub mod notify;

use chrono::{Datelike, Local, NaiveDate, Weekday};
use smithay::utils::{Logical, Point, Rectangle};

use smithay::reexports::calloop::timer::{TimeoutAction, Timer};

use crate::{
    render::menubar::{self, BarItem},
    shortcuts::Action,
    state::State,
    symbols::Symbol,
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
    Rectangle::new(((output_width - MARGIN - 280).max(MARGIN), menubar::HEIGHT + DROP).into(), (280, 48).into())
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Popup {
    /// Notification Center (E4.2); its calendar shows (year, month 1–12).
    Notifications { year: i32, month: u32 },
    Menu(Menu),
    Control,
}

impl Popup {
    /// The bar item it hangs from, which shows the pressed pill.
    pub fn item(self) -> BarItem {
        match self {
            Popup::Notifications { .. } => BarItem::Clock,
            Popup::Menu(Menu::Logo) => BarItem::Logo,
            Popup::Menu(Menu::App) => BarItem::App,
            Popup::Control => BarItem::ControlCenter,
        }
    }
}

/// What a click on `item` opens.
// ponytail: the status items' own menus (Wi-Fi list, Bluetooth devices) come with module expansion; until then they
// open nothing.
fn popup_for(item: BarItem) -> Option<Popup> {
    match item {
        BarItem::Logo => Some(Popup::Menu(Menu::Logo)),
        BarItem::App => Some(Popup::Menu(Menu::App)),
        BarItem::Clock => {
            let (year, month) = this_month();
            Some(Popup::Notifications { year, month })
        }
        BarItem::ControlCenter => Some(Popup::Control),
        BarItem::Status(_) => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Menu {
    Logo,
    App,
}

/// A menu row (D8); `None` in a row list is a separator. No action: drawn disabled.
#[derive(Debug, PartialEq)]
pub struct Row {
    pub label: String,
    /// macOS-style glyphs; ⌘ is the desktop key.
    pub shortcut: &'static str,
    pub action: Option<Action>,
}

/// The rows of `menu`; `app` is the focused app's name.
// ponytail: rows whose window isn't built yet (About, Settings, Force Quit, the lock screen) are disabled until
// their milestone adds it.
pub fn menu_rows(menu: Menu, app: &str) -> Vec<Option<Row>> {
    let row = |label: String, shortcut, action| Some(Row { label, shortcut, action });
    match menu {
        Menu::Logo => vec![
            row("About This Computer".into(), "", None),
            row("System Settings…".into(), "", None),
            row("Software Update…".into(), "", None),
            None,
            row("Force Quit…".into(), "⌥⌘⎋", None),
            None,
            row("Sleep".into(), "", Some(Action::Sleep)),
            row("Restart…".into(), "", Some(Action::Power(alert::Power::Restart))),
            row("Shut Down…".into(), "", Some(Action::Power(alert::Power::ShutDown))),
            None,
            row("Lock Screen".into(), "⌃⌘Q", None),
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
    }
}

/// Menus (D8), in logical px.
pub mod menu {
    pub const PAD: i32 = 5;
    pub const ROW: i32 = 22;
    /// 1 px line with 5 px above and below.
    pub const SEPARATOR: i32 = 11;
    pub const SIDE: i32 = 12;
    pub const MIN_WIDTH: i32 = 180;
}

/// A `width`-wide menu under the bar, left edge at `anchor_x` but kept on screen; its rect and one rect per row.
pub fn menu_layout(anchor_x: i32, output_width: i32, width: i32, rows: &[Option<Row>]) -> (Rect, Vec<Rect>) {
    use menu::*;
    let x = anchor_x.min(output_width - MARGIN - width).max(MARGIN);
    let top = menubar::HEIGHT + DROP;
    let mut y = top + PAD;
    let rects = rows
        .iter()
        .map(|row| {
            let h = if row.is_some() { ROW } else { SEPARATOR };
            let rect = Rectangle::new((x + PAD, y).into(), (width - 2 * PAD, h).into());
            y += h;
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
}

/// The calendar with its top-left corner at (`x`, `y`), output-relative.
pub fn calendar_layout(x: i32, y: i32, rows: usize) -> CalendarLayout {
    use calendar::*;
    let height = GRID_Y + rows as i32 * CELL_H + BOTTOM;
    let at = |dx: i32, dy: i32, w: i32, h: i32| Rectangle::new((x + dx, y + dy).into(), (w, h).into());
    let next = at(WIDTH - PAD + 4 - ARROW, TITLE_Y - 2, ARROW, ARROW);
    let prev = at(WIDTH - PAD + 4 - 2 * ARROW, TITLE_Y - 2, ARROW, ARROW);
    CalendarLayout {
        rect: at(0, 0, WIDTH, height),
        title: at(PAD, TITLE_Y, WIDTH - 2 * PAD - 2 * ARROW, TITLE_H),
        prev,
        next,
        header: std::array::from_fn(|i| at(GRID_X + i as i32 * CELL_W, HEADER_Y, CELL_W, HEADER_H)),
        cells: (0..rows * 7).map(|i| at(GRID_X + (i % 7) as i32 * CELL_W, GRID_Y + (i / 7) as i32 * CELL_H, CELL_W, CELL_H)).collect(),
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

    /// The open popup's area, output-relative.
    pub fn popup_rect(&self) -> Option<Rect> {
        match self.popup? {
            Popup::Notifications { .. } => self.center_layout().map(|c| c.rect),
            // A menu's width follows its text, so the last frame's layout is the one to hit.
            Popup::Menu(_) => self.backend.as_ref()?.menu.rect(),
            Popup::Control => Some(self.control_layout().rect),
        }
    }

    /// Whether the shell, not an app, owns the pointer at `location` (output-relative): the bar or an open popup.
    pub fn shell_covers(&self, location: Point<f64, Logical>) -> bool {
        let on_alert = self.alert.is_some() && self.backend.as_ref().and_then(|b| b.alert.rect()).is_some_and(|r| r.to_f64().contains(location));
        if on_alert || self.on_banner(location) {
            return true;
        }
        let on_bar = self.backend.as_ref().is_some_and(|b| b.menu_bar.visible()) && location.y < f64::from(menubar::HEIGHT);
        on_bar || self.popup_rect().is_some_and(|r| r.to_f64().contains(location))
    }

    /// A press at `location` (output-relative). `true`: the shell took it, so no window gets focus or the click.
    pub fn shell_press(&mut self, location: Point<f64, Logical>) -> bool {
        // The alert is above everything else.
        if self.alert.is_some() && self.alert_press(location) {
            return true;
        }
        if self.banner_press(location) {
            return true;
        }
        let item = self.backend.as_ref().and_then(|b| b.menu_bar.hit(location));
        let open = self.popup;
        let in_popup = self.popup_rect().is_some_and(|r| r.to_f64().contains(location));
        let taken = match (open, item) {
            (Some(popup), _) if in_popup => {
                self.popup_press(popup, location);
                true
            }
            // Its own item again closes the popup, as on macOS.
            (_, Some(item)) => {
                self.popup = if open.map(Popup::item) == Some(item) { None } else { popup_for(item) };
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

    fn popup_press(&mut self, popup: Popup, location: Point<f64, Logical>) {
        match popup {
            Popup::Notifications { .. } => self.center_press(location),
            Popup::Menu(menu) => {
                let index = self.backend.as_ref().and_then(|b| b.menu.row_at(location));
                // Separators and disabled rows keep the menu open, as on macOS.
                if let Some(action) = index.and_then(|i| menu_rows(menu, "").into_iter().nth(i)).flatten().and_then(|row| row.action) {
                    self.popup = None;
                    self.run_action(action);
                }
            }
            Popup::Control => self.control_press(location),
        }
    }

    /// The pointer moved from `from` to `to` (output-relative). With a popup open, sliding along the bar opens the
    /// next item's popup, as on macOS; in a menu, the hovered row follows.
    pub fn shell_motion(&mut self, from: Point<f64, Logical>, to: Point<f64, Logical>) {
        if self.shell_drag.is_some() {
            return self.control_drag(to);
        }
        self.notify_motion(to);
        let Some(open) = self.popup else { return };
        let Some(backend) = self.backend.as_ref() else { return };
        if let Some(next) = backend.menu_bar.hit(to).filter(|item| *item != open.item()).and_then(popup_for) {
            self.popup = Some(next);
            self.schedule_redraw();
        } else if matches!(open, Popup::Menu(_)) && backend.menu.row_at(from) != backend.menu.row_at(to) {
            self.schedule_redraw();
        }
    }

    /// Shows `osd` for 1.5 s after the latest call.
    // ponytail: appears and goes without the `snappy` fade; animation comes with the spring module.
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
        let layout = calendar_layout(992, 28, 5);
        assert_eq!(layout.rect, Rectangle::new((992, 28).into(), (280, 228).into()));
        // Every cell and both arrows sit inside the popover.
        assert!(layout.cells.iter().chain([&layout.prev, &layout.next, &layout.title]).all(|c| layout.rect.contains_rect(*c)));
        assert_eq!(layout.cells.len(), 35);
        assert!(!layout.prev.overlaps(layout.next) && !layout.title.overlaps(layout.prev));
    }

    #[test]
    fn menus_stay_on_screen_and_stack_rows() {
        let rows = menu_rows(Menu::App, "Kate");
        let (rect, cells) = menu_layout(6, 1280, 200, &rows);
        // 5 rows of 22 + 2 separators of 11 + padding 5 at both ends.
        assert_eq!(rect, Rectangle::new((8, 28).into(), (200, 142).into()));
        assert_eq!(cells[0], Rectangle::new((13, 33).into(), (190, 22).into()));
        assert_eq!(cells[2].loc.y, 33 + 22 + 11);
        assert!(cells.iter().all(|c| rect.contains_rect(*c)));
        // Near the right edge it moves left to fit.
        assert_eq!(menu_layout(1200, 1280, 200, &rows).0.loc.x, 1280 - 8 - 200);
        assert_eq!(rows[2].as_ref().map(|r| (r.label.as_str(), r.action.clone())), Some(("Hide Kate", Some(Action::Hide))));
    }

    #[test]
    fn clicks_step_months() {
        let layout = calendar_layout(992, 28, 5);
        let centre = |r: Rect| r.to_f64().loc + r.to_f64().size.downscale(2.0).to_point();
        assert_eq!(calendar_click(&layout, centre(layout.next), (2026, 12)), Some((2027, 1)));
        assert_eq!(calendar_click(&layout, centre(layout.prev), (2026, 1)), Some((2025, 12)));
        assert_eq!(calendar_click(&layout, centre(layout.cells[10]), (2026, 10)), None);
        assert_eq!(calendar_click(&layout, centre(layout.title), (2020, 3)), Some(this_month()));
    }
}
