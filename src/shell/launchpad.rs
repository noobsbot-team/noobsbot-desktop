//! Launchpad (E4.4): every app in a full-screen grid of pages over the dimmed wallpaper, a search field on top. Opens
//! from its dock icon or the `launchpad` binding (F4, off by default); Esc, a click on empty space or the binding
//! again closes it. Pages slide with a two-finger horizontal swipe, the wheel, ←/→ or a click on a page dot. Typing
//! filters; Return opens the first match. Animations run only while something moves.

use std::time::{Duration, Instant};

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
    /// Icon to name, and the name line (Callout).
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
        let block = self.icon + layout::NAME_GAP + layout::NAME_H;
        let (x, y) = (cell.loc.x + (cell.size.w - self.icon) / 2, cell.loc.y + (cell.size.h - block) / 2);
        Rectangle::new((x, y).into(), (self.icon, self.icon).into())
    }

    pub fn name_rect(&self, i: usize) -> Rect {
        let (cell, icon) = (self.cells[i], self.icon_rect(i));
        Rectangle::new((cell.loc.x + 4, icon.loc.y + self.icon + layout::NAME_GAP).into(), (cell.size.w - 8, layout::NAME_H).into())
    }

    /// One dot per page, centred.
    pub fn dots(&self, pages: usize) -> Vec<Rect> {
        use layout::{DOT, DOT_STEP};
        let w = pages as i32 * DOT_STEP;
        let x0 = (self.width - w) / 2 + (DOT_STEP - DOT) / 2;
        (0..pages as i32).map(|i| Rectangle::new((x0 + i * DOT_STEP, self.dots_y - DOT / 2).into(), (DOT, DOT).into())).collect()
    }

    /// The cell whose icon or name is at `p`, on the page in view.
    pub fn hit(&self, p: Point<f64, Logical>) -> Option<usize> {
        (0..self.cells.len()).find(|&i| self.icon_rect(i).to_f64().contains(p) || self.name_rect(i).to_f64().contains(p))
    }
}

/// The grid for an `output`-sized screen with `bottom` px kept for the dock.
pub fn grid(output: Size<i32, Logical>, bottom: i32) -> Grid {
    use layout::*;
    let cols = (((output.w - 2 * SIDE) / COLUMN).max(0) as usize).clamp(COLS.0, COLS.1);
    let rows = (((output.h - FIRST_ROW) / ROW + 1).max(0) as usize).clamp(ROWS.0, ROWS.1);
    let icon = if output.w < NARROW { SMALL_ICON } else { ICON };
    let top = FIELD_Y + FIELD_H + 24;
    let bottom = output.h - bottom - DOTS_H;
    // Cells no wider or taller than the steps above, so a big screen keeps the grid together, centred.
    let cell_w = ((output.w - 2 * SIDE) / cols as i32).clamp(1, COLUMN);
    let cell_h = ((bottom - top) / rows as i32).min(ROW).max(icon + NAME_GAP + NAME_H);
    let (x0, y0) = ((output.w - cell_w * cols as i32) / 2, top + ((bottom - top) - cell_h * rows as i32).max(0) / 2);
    let cells = (0..rows as i32)
        .flat_map(|r| (0..cols as i32).map(move |c| Rectangle::new((x0 + c * cell_w, y0 + r * cell_h).into(), (cell_w, cell_h).into())))
        .collect();
    let field = Rectangle::new(((output.w - FIELD_W) / 2, FIELD_Y).into(), (FIELD_W, FIELD_H).into());
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
    pub page: usize,
    /// 0 closed → 1 open: icons zoom from 1.1 and fade in.
    pub shown: Spring,
    /// The page position the slide is at.
    pub slide: Spring,
    /// A finger swipe in progress, in pages (positive: towards the next page).
    pub drag: f64,
    /// Ends the swipe after `SWIPE_END` without events.
    swipe_end: Option<RegistrationToken>,
}

impl Launchpad {
    fn new(now: Instant) -> Self {
        Self { field: TextField::default(), page: 0, shown: Spring::new(0.0, 1.0, now), slide: Spring::new(0.0, 0.0, now), drag: 0.0, swipe_end: None }
    }

    pub fn closing(&self) -> bool {
        self.shown.to == 0.0
    }

    /// The page position in view.
    pub fn position(&self, now: Instant) -> f64 {
        self.slide.at(now) + self.drag
    }

    pub fn animating(&self, now: Instant) -> bool {
        !self.shown.done(now) || !self.slide.done(now)
    }

    /// Slides to `page`, kept within `pages`.
    fn turn_to(&mut self, page: usize, pages: usize, now: Instant) {
        let page = page.min(pages.saturating_sub(1));
        self.slide = Spring::new(self.position(now), page as f64, now);
        (self.page, self.drag) = (page, 0.0);
    }
}

impl State {
    /// What the grid shows: the launchable apps matching the search.
    pub fn launchpad_apps(&self) -> Vec<&App> {
        self.launchpad.as_ref().map_or_else(Vec::new, |l| filter(self.apps.list(), &l.field.text))
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
        let Some(l) = self.launchpad.as_ref() else { return };
        if l.closing() && l.shown.done(now) {
            self.launchpad = None;
        } else if l.animating(now) {
            self.schedule_redraw();
        }
    }

    fn launchpad_pages(&self) -> usize {
        let n = self.launchpad_apps().len();
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
            self.launch(Some(&app));
        }
        self.close_launchpad();
    }

    /// A key while open (all keys but desktop shortcuts come here): typing searches, ←/→ turn pages while the field is
    /// empty, Return opens the first match, Esc clears the search, then closes.
    pub fn launchpad_key(&mut self, sym: Keysym, ctrl: bool) {
        let Some(l) = self.launchpad.as_mut() else { return };
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
                let first = self.launchpad_apps().first().map(|a| (*a).clone());
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
        let apps = self.launchpad_apps();
        let pages = pages(apps.len(), grid.per_page());
        if pages > 1
            // A dot is small: its hit area reaches 6 px around it.
            && let Some(dot) = grid.dots(pages).iter().position(|d| Rectangle::new(d.loc - Point::from((6, 6)), d.size + Size::from((12, 12))).to_f64().contains(location))
        {
            self.launchpad_turn(dot);
            return true;
        }
        let app = grid.hit(location).and_then(|i| apps.get(page * grid.per_page() + i)).map(|a| (*a).clone());
        match app {
            Some(app) => self.launchpad_launch(Some(app)),
            None => {
                self.close_launchpad();
            }
        }
        true
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
        App { id: name.to_lowercase(), name: name.into(), exec: name.to_lowercase(), icon: String::new(), wm_class: None, no_display: false, ..Default::default() }
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
    fn filters_by_name() {
        let mut hidden = app("Settings Daemon");
        hidden.no_display = true;
        let apps = [app("Ark"), app("Dolphin"), app("Firefox"), app("Kate"), app("KDE Partition Manager"), hidden];
        let names = |q: &str| filter(&apps, q).iter().map(|a| a.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names(""), ["Ark", "Dolphin", "Firefox", "Kate", "KDE Partition Manager"], "no NoDisplay apps");
        assert_eq!(names("k"), ["Kate", "KDE Partition Manager", "Ark"], "prefix matches first");
        assert_eq!(names("FIRE"), ["Firefox"]);
        assert_eq!(names(" set"), Vec::<&str>::new());
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
