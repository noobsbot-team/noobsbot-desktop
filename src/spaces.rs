//! Spaces (E5.1, E8.7): several desktops per display (or one set for all, `[spaces] per_display`), each with its own windows, stacking and focus.
//!
//! Every window stays mapped in the one Smithay `Space`; a hidden Space's windows sit `STRIDE` px rows below their
//! output, so nothing on screen (drawing, hit tests, frame callbacks) sees them and their apps idle, while the dock,
//! the switcher and Force Quit still find them. Row 0 is the shown Space; Space `d` lives in row `d + 1` while hidden.

use std::{collections::HashMap, time::Instant};

use smithay::{
    desktop::Window,
    output::Output,
    reexports::calloop::{
        RegistrationToken,
        timer::{TimeoutAction, Timer},
    },
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::{rules, state::State, wm::Rect};

/// Rows between Spaces: far more than any window can hang off a screen edge.
pub const STRIDE: i32 = 1 << 16;
/// The Space indicator shows this long after a switch.
const HUD_TIME: std::time::Duration = std::time::Duration::from_millis(800);
/// D10 `smooth`.
const SMOOTH: f64 = 0.4;

/// One output's Spaces: `Some(window)` is a fullscreen app's own Space.
pub struct Desks {
    pub list: Vec<Option<Window>>,
    pub current: usize,
}

impl Desks {
    fn new(fixed: u8) -> Self {
        Self { list: vec![None; usize::from(fixed.max(1))], current: 0 }
    }
}

/// A value easing from `from` to `to`.
/// ponytail: an ease-out cubic over `smooth`'s 0.4 s, not a spring; swap for the anim.rs springs when they land.
#[derive(Debug, Clone, Copy)]
pub struct Tween {
    from: f64,
    to: f64,
    start: Instant,
}

impl Tween {
    pub fn new(from: f64, to: f64) -> Self {
        Self { from, to, start: Instant::now() }
    }

    pub fn value(&self, now: Instant) -> f64 {
        let t = (now.saturating_duration_since(self.start).as_secs_f64() / SMOOTH).min(1.0);
        self.from + (self.to - self.from) * (1.0 - (1.0 - t).powi(3))
    }

    pub fn done(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start).as_secs_f64() >= SMOOTH
    }

    pub fn target(&self) -> f64 {
        self.to
    }
}

/// The screen sliding from one Space to the next: `progress` 0 → 1.
#[derive(Debug, Clone)]
pub struct Slide {
    /// Whose Spaces slide (see `desk_key`).
    pub key: String,
    pub from: usize,
    pub to: usize,
    pub progress: Tween,
}

#[derive(Default)]
pub struct Spaces {
    /// By `desk_key`: output name, or `""` for the one set all displays share.
    pub outputs: HashMap<String, Desks>,
    pub slide: Option<Slide>,
    /// The indicator pill is up; its hide timer.
    pub hud: Option<RegistrationToken>,
    /// Whose Spaces the indicator shows.
    pub hud_key: String,
}

/// The row Space `desk` sits in while `current` is shown.
pub fn row_of(desk: usize, current: usize) -> i32 {
    if desk == current { 0 } else { desk as i32 + 1 }
}

/// The Space in `row` while `current` is shown.
pub fn desk_of(row: i32, current: usize) -> usize {
    if row <= 0 { current } else { row as usize - 1 }
}

/// The row of a window whose top is at `y`, below an output at `top`.
pub fn row_at(y: i32, top: i32) -> i32 {
    (y - top + STRIDE / 2).div_euclid(STRIDE)
}

/// Where a Space's windows go when Space `removed` goes: to its left neighbour (the first's to the new first).
pub fn after_remove(desk: usize, removed: usize) -> usize {
    if desk > removed || (desk == removed && desk > 0) { desk - 1 } else { desk }
}

/// `current + delta` if that Space exists.
pub fn step(current: usize, delta: i32, count: usize) -> Option<usize> {
    let to = current as i64 + i64::from(delta);
    (0..count as i64).contains(&to).then_some(to as usize).filter(|t| *t != current)
}

/// Whose Spaces a display named `name` shows: its own, or (`""`) the one set every display shares.
pub fn desk_key(per_display: bool, name: &str) -> String {
    if per_display { name.to_owned() } else { String::new() }
}

/// The row a window on `row` goes to when its display's Spaces are renumbered by `map` and `current` is shown
/// after (`old` before). `stay`: carried along or sticky, so it ends on the shown Space.
pub fn new_row(row: i32, old: usize, current: usize, map: impl Fn(usize) -> usize, stay: bool) -> i32 {
    if stay { 0 } else { row_of(map(desk_of(row, old)), current) }
}

impl State {
    /// The display under the pointer, else the first: the one Space keys, swipes and Mission Control clicks act on.
    pub fn pointer_output(&self) -> Option<Output> {
        let at = self.seat.get_pointer().map(|p| p.current_location()).unwrap_or_default();
        self.space.output_under(at).next().or_else(|| self.space.outputs().next()).cloned()
    }

    /// The key of `output`'s Spaces in `spaces.outputs`.
    pub fn output_key(&self, output: &Output) -> String {
        desk_key(self.config.spaces.per_display, &output.name())
    }

    /// The key of the Spaces `window` belongs to: those of the display it is mostly on, else of the first.
    pub fn window_key(&self, window: &Window) -> String {
        let output = self.window_output(window).or_else(|| self.space.outputs().next().cloned());
        output.map_or_else(String::new, |o| self.output_key(&o))
    }

    /// The Spaces under `key`, made on first use.
    pub fn desks_at(&mut self, key: &str) -> &mut Desks {
        let fixed = self.config.windows.spaces;
        self.spaces.outputs.entry(key.to_owned()).or_insert_with(|| Desks::new(fixed))
    }

    /// (Space names, shown index) of the Spaces under `key`.
    pub fn desk_names_at(&mut self, key: &str) -> (Vec<String>, usize) {
        let (list, current) = {
            let d = self.desks_at(key);
            (d.list.clone(), d.current)
        };
        let mut n = 0;
        let names = list
            .iter()
            .map(|full| match full {
                Some(window) => self.app_id(window).map(|id| self.app_display_name(&id)).unwrap_or_else(|| "Fullscreen".into()),
                None => {
                    n += 1;
                    format!("Desktop {n}")
                }
            })
            .collect();
        (names, current)
    }

    /// An app's display name, as the menu bar shows it.
    pub fn app_display_name(&mut self, id: &str) -> String {
        self.backend.as_mut().map_or_else(|| id.to_owned(), |b| b.menu_bar.name(id))
    }

    /// Rows are counted from the first display's top, for every display (they stay far apart: `STRIDE`).
    fn output_top(&self) -> Option<i32> {
        self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map(|g| g.loc.y)
    }

    /// The row a window is in (0: on screen).
    pub fn window_row(&self, window: &Window) -> i32 {
        let (Some(loc), Some(top)) = (self.space.element_location(window), self.output_top()) else { return 0 };
        row_at(loc.y, top)
    }

    /// On the shown Space (or on all of them).
    pub fn on_current(&self, window: &Window) -> bool {
        self.window_row(window) == 0
    }

    /// One of the windows the Spaces under `key` hold: switching them leaves other displays' windows put.
    fn holds(&self, key: &str, window: &Window) -> bool {
        key.is_empty() || self.window_key(window) == key
    }

    /// The frontmost window of the shown Space.
    pub fn top_window(&self) -> Option<Window> {
        self.space.elements().rev().find(|w| self.on_current(w)).cloned()
    }

    /// The frontmost window of the shown Space under `key`.
    fn top_window_at(&self, key: &str) -> Option<Window> {
        self.space.elements().rev().find(|w| self.on_current(w) && self.holds(key, w)).cloned()
    }

    /// On every Space: `space = "all"` (the quake terminal).
    fn sticky(&self, window: &Window) -> bool {
        self.is_quake(window) || crate::state::ruled(window).is_some_and(|a| matches!(a.space, Some(rules::Space::All(_))))
    }

    /// Moves the windows of the Spaces under `key` to the rows their Space gets: `map` renumbers Spaces, `current` is
    /// the one shown after. `carry` (and sticky windows) end on the shown Space. Stacking is kept: windows are
    /// re-mapped bottom to top.
    fn rearrange(&mut self, key: &str, map: impl Fn(usize) -> usize, current: usize, carry: Option<&Window>) {
        let old = self.desks_at(key).current;
        let Some(top) = self.output_top() else { return };
        let moves: Vec<(Window, Point<i32, Logical>)> = self
            .space
            .elements()
            .filter(|w| self.holds(key, w))
            .filter_map(|w| {
                let loc = self.space.element_location(w)?;
                let row = row_at(loc.y, top);
                let new = new_row(row, old, current, &map, self.sticky(w) || Some(w) == carry);
                Some((w.clone(), loc + Point::from((0, (new - row) * STRIDE))))
            })
            .collect();
        for (window, loc) in moves {
            self.space.map_element(window, loc, false);
        }
        self.desks_at(key).current = current;
        self.restack();
        self.schedule_redraw();
    }

    /// Shows Space `to` under `key`, sliding; `carry` comes along. Returns false if there is no such Space or it is shown.
    fn go_to(&mut self, key: &str, to: usize, carry: Option<&Window>) -> bool {
        let (from, count) = {
            let d = self.desks_at(key);
            (d.current, d.list.len())
        };
        if to == from || to >= count {
            return false;
        }
        self.rearrange(key, |d| d, to, carry);
        self.slide_and_show(key, from, to);
        true
    }

    /// The screen slides from `from` to `to` (not under Mission Control, which shows the new Space at once) and the
    /// indicator comes up, on the displays of `key`.
    fn slide_and_show(&mut self, key: &str, from: usize, to: usize) {
        if self.mission.is_none() {
            self.spaces.slide = Some(Slide { key: key.to_owned(), from, to, progress: Tween::new(0.0, 1.0) });
        }
        if let Some(token) = self.spaces.hud.take() {
            self.loop_handle.remove(token);
        }
        let hide = self.loop_handle.insert_source(Timer::from_duration(HUD_TIME), |_, _, state| {
            state.spaces.hud = None;
            state.schedule_redraw();
            TimeoutAction::Drop
        });
        self.spaces.hud = hide.inspect_err(|err| tracing::warn!("Space indicator timer: {err}")).ok();
        self.spaces.hud_key = key.to_owned();
        self.schedule_redraw();
    }

    /// The indicator on `output`, while it is up there: (Space count, shown Space).
    pub fn space_indicator(&mut self, output: &Output) -> Option<(usize, usize)> {
        let key = self.output_key(output);
        if self.spaces.hud.is_none() || key != self.spaces.hud_key {
            return None;
        }
        let d = self.desks_at(&key);
        Some((d.list.len(), d.current))
    }

    /// Super+← / → and the 3-finger swipe: the neighbouring Space of the display under the pointer.
    pub fn switch_space(&mut self, delta: i32) {
        let Some(key) = self.pointer_output().map(|o| self.output_key(&o)) else { return };
        let d = self.desks_at(&key);
        if let Some(to) = step(d.current, delta, d.list.len()) {
            self.show_space_at(&key, to);
        }
    }

    /// Super+Ctrl+1…9, or a Space clicked in Mission Control: on the display under the pointer.
    pub fn show_space(&mut self, to: usize) {
        if let Some(key) = self.pointer_output().map(|o| self.output_key(&o)) {
            self.show_space_at(&key, to);
        }
    }

    /// Shows Space `to` under `key`, focusing its frontmost window.
    fn show_space_at(&mut self, key: &str, to: usize) {
        if self.go_to(key, to, None) {
            let top = self.top_window_at(key);
            self.focus_window(top, SERIAL_COUNTER.next_serial());
        }
    }

    /// Super+Shift+← / →: the focused window comes along to its display's neighbouring Space.
    pub fn move_to_space(&mut self, delta: i32) {
        let Some(window) = self.focused_window() else { return self.switch_space(delta) };
        let key = self.window_key(&window);
        let d = self.desks_at(&key);
        let Some(to) = step(d.current, delta, d.list.len()) else { return };
        self.go_to(&key, to, Some(&window));
        self.focus_window(Some(window), SERIAL_COUNTER.next_serial());
    }

    /// Puts `window` on Space `desk` of its display without showing it (a drag onto a Space in Mission Control, a
    /// window rule).
    pub fn send_to_space(&mut self, window: &Window, desk: usize) {
        let key = self.window_key(window);
        let current = self.desks_at(&key).current;
        let (Some(frame), row) = (self.frame(window), self.window_row(window)) else { return };
        let dy = (row_of(desk, current) - row) * STRIDE;
        if dy != 0 {
            self.set_visible_loc(window, frame.loc + Point::from((0, dy)));
            if self.focused_window().as_ref() == Some(window) {
                let top = self.top_window_at(&key);
                self.focus_window(top, SERIAL_COUNTER.next_serial());
            }
        }
    }

    /// Move to ▸ Space n (from 0): the window goes, the shown Space stays. Nothing if there is no such Space.
    pub fn move_window_to_space(&mut self, window: &Window, desk: usize) {
        let key = self.window_key(window);
        if desk < self.desks_at(&key).list.len() {
            self.send_to_space(window, desk);
        }
    }

    /// Move to ▸ New Space: a new Space at the end of its display's gets the window (dynamic Spaces only).
    pub fn move_to_new_space(&mut self, window: &Window) {
        let key = self.window_key(window);
        let before = self.desks_at(&key).list.len();
        self.add_space_at(&key);
        if self.desks_at(&key).list.len() > before {
            self.send_to_space(window, before);
        }
    }

    /// The Space a window is on, among its display's.
    pub fn desk_of_window(&mut self, window: &Window) -> usize {
        let (row, key) = (self.window_row(window), self.window_key(window));
        desk_of(row, self.desks_at(&key).current)
    }

    /// Focusing a window on another Space (dock, switcher, notification) shows that Space first.
    pub fn reveal(&mut self, window: &Window) {
        if !self.on_current(window) {
            let (desk, key) = (self.desk_of_window(window), self.window_key(window));
            self.go_to(&key, desk, None);
        }
    }

    /// `+` in Mission Control: a new empty Space at the end of the pointer's display (dynamic Spaces only).
    pub fn add_space(&mut self) {
        if let Some(key) = self.pointer_output().map(|o| self.output_key(&o)) {
            self.add_space_at(&key);
        }
    }

    fn add_space_at(&mut self, key: &str) {
        if self.config.windows.spaces == 0 {
            self.desks_at(key).list.push(None);
            self.schedule_redraw();
        }
    }

    /// ✕ on a Space in Mission Control: its windows move to the neighbouring Space of the pointer's display.
    pub fn remove_space(&mut self, removed: usize) {
        if let Some(key) = self.pointer_output().map(|o| self.output_key(&o)) {
            self.remove_space_at(&key, removed);
        }
    }

    fn remove_space_at(&mut self, key: &str, removed: usize) {
        let (current, count) = {
            let d = self.desks_at(key);
            (d.current, d.list.len())
        };
        if count < 2 || removed >= count {
            return;
        }
        let shown = after_remove(current, removed);
        self.rearrange(key, |d| after_remove(d, removed), shown, None);
        self.desks_at(key).list.remove(removed);
        if current == removed {
            // Its windows are already here: nothing to slide from.
            self.slide_and_show(key, current, shown);
            self.spaces.slide = None;
            let top = self.top_window_at(key);
            self.focus_window(top, SERIAL_COUNTER.next_serial());
        }
    }

    /// Fullscreen = its own Space, right after its display's shown one (the green light, E5.1).
    pub fn fullscreen_space(&mut self, window: &Window) {
        let key = self.window_key(window);
        if self.spaces.outputs.values().any(|d| d.list.contains(&Some(window.clone()))) {
            return;
        }
        let current = self.desks_at(&key).current;
        let at = current + 1;
        self.rearrange(&key, |d| if d >= at { d + 1 } else { d }, at, Some(window));
        self.desks_at(&key).list.insert(at, Some(window.clone()));
        self.slide_and_show(&key, current, at);
    }

    /// Leaving fullscreen (or closing the app) removes its Space; the window goes to the Space on its left.
    pub fn leave_fullscreen_space(&mut self, window: &Window) {
        // Looked up, not taken from where the window is: a closed one is on no display.
        let found = self.spaces.outputs.iter().find_map(|(k, d)| Some((k.clone(), d.list.iter().position(|w| w.as_ref() == Some(window))?)));
        if let Some((key, i)) = found {
            self.remove_space_at(&key, i);
        }
    }

    /// `space = n` in a window rule: the window opens on Space n of its display, made if Spaces are dynamic.
    pub fn apply_space_rule(&mut self, window: &Window) {
        let Some(rules::Space::Number(n)) = crate::state::ruled(window).and_then(|a| a.space) else { return };
        let desk = (n.max(1) - 1) as usize;
        let dynamic = self.config.windows.spaces == 0;
        let key = self.window_key(window);
        let desks = self.desks_at(&key);
        while dynamic && desks.list.len() <= desk.min(15) {
            desks.list.push(None);
        }
        let desk = desk.min(desks.list.len() - 1);
        self.send_to_space(window, desk);
    }

    /// `[spaces] per_display` changed: every window comes to its display's shown Space and the Spaces start over.
    pub fn regroup_spaces(&mut self) {
        let hidden: Vec<Window> = self.space.elements().filter(|w| !self.on_current(w)).cloned().collect();
        for window in hidden {
            let dy = self.window_row(&window) * STRIDE;
            if let Some(frame) = self.frame(&window) {
                self.set_visible_loc(&window, frame.loc - Point::from((0, dy)));
            }
        }
        self.spaces.outputs.clear();
        self.spaces.slide = None;
        self.restack();
        self.schedule_redraw();
    }

    /// The slide's progress this frame (0 → 1), clearing it once done.
    pub fn slide_progress(&mut self) -> Option<(Slide, f64)> {
        let now = Instant::now();
        let slide = self.spaces.slide.clone()?;
        if slide.progress.done(now) {
            self.spaces.slide = None;
            return None;
        }
        let p = slide.progress.value(now);
        Some((slide, p))
    }

    /// This frame of a slide on `output`: the windows of both Spaces, topmost first, each with the output origin it
    /// is drawn against. The new Space comes in from the side it lies on; windows on every Space stay put.
    pub fn slide_frame(&mut self, output: &Output) -> Option<Vec<(Window, Point<i32, Logical>)>> {
        let (slide, p) = self.slide_progress()?;
        if slide.key != self.output_key(output) {
            return None;
        }
        let width = self.space.output_geometry(output)?.size.w;
        let dir = if slide.to > slide.from { 1 } else { -1 };
        let shift = (f64::from(width) * (1.0 - p)).round() as i32 * dir;
        let old = row_of(slide.from, slide.to);
        let windows = self
            .space
            .elements()
            .rev()
            .filter(|w| self.window_output(w).as_ref().unwrap_or(output) == output)
            .filter_map(|w| {
                let row = self.window_row(w);
                let by = match row {
                    0 if self.sticky(w) => 0,
                    0 => shift,
                    r if r == old => shift - dir * width,
                    _ => return None,
                };
                Some((w.clone(), self.row_origin(output, row) - Point::from((by, 0))))
            })
            .collect();
        Some(windows)
    }

    /// Where `output`'s windows hidden on `row` are drawn against: as if that Space were shown.
    pub fn row_origin(&self, output: &Output, row: i32) -> Point<i32, Logical> {
        let origin = self.space.output_geometry(output).map_or((0, 0).into(), |g: Rect| g.loc);
        origin + Point::from((0, row * STRIDE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_and_spaces_round_trip() {
        for current in 0..4 {
            for desk in 0..4 {
                assert_eq!(desk_of(row_of(desk, current), current), desk);
            }
            assert_eq!(row_of(current, current), 0, "the shown Space is on screen");
        }
        // Distinct rows for distinct Spaces.
        assert_ne!(row_of(0, 2), row_of(1, 2));
        assert_eq!(row_at(28, 0), 0);
        assert_eq!(row_at(-500, 0), 0, "a window hanging above the screen is still on it");
        assert_eq!(row_at(3 * STRIDE + 900, 0), 3);
        assert_eq!(row_at(2 * STRIDE - 200, 0), 2);
    }

    #[test]
    fn removing_moves_windows_to_the_neighbour() {
        // Spaces 0 1 2 3, remove 2: 2's windows go to 1, 3 becomes 2.
        assert_eq!([0, 1, 2, 3].map(|d| after_remove(d, 2)), [0, 1, 1, 2]);
        // Remove the first: its windows go to the new first.
        assert_eq!([0, 1, 2].map(|d| after_remove(d, 0)), [0, 0, 1]);
    }

    #[test]
    fn each_display_keeps_its_own_spaces() {
        assert_eq!(desk_key(true, "DP-1"), "DP-1");
        assert_eq!(desk_key(true, "HDMI-A-1"), "HDMI-A-1");
        assert_eq!(desk_key(false, "DP-1"), desk_key(false, "HDMI-A-1"), "one set for all");
        // A display showing Space 2 goes to Space 0: its rows swap, a carried window stays on screen.
        let id = |d: usize| d;
        assert_eq!(new_row(0, 2, 0, id, false), row_of(2, 0), "the old shown Space hides");
        assert_eq!(new_row(row_of(0, 2), 2, 0, id, false), 0, "the new one shows");
        assert_eq!(new_row(row_of(1, 2), 2, 0, id, false), row_of(1, 0), "others keep their Space");
        assert_eq!(new_row(0, 2, 0, id, true), 0, "carried");
        // Removing Space 1 while 2 is shown: 2 becomes 1 and stays on screen.
        assert_eq!(new_row(0, 2, 1, |d| after_remove(d, 1), false), 0);
        assert_eq!(new_row(row_of(1, 2), 2, 1, |d| after_remove(d, 1), false), row_of(0, 1));
    }

    #[test]
    fn steps_stay_in_range() {
        assert_eq!(step(0, 1, 3), Some(1));
        assert_eq!(step(0, -1, 3), None);
        assert_eq!(step(2, 1, 3), None);
        assert_eq!(step(2, -2, 3), Some(0));
        assert_eq!(step(1, 0, 3), None);
    }

    #[test]
    fn tween_eases_to_its_target() {
        let tween = Tween { from: 0.0, to: 1.0, start: Instant::now() - std::time::Duration::from_millis(200) };
        let mid = tween.value(Instant::now());
        assert!(mid > 0.5 && mid < 1.0, "ease-out: past half way at half time, got {mid}");
        let later = Instant::now() + std::time::Duration::from_secs(1);
        assert_eq!(tween.value(later), 1.0);
        assert!(tween.done(later));
    }
}
