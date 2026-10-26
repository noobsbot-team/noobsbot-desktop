//! Mission Control & App Exposé (E5.2): on every display at once (E8.7), the shown Space's windows spread into a grid without overlap, the Spaces
//! bar on top. Windows are drawn from the textures the apps already gave us, scaled, so nothing extra is rendered
//! while it is closed (see render/mission.rs).

use std::time::Instant;

use smithay::{
    desktop::Window,
    input::keyboard::{Keysym, ModifiersState},
    output::Output,
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Size},
};

use crate::{spaces::Tween, state::State, text::ts, wm::Rect};

/// Between windows and around the grid.
pub const GAP: i32 = 32;
/// The Spaces bar, collapsed: name pills, grown with the text size (`pill`).
const PILL: (i32, i32) = (120, 24);
/// Expanded: thumbnails this tall, their names under them (`NAME_H` grown with the text size).
pub const THUMB_H: i32 = 96;
pub const NAME_H: i32 = 16;
const BAR_TOP: i32 = 12;

/// A collapsed name pill, grown with the text size.
fn pill() -> Size<i32, Logical> {
    Size::from((ts(PILL.0), ts(PILL.1)))
}

/// Windows start below the expanded bar, so expanding never moves them.
pub fn area_top() -> i32 {
    BAR_TOP + THUMB_H + 4 + ts(NAME_H) + 20
}
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
    /// The display under the pointer: the one whose bar expands and is hovered.
    pub output: String,
    /// The Spaces bar shows thumbnails (the pointer is at the top).
    pub expanded: bool,
    /// The Space under the pointer.
    pub bar_hover: Option<usize>,
    /// A window pressed: where, and where the pointer is once it moved past the drag threshold.
    pub press: Option<Press>,
    /// A Space pressed in the bar: its place now, where the press was, and whether it is being dragged (E8.7).
    pub space_press: Option<(usize, Point<f64, Logical>, bool)>,
    /// Every Space as a large tile with its windows inside (Ctrl+↑ or the button at the bar's left end).
    pub all: bool,
}

/// The pressed window, where the press was, and the pointer once it dragged past the threshold.
pub type Press = (Window, Point<f64, Logical>, Option<Point<f64, Logical>>);

/// The Spaces bar: one cell per Space (a pill or a thumbnail), and `+` (dynamic Spaces only).
#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    pub cells: Vec<Rect>,
    pub plus: Option<Rect>,
    /// The every-Space view's button, mirroring `+` at the left end.
    pub all: Rect,
    pub expanded: bool,
}

/// The bar on an `output`-sized screen with `count` Spaces.
pub fn bar(output: Size<i32, Logical>, count: usize, expanded: bool, dynamic: bool) -> Bar {
    let pill = pill();
    let mut size = if expanded { Size::from((THUMB_H * output.w / output.h.max(1), THUMB_H)) } else { pill };
    let n = count as i32;
    let side = if expanded { 40 } else { pill.h };
    // Many Spaces on a narrow screen: the cells shrink (thumbnails keep their shape) so `+` stays clear of them.
    // ponytail: past 24 px a cell the bar overflows anyway; nobody has 40 Spaces on a phone.
    let room = output.w - 2 * (3 * BAR_GAP + side);
    let fit = ((room - (n - 1).max(0) * BAR_GAP) / n.max(1)).max(24);
    if fit < size.w {
        size = Size::from((fit, if expanded { THUMB_H * fit / size.w } else { size.h }));
    }
    let width = n * size.w + (n - 1).max(0) * BAR_GAP;
    let x = (output.w - width) / 2;
    let cells = (0..n).map(|i| Rectangle::new((x + i * (size.w + BAR_GAP), BAR_TOP + if expanded { 0 } else { 4 }).into(), size)).collect();
    let plus = dynamic.then(|| {
        Rectangle::new((output.w - 2 * BAR_GAP - side, BAR_TOP + (if expanded { THUMB_H } else { pill.h + 8 } - side) / 2).into(), (side, side).into())
    });
    let y = BAR_TOP + (if expanded { THUMB_H } else { pill.h + 8 } - side) / 2;
    let all = Rectangle::new((2 * BAR_GAP, y).into(), (side, side).into());
    Bar { cells, plus, all, expanded }
}

/// The place among `cells` a Space dragged to `x` takes: the cell with the nearest centre.
pub fn slot(cells: &[Rect], x: f64) -> usize {
    let centre = |c: &Rect| f64::from(c.loc.x) + f64::from(c.size.w) / 2.0;
    (0..cells.len()).min_by(|a, b| (centre(&cells[*a]) - x).abs().total_cmp(&(centre(&cells[*b]) - x).abs())).unwrap_or(0)
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

/// The rect nearest `from` in direction `dir` (a unit step such as `(1, 0)`): its centre must lie that way; distance
/// along `dir` counts once, sideways twice, so the next in the same row or column wins over a diagonal.
pub fn nearest(rects: &[Rect], from: usize, dir: (i32, i32)) -> Option<usize> {
    let centre = |r: &Rect| (r.loc.x * 2 + r.size.w, r.loc.y * 2 + r.size.h);
    let (fx, fy) = centre(rects.get(from)?);
    rects
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != from)
        .filter_map(|(i, r)| {
            let (dx, dy) = (centre(r).0 - fx, centre(r).1 - fy);
            let along = dx * dir.0 + dy * dir.1;
            let side = (dx * dir.1 - dy * dir.0).abs();
            (along > 0).then_some((along + 2 * side, i))
        })
        .min()
        .map(|(_, i)| i)
}

/// Reading order of `rects` (top to bottom, then left to right) for Tab.
fn reading_order(rects: &[Rect]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..rects.len()).collect();
    order.sort_by_key(|&i| (rects[i].loc.y, rects[i].loc.x));
    order
}

/// The every-Space view: `count` tiles with the screen's shape, as large as fit in `area`, in rows, in order.
pub fn tiles(output: Size<i32, Logical>, count: usize, area: Rect) -> Vec<Rect> {
    grid(&vec![Rectangle::from_size(output); count], area, GAP)
}

/// Where a window with on-screen `frame` (output-relative) sits in `tile`, a shrunk `output`.
pub fn in_tile(frame: Rect, output: Size<i32, Logical>, tile: Rect) -> Rect {
    let f = f64::from(tile.size.w) / f64::from(output.w.max(1));
    let s = |v: i32| (f64::from(v) * f).round() as i32;
    Rectangle::new((tile.loc.x + s(frame.loc.x), tile.loc.y + s(frame.loc.y)).into(), (s(frame.size.w), s(frame.size.h)).into())
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

    pub(crate) fn open_mission(&mut self, app: Option<String>) {
        let (now, reduce) = (Instant::now(), self.config.appearance.reduce_motion);
        match self.mission.as_mut() {
            Some(m) if m.progress.target() == 1.0 && m.app == app => m.progress = Tween::eased(m.progress.value(now), 0.0, reduce),
            Some(m) => {
                m.progress = Tween::eased(m.progress.value(now), 1.0, reduce);
                m.app = app;
            }
            None => {
                self.popup = None;
                self.spaces.slide = None;
                let output = self.pointer_output().map(|o| o.name()).unwrap_or_default();
                self.mission = Some(Mission {
                    app,
                    progress: Tween::eased(0.0, 1.0, reduce),
                    hover: None,
                    output,
                    expanded: false,
                    bar_hover: None,
                    press: None,
                    space_press: None,
                    all: false,
                });
            }
        }
        self.schedule_redraw();
    }

    /// A swipe up or down holds Mission Control at `p` (0 closed → 1 open), opening it first.
    pub fn mission_hold(&mut self, p: f64) {
        if self.mission.is_none() {
            self.open_mission(None);
        }
        if let Some(m) = self.mission.as_mut() {
            m.progress = Tween::hold(p.clamp(0.0, 1.0));
        }
        self.schedule_redraw();
    }

    /// The fingers lifted, moving at `v` per second: it springs open or closed from where they left it.
    pub fn mission_let_go(&mut self, open: bool, v: f64) {
        let Some(m) = self.mission.as_mut() else { return };
        m.progress = Tween::fling(m.progress.value(Instant::now()), if open { 1.0 } else { 0.0 }, v);
        if !open {
            (m.press, m.space_press) = (None, None);
        }
        self.schedule_redraw();
    }

    pub fn close_mission(&mut self) {
        let reduce = self.config.appearance.reduce_motion;
        if let Some(m) = self.mission.as_mut().filter(|m| m.progress.target() == 1.0) {
            m.progress = Tween::eased(m.progress.value(Instant::now()), 0.0, reduce);
            m.press = None;
            m.space_press = None;
            self.schedule_redraw();
        }
    }

    /// `output`'s overview: its shown windows and its Spaces bar.
    fn mission_layout(&mut self, on: &Output) -> Option<Layout> {
        let mission = self.mission.as_ref()?;
        let output = self.space.output_geometry(on)?;
        let all = mission.all;
        let first = self.space.outputs().next() == Some(on);
        let windows: Vec<Window> = self
            .space
            .elements()
            .rev()
            .filter(|w| self.window_output(w).as_ref().unwrap_or(on) == on)
            .filter(|w| all || self.on_current(w))
            .filter(|w| w.geometry().size.w > 0 && self.frame(w).is_some())
            .filter(|w| mission.app.is_none() || self.app_key(w) == mission.app)
            .cloned()
            .collect();
        let frames: Vec<Rect> = windows.iter().filter_map(|w| self.frame(w)).map(|f| Rectangle::new(f.loc - output.loc, f.size)).collect();
        let (expanded, dynamic) = (mission.expanded && mission.output == on.name(), self.config.windows.spaces == 0);
        // The dock is on the first display only.
        let bottom = output.size.h - if first { crate::shell::dock::reserve(&self.config.dock) } else { 0 };
        let top = area_top();
        let area = Rectangle::new((GAP, top).into(), (output.size.w - 2 * GAP, bottom - top - GAP / 2).into());
        let key = self.output_key(on);
        let (count, current) = {
            let d = self.desks_at(&key);
            (d.list.len(), d.current)
        };
        let mut bar = bar(output.size, count, expanded || all, dynamic);
        let targets = if all {
            // Below the bar's buttons, room for the last row's names.
            let top = bar.all.loc.y + bar.all.size.h + BAR_GAP;
            bar.cells = tiles(output.size, count, Rectangle::new((GAP, top).into(), (output.size.w - 2 * GAP, bottom - top - GAP / 2 - ts(NAME_H) - 4).into()));
            windows
                .iter()
                .zip(&frames)
                .map(|(w, f)| {
                    let row = self.window_row(w);
                    let tile = bar.cells[crate::spaces::desk_of(row, current).min(count - 1)];
                    in_tile(Rectangle::new(f.loc - Point::from((0, row * crate::spaces::STRIDE)), f.size), output.size, tile)
                })
                .collect()
        } else {
            grid(&frames, area, GAP)
        };
        Some(Layout { windows, frames, targets, bar })
    }

    /// This frame's overview on `output`, or `None` when closed (clearing a finished close).
    pub fn mission_frame(&mut self, output: &Output) -> Option<Overview> {
        let now = Instant::now();
        let mission = self.mission.as_ref()?;
        // A fling's overshoot stops at open and closed.
        let (progress, done) = (mission.progress.value(now).clamp(0.0, 1.0), mission.progress.done(now));
        if done && mission.progress.target() == 0.0 {
            self.mission = None;
            return None;
        }
        let layout = self.mission_layout(output)?;
        let mission = self.mission.as_ref()?;
        let here = mission.output == output.name();
        let all = mission.all;
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
        let bar_hover = mission.bar_hover.filter(|_| here);
        let hover = hover.map(|i| (i, crate::render::window_title(&thumbs[i].window)));
        let key = self.output_key(output);
        let (names, current) = self.desk_names_at(&key);
        let others = if layout.bar.expanded && !all {
            self.space
                .elements()
                .filter(|w| !self.on_current(w) && self.window_output(w).as_ref().unwrap_or(output) == output)
                .map(|w| {
                    let row = self.window_row(w);
                    (crate::spaces::desk_of(row, current), w.clone(), self.row_origin(output, row))
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
        if bar.all.to_f64().contains(location) {
            return Some(BarHit::All);
        }
        bar.cells.iter().position(|c| c.to_f64().contains(location)).map(BarHit::Space)
    }

    /// The display under global `at`, and `at` relative to it.
    fn mission_local(&self, at: Point<f64, Logical>) -> Option<(Output, Point<f64, Logical>)> {
        let output = self.space.output_under(at).next()?.clone();
        let origin = self.space.output_geometry(&output)?.loc.to_f64();
        Some((output, at - origin))
    }

    /// A press at global `at`. `true`: Mission Control is open and took it.
    pub fn mission_press(&mut self, at: Point<f64, Logical>, button: u32) -> bool {
        if !self.mission_open() {
            return false;
        }
        let Some((output, location)) = self.mission_local(at) else { return true };
        let Some(layout) = self.mission_layout(&output) else { return true };
        if button != 0x110 {
            return true;
        }
        let window = layout.windows.iter().zip(&layout.targets).find(|(_, t)| t.to_f64().contains(location)).map(|(w, _)| w.clone());
        match (self.bar_hit(&layout.bar, location), window) {
            (Some(BarHit::Plus), _) => self.add_space(),
            (Some(BarHit::Close(i)), _) => self.remove_space(i),
            (Some(BarHit::All), _) => self.mission_all(),
            // Windows sit inside the every-Space view's tiles: they come first.
            (_, Some(window)) => {
                if let Some(m) = self.mission.as_mut() {
                    m.press = Some((window, location, None));
                }
            }
            // Shown on release; dragged, it moves along the bar.
            (Some(BarHit::Space(i)), None) => {
                if let Some(m) = self.mission.as_mut() {
                    m.space_press = Some((i, location, false));
                }
            }
            // The empty desktop: back to it.
            (None, None) => self.close_mission(),
        }
        true
    }

    /// The pointer moved to global `at`: hover, bar expansion and window drags.
    pub fn mission_motion(&mut self, at: Point<f64, Logical>) {
        if !self.mission_open() {
            return;
        }
        let Some((output, to)) = self.mission_local(at) else { return };
        if let Some(m) = self.mission.as_mut().filter(|m| m.output != output.name()) {
            m.output = output.name();
            m.expanded = false;
            (m.press, m.space_press) = (None, None);
        }
        let Some(layout) = self.mission_layout(&output) else { return };
        let hover = layout.windows.iter().zip(&layout.targets).find(|(_, t)| t.to_f64().contains(to)).map(|(w, _)| w.clone());
        let bar_hover = layout.bar.cells.iter().position(|c| c.to_f64().contains(to));
        let Some(m) = self.mission.as_mut() else { return };
        // Expands at the top edge, collapses once the pointer is back among the windows.
        let expanded = if m.expanded { to.y < f64::from(area_top()) } else { to.y < f64::from(BAR_TOP + pill().h + 8) };
        let mut changed = m.hover != hover || m.bar_hover != bar_hover || m.expanded != expanded;
        (m.hover, m.bar_hover, m.expanded) = (hover, bar_hover, expanded);
        if let Some((_, at, dragging)) = m.press.as_mut()
            && (dragging.is_some() || (to - *at).x.hypot((to - *at).y) > crate::wm::DRAG_THRESHOLD)
        {
            *dragging = Some(to);
            changed = true;
        }
        if let Some((from, at, dragging)) = m.space_press.as_mut()
            && (*dragging || (to - *at).x.hypot((to - *at).y) > crate::wm::DRAG_THRESHOLD)
        {
            *dragging = true;
            let (from, onto) = (*from, slot(&layout.bar.cells, to.x));
            if onto != from {
                m.space_press = m.space_press.map(|(_, at, d)| (onto, at, d));
                self.move_space(from, onto);
                changed = true;
            }
        }
        if changed {
            self.schedule_redraw();
        }
    }

    /// The left button went up at `location`: a click focuses the window and closes; a drag onto a Space moves the
    /// window there.
    pub fn mission_release(&mut self, at: Point<f64, Logical>) {
        if let Some((i, _, dragged)) = self.mission.as_mut().and_then(|m| m.space_press.take()) {
            if !dragged {
                self.show_space(i);
                self.close_mission();
            }
            return;
        }
        let Some((window, _, dragged)) = self.mission.as_mut().and_then(|m| m.press.take()) else { return };
        let Some((output, location)) = self.mission_local(at) else { return };
        match dragged {
            None => {
                self.focus_window(Some(window), SERIAL_COUNTER.next_serial());
                self.close_mission();
            }
            Some(_) => {
                // ponytail: a drag stays on its display (the press is dropped when the pointer leaves it).
                let target = self.mission_layout(&output).and_then(|l| l.bar.cells.iter().position(|c| c.to_f64().contains(location)));
                if let Some(desk) = target {
                    self.send_to_space(&window, desk);
                }
                self.schedule_redraw();
            }
        }
    }

    /// Esc closes Mission Control; arrows and Tab move the highlight (the hover ring), Return or Space opens it,
    /// Delete or Super+W closes it; Ctrl+← / → show the next Space. `true`: the key was taken.
    pub fn mission_key(&mut self, key: Keysym, mods: &ModifiersState) -> bool {
        if !self.mission_open() {
            return false;
        }
        let dir = match key {
            Keysym::Left => Some((-1, 0)),
            Keysym::Right => Some((1, 0)),
            Keysym::Up => Some((0, -1)),
            Keysym::Down => Some((0, 1)),
            _ => None,
        };
        match key {
            Keysym::Escape => self.close_mission(),
            Keysym::Left | Keysym::Right if mods.ctrl => {
                if let Some(m) = self.mission.as_mut() {
                    m.hover = None;
                }
                self.switch_space(if key == Keysym::Left { -1 } else { 1 });
            }
            Keysym::Up if mods.ctrl => self.mission_all(),
            _ if mods.ctrl || mods.alt || (mods.logo && key != Keysym::w) => return false,
            Keysym::w | Keysym::Delete => {
                let Some(window) = self.mission.as_ref().and_then(|m| m.hover.clone()) else { return key == Keysym::Delete };
                self.close_window(&window);
            }
            Keysym::Tab | Keysym::ISO_Left_Tab => self.mission_step(|targets, at| {
                let order = reading_order(targets);
                let n = order.len();
                let pos = at.and_then(|a| order.iter().position(|i| *i == a));
                let next = match (pos, key == Keysym::Tab) {
                    (None, true) => 0,
                    (None, false) => n - 1,
                    (Some(p), true) => (p + 1) % n,
                    (Some(p), false) => (p + n - 1) % n,
                };
                Some(order[next])
            }),
            _ if dir.is_some() => {
                let dir = dir.unwrap_or_default();
                self.mission_step(|targets, at| match at {
                    Some(at) => nearest(targets, at, dir).or(Some(at)),
                    None => reading_order(targets).first().copied(),
                });
            }
            Keysym::Return | Keysym::KP_Enter | Keysym::space => {
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

    /// Every Space as tiles, or back to the shown one's windows.
    // ponytail: switching views snaps (no tween), and closing from it slides other Spaces' windows off below; tween
    // between the two layouts if it looks rough.
    fn mission_all(&mut self) {
        if let Some(m) = self.mission.as_mut() {
            m.all = !m.all;
            m.hover = None;
            self.schedule_redraw();
        }
    }

    /// Moves the highlight on the pointer's display: `pick` gets the grid's rects and the highlighted one's index.
    fn mission_step(&mut self, pick: impl Fn(&[Rect], Option<usize>) -> Option<usize>) {
        let Some(output) = self.pointer_output() else { return };
        let Some(layout) = self.mission_layout(&output).filter(|l| !l.windows.is_empty()) else { return };
        let Some(m) = self.mission.as_mut() else { return };
        let at = m.hover.as_ref().and_then(|h| layout.windows.iter().position(|w| w == h));
        m.hover = pick(&layout.targets, at).and_then(|i| layout.windows.get(i).cloned());
        self.schedule_redraw();
    }
}

enum BarHit {
    Space(usize),
    Close(usize),
    Plus,
    All,
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
        assert!(expanded.cells.iter().all(|c| c.loc.y + c.size.h + 4 + NAME_H < area_top()));
        assert!(expanded.plus.is_some_and(|p| expanded.cells.iter().all(|c| !c.overlaps(p))));
        assert_eq!(bar(output, 3, false, false).plus, None, "a fixed count has no +");
    }

    #[test]
    fn bar_text_grows_with_text_size() {
        let output = Size::from((1280, 800));
        let at = |k, expanded| crate::text::with_scale(k, || (bar(output, 3, expanded, true), area_top()));
        assert_eq!(at(1.0, false), (bar(output, 3, false, true), area_top()));
        let ((one, _), (big, _)) = (at(1.0, false), at(1.5, false));
        // Callout (15 px lines) at 1.5 fits the pill; the pills and their + grow, still apart and on screen.
        assert_eq!(big.cells[0].size, Size::from((180, 36)));
        assert!(big.cells[0].size.w > one.cells[0].size.w && big.cells.windows(2).all(|c| !c[0].overlaps(c[1])));
        assert!(big.plus.is_some_and(|p| p.size.h == 36 && p.loc.x + p.size.w <= 1280 && big.cells.iter().all(|c| !c.overlaps(p))));
        // Expanded: the thumbnails keep their size, the names under them grow and windows start below them.
        let (big, top) = at(1.5, true);
        assert_eq!(big.cells[0].size, Size::from((153, THUMB_H)));
        assert!(big.cells.iter().all(|c| c.loc.y + c.size.h + 4 + 24 < top));
    }

    #[test]
    fn crowded_bars_shrink_their_cells_and_keep_the_plus() {
        for (output, count, expanded) in [((1024, 768), 9, true), ((1024, 768), 12, false), ((768, 1024), 8, true)] {
            let output = Size::from(output);
            let bar = bar(output, count, expanded, true);
            let plus = bar.plus.unwrap();
            let last = bar.cells[count - 1];
            assert!(bar.cells[0].loc.x >= BAR_GAP && last.loc.x + last.size.w + BAR_GAP <= plus.loc.x, "{output:?} {count} {expanded}");
            assert!(plus.loc.x + plus.size.w <= output.w);
            assert!(bar.cells.windows(2).all(|c| !c[0].overlaps(c[1])));
            if expanded {
                let cell = bar.cells[0].size;
                assert!(cell.h < THUMB_H && (cell.w * output.h - cell.h * output.w).abs() <= output.w, "thumbnails keep the screen's shape");
            }
        }
        assert_eq!(bar(Size::from((1280, 800)), 3, true, true).cells[0].size, Size::from((153, THUMB_H)), "roomy bars are unchanged");
    }

    #[test]
    fn a_dragged_space_takes_the_nearest_place() {
        let cells = bar(Size::from((1280, 800)), 3, true, true).cells;
        let centre = |i: usize| f64::from(cells[i].loc.x + cells[i].size.w / 2);
        assert_eq!(slot(&cells, 0.0), 0);
        assert_eq!(slot(&cells, centre(1) + 10.0), 1);
        assert_eq!(slot(&cells, (centre(1) + centre(2)) / 2.0 + 1.0), 2, "past half way: the next place");
        assert_eq!(slot(&cells, 5000.0), 2);
        assert_eq!(slot(&[], 10.0), 0);
    }

    #[test]
    fn nearest_goes_the_way_asked() {
        // 0 1 2
        //  3 4
        let r = [rect(0, 0, 100, 100), rect(200, 0, 100, 100), rect(400, 0, 100, 100), rect(100, 200, 100, 100), rect(300, 200, 100, 100)];
        assert_eq!(nearest(&r, 0, (1, 0)), Some(1));
        assert_eq!(nearest(&r, 1, (1, 0)), Some(2));
        assert_eq!(nearest(&r, 2, (1, 0)), None, "nothing further right");
        assert_eq!(nearest(&r, 0, (-1, 0)), None);
        assert_eq!(nearest(&r, 0, (0, 1)), Some(3));
        assert_eq!(nearest(&r, 2, (0, 1)), Some(4));
        assert_eq!(nearest(&r, 4, (0, -1)), Some(1), "a tie goes to the first");
        assert_eq!(nearest(&r, 3, (1, 0)), Some(4), "same row beats a diagonal");
        assert_eq!(nearest(&r, 9, (1, 0)), None);
        assert_eq!(reading_order(&r), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn tiles_fill_rows_in_order_with_the_screen_shape() {
        let output = Size::from((1600, 1000));
        let area = rect(32, 96, 1536, 760);
        for count in 1..=9 {
            let t = tiles(output, count, area);
            assert_eq!(t.len(), count);
            for (i, a) in t.iter().enumerate() {
                assert!(area.contains_rect(*a), "{count}: {a:?}");
                assert!((a.size.w * output.h - a.size.h * output.w).abs() <= output.w, "{count}: shape kept");
                assert_eq!(a.size, t[0].size, "{count}: equal tiles");
                for b in &t[i + 1..] {
                    assert!(!a.overlaps(*b));
                    assert!((b.loc.y, b.loc.x) > (a.loc.y, a.loc.x), "{count}: reading order");
                }
            }
        }
        assert_eq!(tiles(output, 4, area)[2].loc.y, tiles(output, 4, area)[3].loc.y, "four make two rows of two");
        assert!(tiles(output, 4, area)[1].loc.y < tiles(output, 4, area)[2].loc.y);
        // A window filling the left half fills the tile's left half.
        assert_eq!(in_tile(rect(0, 0, 800, 1000), output, rect(100, 100, 400, 250)), rect(100, 100, 200, 250));
    }

    #[test]
    fn lerp_goes_end_to_end() {
        let (a, b) = (rect(0, 0, 100, 100), rect(100, 50, 50, 20));
        assert_eq!(lerp(a, b, 0.0), a);
        assert_eq!(lerp(a, b, 1.0), b);
        assert_eq!(lerp(a, b, 0.5), rect(50, 25, 75, 60));
    }
}
