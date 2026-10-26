//! The on-screen keyboard (E8.6): a glass panel of keys at the bottom of the screen, shown by the
//! `on-screen-keyboard` action or always with `[accessibility] on_screen_keyboard`. A click types the key through the
//! seat's keyboard, as if pressed on a real one, so shortcuts, the lock screen's password field and every app get it.
//! Modifiers latch for the next key. US key positions; the letters come from the active xkb layout.

use smithay::{
    backend::input::KeyState,
    input::keyboard::Keycode,
    utils::{Logical, Point, Rectangle, Size},
};

use crate::{state::State, wm::Rect};

/// A key: evdev code, width in quarter units, label (`""`: the active layout's character).
pub type Key = (u32, u8, &'static str);

/// Five rows of 15 units (60 quarters) each.
pub const ROWS: [&[Key]; 5] = [
    &[
        (41, 4, ""),
        (2, 4, ""),
        (3, 4, ""),
        (4, 4, ""),
        (5, 4, ""),
        (6, 4, ""),
        (7, 4, ""),
        (8, 4, ""),
        (9, 4, ""),
        (10, 4, ""),
        (11, 4, ""),
        (12, 4, ""),
        (13, 4, ""),
        (14, 8, "delete"),
    ],
    &[
        (15, 6, "tab"),
        (16, 4, ""),
        (17, 4, ""),
        (18, 4, ""),
        (19, 4, ""),
        (20, 4, ""),
        (21, 4, ""),
        (22, 4, ""),
        (23, 4, ""),
        (24, 4, ""),
        (25, 4, ""),
        (26, 4, ""),
        (27, 4, ""),
        (43, 6, ""),
    ],
    &[
        (58, 7, "caps lock"),
        (30, 4, ""),
        (31, 4, ""),
        (32, 4, ""),
        (33, 4, ""),
        (34, 4, ""),
        (35, 4, ""),
        (36, 4, ""),
        (37, 4, ""),
        (38, 4, ""),
        (39, 4, ""),
        (40, 4, ""),
        (28, 9, "return"),
    ],
    &[
        (42, 9, "shift"),
        (44, 4, ""),
        (45, 4, ""),
        (46, 4, ""),
        (47, 4, ""),
        (48, 4, ""),
        (49, 4, ""),
        (50, 4, ""),
        (51, 4, ""),
        (52, 4, ""),
        (53, 4, ""),
        (54, 11, "shift"),
    ],
    &[
        (1, 4, "esc"),
        (29, 5, "ctrl"),
        (56, 5, "alt"),
        (125, 5, "super"),
        (57, 20, ""),
        (100, 5, "alt"),
        (105, 4, "←"),
        (103, 4, "↑"),
        (108, 4, "↓"),
        (106, 4, "→"),
    ],
];

const SHIFT: [u32; 2] = [42, 54];

/// Ctrl, Shift, Alt, Super: latched by a click, let go after the next key.
pub fn is_modifier(code: u32) -> bool {
    matches!(code, 29 | 42 | 54 | 56 | 100 | 125)
}

/// Every key in row order.
pub fn keys() -> impl Iterator<Item = Key> {
    ROWS.iter().flat_map(|r| r.iter().copied())
}

pub const PAD: i32 = 10;
pub const GAP: i32 = 5;
pub const RADIUS: f64 = 14.0;
/// Bottom margin.
const MARGIN: i32 = 8;

/// The unit (a letter key plus its gap): 44 px, smaller on a narrow screen.
fn unit(output: Size<i32, Logical>) -> i32 {
    ((output.w - 2 * PAD - 2 * MARGIN) / 15).clamp(16, 44)
}

/// The panel, centred at the bottom of an output of `output` size.
pub fn panel(output: Size<i32, Logical>) -> Rect {
    let u = unit(output);
    let (w, h) = (2 * PAD + 15 * u - GAP, 2 * PAD + 5 * u - GAP);
    Rectangle::new(((output.w - w) / 2, output.h - h - MARGIN).into(), (w, h).into())
}

/// Each key's rect, in `keys()` order.
pub fn key_rects(output: Size<i32, Logical>) -> Vec<Rect> {
    let (p, u) = (panel(output), unit(output));
    let mut out = Vec::new();
    for (row, keys) in ROWS.iter().enumerate() {
        let mut q = 0;
        for &(_, quarters, _) in keys.iter() {
            let (x, end) = (q * u / 4, (q + i32::from(quarters)) * u / 4);
            out.push(Rectangle::new((p.loc.x + PAD + x, p.loc.y + PAD + row as i32 * u).into(), (end - x - GAP, u - GAP).into()));
            q += i32::from(quarters);
        }
    }
    out
}

/// The key at `at` (output-relative), by index into `keys()`.
pub fn hit(output: Size<i32, Logical>, at: Point<f64, Logical>) -> Option<usize> {
    key_rects(output).iter().position(|r| r.to_f64().contains(at))
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Osk {
    /// Shown by the action (also shown while `[accessibility] on_screen_keyboard` is on).
    pub open: bool,
    /// Modifiers clicked, by evdev code, held down for the next key.
    pub latched: Vec<u32>,
}

impl State {
    pub fn osk_shown(&self) -> bool {
        self.osk.open || self.config.accessibility.on_screen_keyboard
    }

    pub fn toggle_osk(&mut self) {
        self.osk.open = !self.osk_shown();
        self.osk.latched.clear();
        self.schedule_redraw();
    }

    /// The panel on the shell's output, when shown.
    pub fn osk_panel(&self) -> Option<Rect> {
        self.osk_shown().then(|| self.space.outputs().next().and_then(|o| self.space.output_geometry(o))).flatten().map(|g| panel(g.size))
    }

    /// Each key's label: its own, or the active layout's character (upper case while Shift is latched).
    pub fn osk_labels(&mut self) -> Vec<String> {
        let Some(keyboard) = self.seat.get_keyboard() else { return Vec::new() };
        let shift = self.osk.latched.iter().any(|c| SHIFT.contains(c));
        keyboard.with_xkb_state(self, |ctx| {
            let xkb = ctx.xkb().lock().unwrap();
            keys()
                .map(|(code, _, label)| match label {
                    "" => {
                        // ponytail: level 1 only, upper-cased with Shift; Shift's digits row ("!", "@") isn't shown.
                        let c = xkb.raw_syms_for_key_in_layout(Keycode::new(code + 8), xkb.active_layout()).first().and_then(|s| s.key_char());
                        c.map(|c| if shift { c.to_uppercase().collect() } else { c.to_string() }).unwrap_or_default()
                    }
                    label => label.to_owned(),
                })
                .collect()
        })
    }

    /// A press at `location` (output-relative): a key types (a modifier latches or lets go). `true`: on the panel.
    pub fn osk_press(&mut self, location: Point<f64, Logical>) -> bool {
        let Some(output) = self.osk_panel().and_then(|_| self.space.outputs().next().and_then(|o| self.space.output_geometry(o))) else { return false };
        if !panel(output.size).to_f64().contains(location) {
            return false;
        }
        let Some((code, ..)) = hit(output.size, location).and_then(|i| keys().nth(i)) else { return true };
        if is_modifier(code) {
            match self.osk.latched.iter().position(|c| *c == code) {
                Some(i) => _ = self.osk.latched.remove(i),
                None => self.osk.latched.push(code),
            }
        } else {
            let mods = std::mem::take(&mut self.osk.latched);
            let time = std::time::Duration::from(self.protocols.clock.now()).as_millis() as u32;
            for &m in &mods {
                self.key_input(Keycode::new(m + 8), KeyState::Pressed, time);
            }
            self.key_input(Keycode::new(code + 8), KeyState::Pressed, time);
            self.key_input(Keycode::new(code + 8), KeyState::Released, time);
            for &m in mods.iter().rev() {
                self.key_input(Keycode::new(m + 8), KeyState::Released, time);
            }
        }
        self.schedule_redraw();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_fifteen_units() {
        for row in ROWS {
            assert_eq!(row.iter().map(|k| u32::from(k.1)).sum::<u32>(), 60, "{row:?}");
        }
        let mut codes: Vec<u32> = keys().map(|k| k.0).collect();
        codes.sort();
        codes.dedup();
        assert_eq!(codes.len(), keys().count(), "a key twice");
    }

    #[test]
    fn keys_fit_and_hit() {
        for size in [Size::from((1920, 1080)), Size::from((640, 480))] {
            let p = panel(size);
            assert!(Rectangle::from_size(size).contains_rect(p), "{size:?}");
            let rects = key_rects(size);
            assert!(rects.iter().all(|r| p.contains_rect(*r)));
            // Each row ends at the same right edge, PAD from the panel's.
            let right = p.loc.x + p.size.w - PAD;
            let mut i = 0;
            for row in ROWS {
                i += row.len();
                assert_eq!(rects[i - 1].loc.x + rects[i - 1].size.w, right);
            }
            for (n, r) in rects.iter().enumerate() {
                assert_eq!(hit(size, r.loc.to_f64() + Point::from((1.0, 1.0))), Some(n));
            }
        }
        let size = Size::from((1920, 1080));
        let q = keys().position(|k| k.0 == 16).unwrap();
        let r = key_rects(size)[q];
        assert_eq!(hit(size, (f64::from(r.loc.x - 2), f64::from(r.loc.y + 5)).into()), None, "the gap");
        assert_eq!(hit(size, (5.0, 5.0).into()), None);
    }
}
