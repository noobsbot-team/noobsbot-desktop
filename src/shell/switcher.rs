//! The app switcher (E4.5): hold the desktop key and press Tab to step through the apps, most recently used first;
//! letting go of the key switches. Shift steps back, Esc cancels, Q quits the selected app. It shows only after a
//! 150 ms hold, so a quick Super+Tab flips to the previous app without a flash.

use std::time::Duration;

use smithay::{
    desktop::Window,
    input::keyboard::{Keysym, ModifiersState},
    reexports::calloop::{
        RegistrationToken,
        timer::{TimeoutAction, Timer},
    },
    utils::{IsAlive, Rectangle, SERIAL_COUNTER, Size},
};

use crate::{state::State, wm::Rect};

/// How long the keys must be held before the panel shows.
const HOLD: Duration = Duration::from_millis(150);

/// The binding's modifiers: the switcher stays open while any of them is down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Held {
    pub desktop: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// The binding's key, which steps like Tab.
    pub key: Keysym,
}

impl Held {
    fn down(&self, mods: &ModifiersState, desktop_is_alt: bool) -> bool {
        let desktop = if desktop_is_alt { mods.alt } else { mods.logo };
        (self.desktop && desktop) || (self.ctrl && mods.ctrl) || (self.alt && mods.alt)
    }
}

/// One app in the strip: its windows, most recent first.
#[derive(Debug, Clone)]
pub struct App {
    pub name: String,
    pub windows: Vec<Window>,
}

pub struct Open {
    pub apps: Vec<App>,
    pub selected: usize,
    /// Past the 150 ms hold.
    pub visible: bool,
    held: Held,
    timer: Option<RegistrationToken>,
    /// What the last key decided; carried out once the keyboard is free (see `switcher_settle`).
    outcome: Option<Outcome>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Outcome {
    Commit,
    Cancel,
    Quit,
}

#[derive(Default)]
pub struct Switcher {
    /// Windows, most recently focused first.
    mru: Vec<Window>,
    pub open: Option<Open>,
}

impl Switcher {
    /// `window` got focus.
    pub fn touch(&mut self, window: &Window) {
        self.mru.retain(|w| w != window && w.alive());
        self.mru.insert(0, window.clone());
    }
}

/// `all` ordered by `mru` (most recent first); what was never focused follows in `all`'s order.
pub fn mru_order<T: PartialEq + Clone>(mru: &[T], all: &[T]) -> Vec<T> {
    let mut out: Vec<T> = mru.iter().filter(|t| all.contains(t)).cloned().collect();
    out.extend(all.iter().filter(|t| !mru.contains(t)).cloned());
    out
}

/// Groups `items` by key, keys in the order they first appear, items in order within each.
pub fn group<K: PartialEq, T>(items: impl IntoIterator<Item = (K, T)>) -> Vec<(K, Vec<T>)> {
    let mut out: Vec<(K, Vec<T>)> = Vec::new();
    for (key, item) in items {
        match out.iter_mut().find(|(k, _)| *k == key) {
            Some((_, list)) => list.push(item),
            None => out.push((key, vec![item])),
        }
    }
    out
}

/// One step from `i` among `len`, wrapping.
fn step(i: usize, len: usize, back: bool) -> usize {
    if len == 0 {
        0
    } else if back {
        (i + len - 1) % len
    } else {
        (i + 1) % len
    }
}

/// The switcher (E4.5): a `hud` panel, radius 18, centred.
pub mod layout {
    pub const RADIUS: f64 = 18.0;
    pub const PAD: i32 = 16;
    pub const ICON: i32 = 96;
    /// 12 px between icons; the highlight (radius 12) reaches 6 px around one.
    pub const CELL: i32 = ICON + 12;
    pub const NAME_GAP: i32 = 6;
    pub const NAME_H: i32 = 16;
    pub const MARGIN: i32 = 16;
    /// Wide enough for a long app name under a single icon.
    pub const MIN_WIDTH: i32 = 240;
}

/// The panel, one cell per app, and the name line under the strip, output-relative. With too many apps for the
/// screen the cells shrink to fit.
pub fn layout(output: Size<i32, smithay::utils::Logical>, count: usize) -> (Rect, Vec<Rect>, Rect) {
    use layout::*;
    let n = count.max(1) as i32;
    let cell = CELL.min((output.w - 2 * MARGIN - 2 * PAD) / n).max(8);
    let (w, h) = ((2 * PAD + n * cell).max(MIN_WIDTH.min(output.w)), PAD + cell + NAME_GAP + NAME_H + PAD);
    let first = (w - n * cell) / 2;
    let (x, y) = ((output.w - w) / 2, ((output.h - h) / 2).max(0));
    let cells = (0..count as i32).map(|i| Rectangle::new((x + first + i * cell, y + PAD).into(), (cell, cell).into())).collect();
    let name = Rectangle::new((x + PAD, y + PAD + cell + NAME_GAP).into(), (w - 2 * PAD, NAME_H).into());
    (Rectangle::new((x, y).into(), (w, h).into()), cells, name)
}

impl State {
    /// The theme icon of the installed app `window` belongs to.
    pub fn app_icon(&self, window: &Window) -> Option<String> {
        self.apps.find(&self.app_key(window)?).map(|a| a.icon.clone())
    }

    /// The apps with a window, most recently used first.
    pub fn switcher_apps(&mut self) -> Vec<App> {
        let all: Vec<Window> = self.space.elements().rev().cloned().chain(self.hidden.iter().map(|(w, _)| w.clone())).collect();
        let ordered = mru_order(&self.switcher.mru, &all);
        let keyed: Vec<(String, Window)> = ordered.into_iter().map(|w| (self.app_id(&w).unwrap_or_default(), w)).collect();
        group(keyed)
            .into_iter()
            .map(|(id, windows)| {
                // ponytail: the display name comes from the .desktop file, cached by the menu bar.
                let name = self.backend.as_mut().map_or_else(|| id.clone(), |b| b.menu_bar.name(&id));
                App { name, windows }
            })
            .collect()
    }

    /// Super+Tab (or Shift for back): opens on the previous app, or steps if open.
    pub fn switcher_start(&mut self, back: bool, held: Held) {
        if let Some(open) = self.switcher.open.as_mut() {
            open.selected = step(open.selected, open.apps.len(), back);
            self.schedule_redraw();
            return;
        }
        let apps = self.switcher_apps();
        if apps.is_empty() {
            return;
        }
        let selected = step(0, apps.len(), back);
        let timer = self.loop_handle.insert_source(Timer::from_duration(HOLD), |_, _, state| {
            if let Some(open) = state.switcher.open.as_mut() {
                open.visible = true;
                open.timer = None;
                state.schedule_redraw();
            }
            TimeoutAction::Drop
        });
        self.switcher.open = Some(Open { apps, selected, visible: false, held, timer: timer.ok(), outcome: None });
        // A binding without modifiers (an F key) has nothing to hold: it switches at once.
        let mods = self.modifiers();
        if !held.down(&mods, self.nested) {
            self.switcher_finish(Outcome::Commit);
        }
    }

    /// `--show switcher`: open and visible at once, on the previous app, with no key held.
    pub fn switcher_preview(&mut self) {
        let apps = self.switcher_apps();
        let selected = step(0, apps.len(), false);
        self.switcher.open = Some(Open { apps, selected, visible: true, held: Held { desktop: true, ..Default::default() }, timer: None, outcome: None });
        self.schedule_redraw();
    }

    /// A key press while open: `true` if the switcher took it (all keys go to it while it's up).
    pub fn switcher_key(&mut self, sym: Keysym, mods: &ModifiersState) -> bool {
        let Some(open) = self.switcher.open.as_mut() else { return false };
        let len = open.apps.len();
        match sym {
            Keysym::Tab | Keysym::ISO_Left_Tab | Keysym::Right | Keysym::Left => {
                open.selected = step(open.selected, len, mods.shift || sym == Keysym::ISO_Left_Tab || sym == Keysym::Left);
            }
            k if k == open.held.key => open.selected = step(open.selected, len, mods.shift),
            Keysym::Escape => open.outcome = Some(Outcome::Cancel),
            Keysym::Return | Keysym::KP_Enter => open.outcome = Some(Outcome::Commit),
            Keysym::q | Keysym::Q => open.outcome = Some(Outcome::Quit),
            _ => {}
        }
        self.schedule_redraw();
        true
    }

    /// A key release while open: letting go of the binding's modifiers switches.
    pub fn switcher_release(&mut self, mods: &ModifiersState) {
        let nested = self.nested;
        if let Some(open) = self.switcher.open.as_mut()
            && open.outcome.is_none()
            && !open.held.down(mods, nested)
        {
            open.outcome = Some(Outcome::Commit);
        }
    }

    /// Carries out what the last key decided. Runs after the keyboard's input call, which must not change focus.
    pub fn switcher_settle(&mut self) {
        if let Some(outcome) = self.switcher.open.as_mut().and_then(|o| o.outcome.take()) {
            self.switcher_finish(outcome);
        }
    }

    fn switcher_finish(&mut self, outcome: Outcome) {
        let Some(open) = self.switcher.open.as_mut() else { return };
        let app = open.apps.get(open.selected).cloned();
        if outcome == Outcome::Quit {
            // Stays open on the next app, as on macOS.
            if let Some(app) = &app {
                app.windows.iter().filter_map(|w| w.toplevel()).for_each(|t| t.send_close());
                open.apps.remove(open.selected);
                open.selected = open.selected.min(open.apps.len().saturating_sub(1));
            }
            if !open.apps.is_empty() {
                self.schedule_redraw();
                return;
            }
        }
        if let Some(open) = self.switcher.open.take()
            && let Some(token) = open.timer
        {
            self.loop_handle.remove(token);
        }
        self.schedule_redraw();
        if outcome == Outcome::Commit
            && let Some(app) = app
        {
            self.switch_to(&app.windows);
        }
    }

    /// Brings every window of an app forward (hidden ones come back), the most recent on top with focus.
    fn switch_to(&mut self, windows: &[Window]) {
        let windows: Vec<&Window> = windows.iter().filter(|w| w.alive()).collect();
        for window in windows.iter().rev() {
            if let Some(i) = self.hidden.iter().position(|(w, _)| w == *window) {
                let (window, loc) = self.hidden.remove(i);
                self.set_visible_loc(&window, loc);
            }
            self.space.raise_element(window, false);
        }
        if let Some(top) = windows.first() {
            self.focus_window(Some((*top).clone()), SERIAL_COUNTER.next_serial());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn most_recent_first_then_the_rest() {
        assert_eq!(mru_order(&["b", "a", "gone"], &["a", "b", "c", "d"]), ["b", "a", "c", "d"]);
        assert_eq!(mru_order::<&str>(&[], &["a", "b"]), ["a", "b"]);
    }

    #[test]
    fn groups_by_app_in_first_seen_order() {
        let grouped = group([("kate", 1), ("helium", 2), ("kate", 3), ("term", 4), ("helium", 5)]);
        assert_eq!(grouped, [("kate", vec![1, 3]), ("helium", vec![2, 5]), ("term", vec![4])]);
    }

    #[test]
    fn steps_wrap() {
        assert_eq!(step(0, 3, false), 1);
        assert_eq!(step(2, 3, false), 0);
        assert_eq!(step(0, 3, true), 2);
        assert_eq!(step(0, 1, false), 0);
        assert_eq!(step(0, 0, true), 0);
    }

    #[test]
    fn held_modifiers() {
        let held = Held { desktop: true, ..Default::default() };
        let alt = ModifiersState { alt: true, ..Default::default() };
        let logo = ModifiersState { logo: true, ..Default::default() };
        assert!(held.down(&alt, true), "nested: Alt is the desktop key");
        assert!(!held.down(&alt, false));
        assert!(held.down(&logo, false));
        assert!(!Held::default().down(&logo, false), "an F-key binding holds nothing");
    }

    #[test]
    fn layout_centres_and_shrinks() {
        let (panel, cells, name) = layout((1280, 800).into(), 3);
        assert_eq!(panel.size.w, 2 * layout::PAD + 3 * layout::CELL);
        assert_eq!(panel.loc.x, (1280 - panel.size.w) / 2);
        assert!(cells.iter().chain([&name]).all(|c| panel.contains_rect(*c)));
        assert_eq!(cells[1].loc.x - cells[0].loc.x, layout::CELL);
        let (panel, cells, _) = layout((1280, 800).into(), 1);
        assert_eq!(panel.size.w, layout::MIN_WIDTH, "room for the name");
        assert_eq!(cells[0].loc.x - panel.loc.x, (layout::MIN_WIDTH - layout::CELL) / 2, "the icon centred");
        let (panel, cells, _) = layout((800, 600).into(), 20);
        assert!(panel.size.w <= 800 - 2 * layout::MARGIN && panel.loc.x >= 0);
        assert!(cells.iter().all(|c| panel.contains_rect(*c)));
    }
}
