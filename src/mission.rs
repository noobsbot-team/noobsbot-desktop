//! Mission Control & App Exposé (E5.2): the shown Space's windows spread into a grid without overlap, the Spaces
//! bar on top. Windows are drawn from the textures the apps already gave us, scaled, so nothing extra is rendered
//! while it is closed (see render/mission.rs).

use std::time::Instant;

use smithay::{
    desktop::Window,
    input::keyboard::Keysym,
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Size},
};

use crate::{spaces::Tween, state::State, wm::Rect};

/// Between windows and around the grid.
pub const GAP: i32 = 32;
/// The Spaces bar, collapsed: name pills.
const PILL: (i32, i32) = (120, 24);
/// Expanded: thumbnails this tall, their names under them.
pub const THUMB_H: i32 = 96;
pub const NAME_H: i32 = 16;
const BAR_TOP: i32 = 12;
/// Windows start below the expanded bar, so expanding never moves them.
pub const AREA_TOP: i32 = BAR_TOP + THUMB_H + 4 + NAME_H + 20;
const BAR_GAP: i32 = 16;
/// The ✕ on a hovered Space.
const CLOSE: i32 = 20;

/// Mission Control while it is up (and while it closes).
pub struct Mission {
    /// App Exposé: only this app's windows.
    pub app: Option<String>,
    /// 0 closed → 1 open.
    pub progress: Tween,
    pub hover: Option<Window>,
    /// The Spaces bar shows thumbnails (the pointer is at the top).
    pub expanded: bool,
    /// The Space under the pointer.
    pub bar_hover: Option<usize>,
    /// A window pressed: where, and where the pointer is once it moved past the drag threshold.
    pub press: Option<Press>,
}

/// The pressed window, where the press was, and the pointer once it dragged past the threshold.
pub type Press = (Window, Point<f64, Logical>, Option<Point<f64, Logical>>);

/// The Spaces bar: one cell per Space (a pill or a thumbnail), and `+` (dynamic Spaces only).
#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    pub cells: Vec<Rect>,
    pub plus: Option<Rect>,
    pub expanded: bool,
}

/// The bar on an `output`-sized screen with `count` Spaces.
pub fn bar(output: Size<i32, Logical>, count: usize, expanded: bool, dynamic: bool) -> Bar {
    let size = if expanded { Size::from((THUMB_H * output.w / output.h.max(1), THUMB_H)) } else { Size::from(PILL) };
    let n = count as i32;
    let width = n * size.w + (n - 1).max(0) * BAR_GAP;
    let x = (output.w - width) / 2;
    let cells = (0..n).map(|i| Rectangle::new((x + i * (size.w + BAR_GAP), BAR_TOP + if expanded { 0 } else { 4 }).into(), size)).collect();
    let side = if expanded { 40 } else { PILL.1 };
    let plus = dynamic.then(|| {
        Rectangle::new((output.w - 2 * BAR_GAP - side, BAR_TOP + (if expanded { THUMB_H } else { PILL.1 + 8 } - side) / 2).into(), (side, side).into())
    });
    Bar { cells, plus, expanded }
}

/// The ✕ that removes the Space in `cell`: over its top-left corner.
pub fn close_button(cell: Rect) -> Rect {
    Rectangle::new((cell.loc.x - CLOSE / 3, cell.loc.y - CLOSE / 3).into(), (CLOSE, CLOSE).into())
}

/// Where each frame goes in `area`: rows of equal cells, each window shrunk (never grown) to fit its cell with its
/// shape kept, rows centred. Windows keep their reading order (top to bottom, then left to right), so they travel
/// the least. Returns one rect per frame, in input order; cells never overlap, so neither do the windows.
pub fn grid(frames: &[Rect], area: Rect, gap: i32) -> Vec<Rect> {
    let n = frames.len();
    if n == 0 {
        return Vec::new();
    }
    let fit = |cols: usize| {
        let rows = n.div_ceil(cols);
        let w = (area.size.w - gap * (cols as i32 - 1)) as f64 / cols as f64;
        let h = (area.size.h - gap * (rows as i32 - 1)) as f64 / rows as f64;
        (w, h)
    };
    let scale = |f: &Rect, (w, h): (f64, f64)| (w / f64::from(f.size.w.max(1))).min(h / f64::from(f.size.h.max(1))).min(1.0);
    // The column count that keeps the smallest window largest.
    let cols = (1..=n).max_by(|a, b| {
        let worst = |c: usize| frames.iter().map(|f| scale(f, fit(c))).fold(f64::INFINITY, f64::min);
        worst(*a).total_cmp(&worst(*b)).then(b.cmp(a))
    });
    let cols = cols.unwrap_or(1);
    let cell = fit(cols);
    let centre = |f: &Rect| (f.loc.y + f.size.h / 2, f.loc.x + f.size.w / 2);
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| centre(&frames[i]));
    let mut out = vec![Rect::default(); n];
    let rows: Vec<Vec<usize>> = order
        .chunks(cols)
        .map(|row| {
            let mut row = row.to_vec();
            row.sort_by_key(|&i| centre(&frames[i]).1);
            row
        })
        .collect();
    let sized = |i: usize| {
        let s = scale(&frames[i], cell);
        Size::<i32, Logical>::from(((f64::from(frames[i].size.w) * s).floor() as i32, (f64::from(frames[i].size.h) * s).floor() as i32))
    };
    let heights: Vec<i32> = rows.iter().map(|row| row.iter().map(|&i| sized(i).h).max().unwrap_or(0)).collect();
    let total = heights.iter().sum::<i32>() + gap * (rows.len() as i32 - 1);
    let mut y = area.loc.y + (area.size.h - total) / 2;
    for (row, height) in rows.iter().zip(&heights) {
        let width = row.iter().map(|&i| sized(i).w).sum::<i32>() + gap * (row.len() as i32 - 1);
        let mut x = area.loc.x + (area.size.w - width) / 2;
        for &i in row {
            let size = sized(i);
            out[i] = Rectangle::new((x, y + (height - size.h) / 2).into(), size);
            x += size.w + gap;
        }
        y += height + gap;
    }
    out
}

/// `a` → `b` at `t`.
pub fn lerp(a: Rect, b: Rect, t: f64) -> Rect {
    let mix = |a: i32, b: i32| (f64::from(a) + f64::from(b - a) * t).round() as i32;
    Rectangle::new((mix(a.loc.x, b.loc.x), mix(a.loc.y, b.loc.y)).into(), (mix(a.size.w, b.size.w), mix(a.size.h, b.size.h)).into())
}

/// One window in the overview: its real frame and where it is drawn this frame (both output-relative).
pub struct Thumb {
    pub window: Window,
    pub from: Rect,
    pub to: Rect,
}

/// Everything a Mission Control frame draws.
pub struct Overview {
    pub progress: f64,
    pub animating: bool,
    /// Topmost first.
    pub thumbs: Vec<Thumb>,
    /// The hovered thumb and its title.
    pub hover: Option<(usize, String)>,
    pub bar: Bar,
    pub names: Vec<String>,
    pub current: usize,
    pub bar_hover: Option<usize>,
    /// Other Spaces' windows, for their thumbnails: (Space, window, the output origin it is drawn against).
    pub others: Vec<(usize, Window, Point<i32, Logical>)>,
}

/// What the grid is made of.
struct Layout {
    windows: Vec<Window>,
    frames: Vec<Rect>,
    targets: Vec<Rect>,
    bar: Bar,
}

impl State {
    pub fn mission_open(&self) -> bool {
        self.mission.as_ref().is_some_and(|m| m.progress.target() == 1.0)
    }

    /// Super+↑ (and later the 3-finger swipe up): opens or closes Mission Control.
    pub fn toggle_mission_control(&mut self) {
        self.open_mission(None);
    }

    /// Super+↓: only the focused app's windows. Nothing focused: nothing to show.
    pub fn app_expose(&mut self) {
        let Some(app) = self.focused_window().and_then(|w| self.app_key(&w)) else { return };
        self.open_mission(Some(app));
    }

    fn open_mission(&mut self, app: Option<String>) {
        let now = Instant::now();
        match self.mission.as_mut() {
            Some(m) if m.progress.target() == 1.0 && m.app == app => m.progress = Tween::new(m.progress.value(now), 0.0),
            Some(m) => {
                m.progress = Tween::new(m.progress.value(now), 1.0);
                m.app = app;
            }
            None => {
                self.popup = None;
                self.spaces.slide = None;
                self.mission = Some(Mission { app, progress: Tween::new(0.0, 1.0), hover: None, expanded: false, bar_hover: None, press: None });
            }
        }
        self.schedule_redraw();
    }

    pub fn close_mission(&mut self) {
        if let Some(m) = self.mission.as_mut().filter(|m| m.progress.target() == 1.0) {
            m.progress = Tween::new(m.progress.value(Instant::now()), 0.0);
            m.press = None;
            self.schedule_redraw();
        }
    }

    fn mission_layout(&mut self) -> Option<Layout> {
        let mission = self.mission.as_ref()?;
        let output = self.space.outputs().next().and_then(|o| self.space.output_geometry(o))?;
        let windows: Vec<Window> = self
            .space
            .elements()
            .rev()
            .filter(|w| self.on_current(w) && w.geometry().size.w > 0)
            .filter(|w| mission.app.is_none() || self.app_key(w) == mission.app)
            .cloned()
            .collect();
        let frames: Vec<Rect> = windows.iter().filter_map(|w| self.frame(w)).map(|f| Rectangle::new(f.loc - output.loc, f.size)).collect();
        let (expanded, dynamic) = (mission.expanded, self.config.windows.spaces == 0);
        let bottom = output.size.h - crate::shell::dock::reserve(&self.config.dock);
        let area = Rectangle::new((GAP, AREA_TOP).into(), (output.size.w - 2 * GAP, bottom - AREA_TOP - GAP / 2).into());
        let targets = grid(&frames, area, GAP);
        let count = self.desks().map_or(1, |d| d.list.len());
        Some(Layout { windows, frames, targets, bar: bar(output.size, count, expanded, dynamic) })
    }

    /// This frame's overview, or `None` when closed (clearing a finished close).
    pub fn mission_frame(&mut self) -> Option<Overview> {
        let now = Instant::now();
        let mission = self.mission.as_ref()?;
        let (progress, done) = (mission.progress.value(now), mission.progress.done(now));
        if done && mission.progress.target() == 0.0 {
            self.mission = None;
            return None;
        }
        let layout = self.mission_layout()?;
        let mission = self.mission.as_ref()?;
        let dragged = mission.press.as_ref().and_then(|(w, at, to)| to.map(|to| (w.clone(), to - *at)));
        let mut thumbs: Vec<Thumb> = layout
            .windows
            .iter()
            .zip(layout.frames.iter().zip(&layout.targets))
            .map(|(window, (from, target))| {
                let mut to = lerp(*from, *target, progress);
                if let Some((_, by)) = dragged.as_ref().filter(|(w, _)| w == window) {
                    to.loc += by.to_i32_round();
                }
                Thumb { window: window.clone(), from: *from, to }
            })
            .collect();
        // The dragged window over the others.
        if let Some(i) = dragged.and_then(|(w, _)| thumbs.iter().position(|t| t.window == w)) {
            let thumb = thumbs.remove(i);
            thumbs.insert(0, thumb);
        }
        let hover = mission.hover.as_ref().filter(|_| done).and_then(|h| thumbs.iter().position(|t| t.window == *h));
        let bar_hover = mission.bar_hover;
        let hover = hover.map(|i| (i, crate::render::window_title(&thumbs[i].window)));
        let (names, current) = self.desk_names();
        let others = if layout.bar.expanded {
            self.space
                .elements()
                .filter(|w| !self.on_current(w))
                .map(|w| {
                    let row = self.window_row(w);
                    (crate::spaces::desk_of(row, current), w.clone(), self.row_origin(row))
                })
                .collect()
        } else {
            Vec::new()
        };
        Some(Overview { progress, animating: !done, thumbs, hover, bar: layout.bar, names, current, bar_hover, others })
    }

    /// The Space cell, its ✕ or `+` under `location`.
    fn bar_hit(&self, bar: &Bar, location: Point<f64, Logical>) -> Option<BarHit> {
        let hovered = self.mission.as_ref().and_then(|m| m.bar_hover);
        let removable = bar.cells.len() > 1 && self.config.windows.spaces == 0;
        if let Some(i) = hovered.filter(|i| removable && bar.cells.get(*i).is_some_and(|c| close_button(*c).to_f64().contains(location))) {
            return Some(BarHit::Close(i));
        }
        if bar.plus.is_some_and(|p| p.to_f64().contains(location)) {
            return Some(BarHit::Plus);
        }
        bar.cells.iter().position(|c| c.to_f64().contains(location)).map(BarHit::Space)
    }

    /// A press at `location` (output-relative). `true`: Mission Control is open and took it.
    pub fn mission_press(&mut self, location: Point<f64, Logical>, button: u32) -> bool {
        if !self.mission_open() {
            return false;
        }
        let Some(layout) = self.mission_layout() else { return true };
        if button != 0x110 {
            return true;
        }
        let window = layout.windows.iter().zip(&layout.targets).find(|(_, t)| t.to_f64().contains(location)).map(|(w, _)| w.clone());
        match (self.bar_hit(&layout.bar, location), window) {
            (Some(BarHit::Plus), _) => self.add_space(),
            (Some(BarHit::Close(i)), _) => self.remove_space(i),
            (Some(BarHit::Space(i)), _) => {
                self.show_space(i);
                self.close_mission();
            }
            (None, Some(window)) => {
                if let Some(m) = self.mission.as_mut() {
                    m.press = Some((window, location, None));
                }
            }
            // The empty desktop: back to it.
            (None, None) => self.close_mission(),
        }
        true
    }

    /// The pointer moved to `to` (output-relative): hover, bar expansion and window drags.
    pub fn mission_motion(&mut self, to: Point<f64, Logical>) {
        if !self.mission_open() {
            return;
        }
        let Some(layout) = self.mission_layout() else { return };
        let hover = layout.windows.iter().zip(&layout.targets).find(|(_, t)| t.to_f64().contains(to)).map(|(w, _)| w.clone());
        let bar_hover = layout.bar.cells.iter().position(|c| c.to_f64().contains(to));
        let Some(m) = self.mission.as_mut() else { return };
        // Expands at the top edge, collapses once the pointer is back among the windows.
        let expanded = if m.expanded { to.y < f64::from(AREA_TOP) } else { to.y < f64::from(BAR_TOP + PILL.1 + 8) };
        let mut changed = m.hover != hover || m.bar_hover != bar_hover || m.expanded != expanded;
        (m.hover, m.bar_hover, m.expanded) = (hover, bar_hover, expanded);
        if let Some((_, at, dragging)) = m.press.as_mut()
            && (dragging.is_some() || (to - *at).x.hypot((to - *at).y) > crate::wm::DRAG_THRESHOLD)
        {
            *dragging = Some(to);
            changed = true;
        }
        if changed {
            self.schedule_redraw();
        }
    }

    /// The left button went up at `location`: a click focuses the window and closes; a drag onto a Space moves the
    /// window there.
    pub fn mission_release(&mut self, location: Point<f64, Logical>) {
        let Some((window, _, dragged)) = self.mission.as_mut().and_then(|m| m.press.take()) else { return };
        match dragged {
            None => {
                self.focus_window(Some(window), SERIAL_COUNTER.next_serial());
                self.close_mission();
            }
            Some(_) => {
                let target = self.mission_layout().and_then(|l| l.bar.cells.iter().position(|c| c.to_f64().contains(location)));
                if let Some(desk) = target {
                    self.send_to_space(&window, desk);
                }
                self.schedule_redraw();
            }
        }
    }

    /// Esc closes Mission Control; Return opens the hovered window. `true`: the key was taken.
    pub fn mission_key(&mut self, key: Keysym) -> bool {
        if !self.mission_open() {
            return false;
        }
        match key {
            Keysym::Escape => self.close_mission(),
            Keysym::Return | Keysym::KP_Enter => {
                let hovered = self.mission.as_ref().and_then(|m| m.hover.clone());
                if let Some(window) = hovered {
                    self.focus_window(Some(window), SERIAL_COUNTER.next_serial());
                }
                self.close_mission();
            }
            _ => return false,
        }
        true
    }
}

enum BarHit {
    Space(usize),
    Close(usize),
    Plus,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn grid_never_overlaps_and_stays_inside() {
        let area = rect(32, 148, 1216, 500);
        for n in 1..=12 {
            let frames: Vec<Rect> = (0..n).map(|i| rect(40 * i, 30 * i, 600 + 37 * i % 300, 400 + 53 * i % 250)).collect();
            let out = grid(&frames, area, GAP);
            assert_eq!(out.len(), frames.len());
            for (i, a) in out.iter().enumerate() {
                assert!(area.contains_rect(*a), "{n} windows: {a:?} outside {area:?}");
                assert!(a.size.w > 0 && a.size.h > 0);
                for b in &out[i + 1..] {
                    assert!(!a.overlaps(*b), "{n} windows: {a:?} overlaps {b:?}");
                }
            }
            // Shapes are kept (to rounding).
            for (f, t) in frames.iter().zip(&out) {
                let (rf, rt) = (f64::from(f.size.w) / f64::from(f.size.h), f64::from(t.size.w) / f64::from(t.size.h));
                assert!((rf - rt).abs() < 0.02, "aspect {rf} became {rt}");
            }
        }
    }

    #[test]
    fn grid_never_grows_and_centres() {
        let area = rect(0, 0, 1000, 800);
        let out = grid(&[rect(10, 10, 400, 300)], area, GAP);
        assert_eq!(out, vec![rect(300, 250, 400, 300)]);
        assert!(grid(&[], area, GAP).is_empty());
    }

    #[test]
    fn grid_keeps_reading_order() {
        // Two side by side, swapped in the list: the left one stays left.
        let frames = [rect(700, 100, 400, 300), rect(0, 100, 400, 300)];
        let out = grid(&frames, rect(0, 0, 1000, 400), GAP);
        assert!(out[1].loc.x < out[0].loc.x);
    }

    #[test]
    fn bar_cells_are_centred_and_apart() {
        let output = Size::from((1280, 800));
        let collapsed = bar(output, 3, false, true);
        assert_eq!(collapsed.cells.len(), 3);
        let (first, last) = (collapsed.cells[0], collapsed.cells[2]);
        assert_eq!(first.loc.x, 1280 - (last.loc.x + last.size.w), "centred");
        assert!(!collapsed.cells[0].overlaps(collapsed.cells[1]));
        let expanded = bar(output, 3, true, true);
        assert_eq!(expanded.cells[0].size, Size::from((153, THUMB_H)), "thumbnails have the screen's shape");
        assert!(expanded.cells.iter().all(|c| c.loc.y + c.size.h + 4 + NAME_H < AREA_TOP));
        assert!(expanded.plus.is_some_and(|p| expanded.cells.iter().all(|c| !c.overlaps(p))));
        assert_eq!(bar(output, 3, false, false).plus, None, "a fixed count has no +");
    }

    #[test]
    fn lerp_goes_end_to_end() {
        let (a, b) = (rect(0, 0, 100, 100), rect(100, 50, 50, 20));
        assert_eq!(lerp(a, b, 0.0), a);
        assert_eq!(lerp(a, b, 1.0), b);
        assert_eq!(lerp(a, b, 0.5), rect(50, 25, 75, 60));
    }
}
