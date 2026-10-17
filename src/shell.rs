//! Shell popups opened from the menu bar (E2.2): which one is open, where it sits, and what a click on the bar or in
//! a popup does. Layout lives here as plain rectangles so drawing (render/) and clicks share it.

use chrono::{Datelike, Local, NaiveDate, Weekday};
use smithay::utils::{Logical, Point, Rectangle};

use crate::{
    render::menubar::{self, BarItem},
    state::State,
    wm::Rect,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Popup {
    /// The month shown, as (year, month 1–12).
    Calendar { year: i32, month: u32 },
}

/// Popups hang this far below the bar and keep this far from the screen's edge.
const DROP: i32 = 4;
const MARGIN: i32 = 8;

/// The calendar popover (E2.5), in logical px relative to its top-left corner.
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
#[derive(Debug, PartialEq)]
pub struct CalendarLayout {
    pub rect: Rect,
    pub title: Rect,
    pub prev: Rect,
    pub next: Rect,
    /// Column headers, then one rect per grid cell, row by row.
    pub header: [Rect; 7],
    pub cells: Vec<Rect>,
}

pub fn calendar_layout(output_width: i32, rows: usize) -> CalendarLayout {
    use calendar::*;
    let height = GRID_Y + rows as i32 * CELL_H + BOTTOM;
    // Under the clock, which is always the bar's last item: right-aligned to the screen edge.
    let x = (output_width - MARGIN - WIDTH).max(MARGIN);
    let at = |dx: i32, dy: i32, w: i32, h: i32| Rectangle::new((x + dx, menubar::HEIGHT + DROP + dy).into(), (w, h).into());
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
        let width = self.space.outputs().next().and_then(|o| self.space.output_geometry(o))?.size.w;
        match self.popup? {
            Popup::Calendar { year, month } => Some(calendar_layout(width, grid(year, month, self.config.clock.week_start.into()).len()).rect),
        }
    }

    /// Whether the shell, not an app, owns the pointer at `location` (output-relative): the bar or an open popup.
    pub fn shell_covers(&self, location: Point<f64, Logical>) -> bool {
        let on_bar = self.backend.as_ref().is_some_and(|b| b.menu_bar.visible()) && location.y < f64::from(menubar::HEIGHT);
        on_bar || self.popup_rect().is_some_and(|r| r.to_f64().contains(location))
    }

    /// A press at `location` (output-relative). `true`: the shell took it, so no window gets focus or the click.
    pub fn shell_press(&mut self, location: Point<f64, Logical>) -> bool {
        let item = self.backend.as_ref().and_then(|b| b.menu_bar.hit(location));
        let open = self.popup;
        let in_popup = self.popup_rect().is_some_and(|r| r.to_f64().contains(location));
        let taken = match (open, item) {
            (Some(Popup::Calendar { year, month }), _) if in_popup => {
                let width = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map_or(0, |g| g.size.w);
                let layout = calendar_layout(width, grid(year, month, self.config.clock.week_start.into()).len());
                if let Some((year, month)) = calendar_click(&layout, location, (year, month)) {
                    self.popup = Some(Popup::Calendar { year, month });
                }
                true
            }
            // The clock again closes its calendar, as on macOS.
            (Some(Popup::Calendar { .. }), Some(BarItem::Clock)) => {
                self.popup = None;
                true
            }
            (_, Some(BarItem::Clock)) => {
                let (year, month) = this_month();
                self.popup = Some(Popup::Calendar { year, month });
                true
            }
            // ponytail: the other items' menus (logo, app, Control Center) come next; until then a click only closes.
            (_, Some(_)) => {
                self.popup = None;
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

    /// Esc: closes the open popup. `true` if there was one.
    pub fn close_popup(&mut self) -> bool {
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
    fn layout_fits_under_the_bar_at_the_right() {
        let layout = calendar_layout(1280, 5);
        assert_eq!(layout.rect, Rectangle::new((992, 28).into(), (280, 228).into()));
        // Every cell and both arrows sit inside the popover.
        assert!(layout.cells.iter().chain([&layout.prev, &layout.next, &layout.title]).all(|c| layout.rect.contains_rect(*c)));
        assert_eq!(layout.cells.len(), 35);
        assert!(!layout.prev.overlaps(layout.next) && !layout.title.overlaps(layout.prev));
    }

    #[test]
    fn clicks_step_months() {
        let layout = calendar_layout(1280, 5);
        let centre = |r: Rect| r.to_f64().loc + r.to_f64().size.downscale(2.0).to_point();
        assert_eq!(calendar_click(&layout, centre(layout.next), (2026, 12)), Some((2027, 1)));
        assert_eq!(calendar_click(&layout, centre(layout.prev), (2026, 1)), Some((2025, 12)));
        assert_eq!(calendar_click(&layout, centre(layout.cells[10]), (2026, 10)), None);
        assert_eq!(calendar_click(&layout, centre(layout.title), (2020, 3)), Some(this_month()));
    }
}
