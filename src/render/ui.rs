//! D8 components as drawing helpers (E2.1), in every D7 state. A `Ui` collects glass back to front and text pieces;
//! the caller keeps the result between frames (new elements are new damage ids) and draws it with `label_element`.

use std::sync::Arc;

use smithay::{
    backend::renderer::gles::element::PixelShaderElement,
    utils::{Point, Rectangle},
};

use super::{Material, Shaders};
use crate::{
    shell::{control::slider_fill, menu},
    symbols::{Symbol, Symbols, Weight},
    text::{Rendered, Style, Text, ts},
    ui::field::TextField,
    wm::Rect,
};

/// D7: what the pointer or keyboard is doing to a control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Normal,
    Hover,
    Pressed,
    Disabled,
    Focused,
}

impl State {
    pub const ALL: [State; 5] = [State::Normal, State::Hover, State::Pressed, State::Disabled, State::Focused];

    /// Disabled controls are drawn at 40%.
    fn alpha(self) -> f32 {
        if self == State::Disabled { 0.4 } else { 1.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Centre,
    Right,
}

/// A menu row: label, right-aligned shortcut, enabled, opens a submenu. `None` rows are separators.
pub type MenuItem<'a> = Option<(&'a str, &'a str, bool, bool)>;

pub const BODY: Style = Style { size: 13, line_height: 16, weight: 400, color: [0; 4], tabular: false };
pub const HEADLINE: Style = Style { weight: 600, ..BODY };
pub const CALLOUT: Style = Style { size: 12, line_height: 15, ..BODY };
pub const CAPTION: Style = Style { size: 10, line_height: 13, weight: 500, ..BODY };
const TITLE1: Style = Style { size: 22, line_height: 26, ..BODY };
pub const TITLE2: Style = Style { size: 17, line_height: 22, weight: 600, ..BODY };
pub const TITLE3: Style = Style { size: 15, line_height: 20, weight: 600, ..BODY };

const WHITE: [u8; 4] = [255; 4];
/// D8 badge red.
const RED: [f32; 4] = [1.0, 0x3B as f32 / 255.0, 0x30 as f32 / 255.0, 1.0];

pub struct Ui<'a> {
    pub shaders: &'a Shaders,
    pub text: &'a mut Text,
    pub symbols: &'a mut Symbols,
    pub dark: bool,
    pub accent: [u8; 3],
    pub glass_opacity: f32,
    pub scale: f64,
    /// Back to front: what is pushed first is behind.
    pub glass: Vec<PixelShaderElement>,
    /// Text and glyphs, each centred in its rect.
    pub pieces: Vec<(Arc<Rendered>, Rect)>,
}

/// D2 text at `alpha`: black in light mode, white in dark.
pub fn ink(dark: bool, alpha: f32) -> [u8; 4] {
    let a = (alpha * 255.0).round() as u8;
    if dark { [255, 255, 255, a] } else { [0, 0, 0, a] }
}

/// D2 `label`, `label-secondary`, for drawing code without a `Ui`.
pub fn label(dark: bool) -> [u8; 4] {
    ink(dark, 0.85)
}
pub fn secondary(dark: bool) -> [u8; 4] {
    ink(dark, if dark { 0.55 } else { 0.50 })
}

/// D2 `separator`.
pub fn separator(dark: bool) -> [f32; 4] {
    if dark { [1.0, 1.0, 1.0, 0.10] } else { [0.0, 0.0, 0.0, 0.10] }
}

/// D7 hover / pressed overlay over a control.
pub fn overlay(state: State, dark: bool) -> Option<[f32; 4]> {
    match (state, dark) {
        (State::Hover, false) => Some([0.0, 0.0, 0.0, 0.06]),
        (State::Hover, true) => Some([1.0, 1.0, 1.0, 0.08]),
        (State::Pressed, false) => Some([0.0, 0.0, 0.0, 0.12]),
        (State::Pressed, true) => Some([1.0, 1.0, 1.0, 0.14]),
        _ => None,
    }
}

pub(super) fn fade([r, g, b, a]: [u8; 4], k: f32) -> [u8; 4] {
    [r, g, b, (f32::from(a) * k).round() as u8]
}

fn inset(rect: Rect, by: i32) -> Rect {
    Rectangle::new((rect.loc.x + by, rect.loc.y + by).into(), ((rect.size.w - 2 * by).max(1), (rect.size.h - 2 * by).max(1)).into())
}

impl Ui<'_> {
    /// Text at `alpha`: black in light mode, white in dark.
    pub fn ink(&self, alpha: f32) -> [u8; 4] {
        ink(self.dark, alpha)
    }

    /// D2 `label`, `label-secondary`, `label-tertiary`.
    pub fn label(&self) -> [u8; 4] {
        label(self.dark)
    }
    pub fn secondary(&self) -> [u8; 4] {
        secondary(self.dark)
    }
    pub fn tertiary(&self) -> [u8; 4] {
        self.ink(if self.dark { 0.25 } else { 0.26 })
    }

    /// D2 `fill` × `times`: controls on glass use it at 2× (D7 toggles), so they show.
    pub(super) fn fill(&self, times: f32) -> [f32; 4] {
        if self.dark { [1.0, 1.0, 1.0, 0.08 * times] } else { [0.0, 0.0, 0.0, 0.05 * times] }
    }

    fn accent(&self, alpha: f32) -> [f32; 4] {
        let [r, g, b] = self.accent.map(|c| f32::from(c) / 255.0);
        [r, g, b, alpha]
    }

    fn logical(&self, px: u32) -> i32 {
        (f64::from(px) / self.scale).round() as i32
    }

    pub fn shape(&mut self, rect: Rect, [r, g, b, a]: [f32; 4], radius: f64) {
        self.glass.push(self.shaders.glass_shape(rect, [r, g, b, a], radius, false, self.dark, self.scale));
    }

    /// A D3 material with its D6 shadow, at `radius` (the material's own when `None`).
    pub fn material(&mut self, rect: Rect, material: Material, radius: Option<f64>) {
        let spec = material.spec(self.dark);
        let radius = radius.unwrap_or(spec.radius);
        self.glass.extend(self.shaders.shadow(rect, radius, spec.elevation, self.dark, self.scale, false));
        let [r, g, b] = spec.tint.map(|c| f32::from(c) / 255.0);
        self.glass.push(self.shaders.glass_shape(rect, [r, g, b, spec.opacity * self.glass_opacity], radius, true, self.dark, self.scale));
    }

    /// D7 hover / pressed overlay and focus ring over a control drawn at `rect`.
    pub(super) fn states(&mut self, rect: Rect, radius: f64, state: State) {
        if let Some(overlay) = overlay(state, self.dark) {
            self.shape(rect, overlay, radius);
        }
        if state == State::Focused {
            let ring = self.shaders.ring(rect, radius, 3, self.accent(0.5), self.scale);
            self.glass.push(ring);
        }
    }

    /// One line in `rect`, ellipsised to its width; returns the text's width.
    pub fn text(&mut self, words: &str, style: Style, rect: Rect, align: Align) -> i32 {
        let piece = self.text.label(words, style, rect.size.w.max(1) as u32, self.scale);
        let w = self.logical(piece.width).min(rect.size.w);
        let x = match align {
            Align::Left => rect.loc.x,
            Align::Centre => rect.loc.x + (rect.size.w - w) / 2,
            Align::Right => rect.loc.x + rect.size.w - w,
        };
        self.pieces.push((piece, Rectangle::new((x, rect.loc.y).into(), (w, rect.size.h).into())));
        w
    }

    /// A `size` px symbol centred in `rect`.
    pub fn symbol(&mut self, symbol: Symbol, weight: Weight, size: u16, rect: Rect, color: [u8; 4]) {
        let piece = self.symbols.get(symbol, weight, size, self.scale, color);
        self.pieces.push((piece, rect));
    }

    /// Button (D8): regular 22 px or large 28 tall; `default` = accent fill and white text, else `fill`. Pressed
    /// shrinks to 0.97.
    pub fn button(&mut self, rect: Rect, title: &str, default: bool, state: State) {
        let rect = if state == State::Pressed {
            let (w, h) = ((f64::from(rect.size.w) * 0.97).round() as i32, (f64::from(rect.size.h) * 0.97).round() as i32);
            Rectangle::new((rect.loc.x + (rect.size.w - w) / 2, rect.loc.y + (rect.size.h - h) / 2).into(), (w, h).into())
        } else {
            rect
        };
        let k = state.alpha();
        let [r, g, b, a] = if default { self.accent(1.0) } else { self.fill(2.0) };
        self.shape(rect, [r, g, b, a * k], 6.0);
        self.states(rect, 6.0, state);
        let color = if default { WHITE } else { self.label() };
        self.text(title, Style { color: fade(color, k), ..BODY }, inset(rect, 6), Align::Centre);
    }

    /// Toggle switch (D8): a 26 × 15 pill at `at`, 13 px white knob with a level-2 shadow.
    pub fn switch(&mut self, at: Point<i32, smithay::utils::Logical>, on: bool, state: State) {
        let k = state.alpha();
        let track = Rectangle::new(at, (26, 15).into());
        let [r, g, b, a] = if on { self.accent(1.0) } else { self.fill(2.0) };
        self.shape(track, [r, g, b, a * k], 7.5);
        let knob = Rectangle::new((at.x + if on { 12 } else { 1 }, at.y + 1).into(), (13, 13).into());
        self.glass.extend(self.shaders.shadow(knob, 6.5, 2, self.dark, self.scale, false));
        self.glass.push(self.shaders.light(knob, [1.0, 1.0, 1.0, k], false, self.scale));
        if state == State::Focused {
            self.states(track, 7.5, state);
        }
    }

    /// Round toggle (Control Center, D8): a 28 px circle; on = accent and a white glyph, off = `fill`.
    pub fn round_toggle(&mut self, rect: Rect, symbol: Symbol, on: bool, state: State) {
        let k = state.alpha();
        let [r, g, b, a] = if on { self.accent(1.0) } else { self.fill(2.0) };
        self.glass.push(self.shaders.light(rect, [r, g, b, a * k], false, self.scale));
        self.states(rect, f64::from(rect.size.w) / 2.0, state);
        let color = if on { WHITE } else { self.label() };
        self.symbol(symbol, Weight::Fill, 16, inset(rect, 6), fade(color, k));
    }

    /// Slider (D8): a 28 px pill with the glyph inside at the left; fill white (dark) / `label` (light) at 90%.
    pub fn slider(&mut self, track: Rect, percent: u8, symbol: Symbol) {
        let round = f64::from(track.size.h) / 2.0;
        self.shape(track, self.fill(2.0), round);
        let fill = if self.dark { [1.0, 1.0, 1.0, 0.9] } else { [0.0, 0.0, 0.0, 0.85 * 0.9] };
        self.shape(Rectangle::new(track.loc, (slider_fill(track, percent), track.size.h).into()), fill, round);
        let on_fill = if self.dark { [0, 0, 0, 191] } else { WHITE };
        self.symbol(symbol, Weight::Fill, 16, Rectangle::new((track.loc.x + 6, track.loc.y + 6).into(), (16, 16).into()), on_fill);
    }

    /// Text field (D8): regular 22 px tall, or Spotlight's 52 (Title 1 text); `fill` background, radius 6. Focused:
    /// the caret and the D7 ring; select-all tints the text accent.
    pub fn field(&mut self, rect: Rect, field: &TextField, placeholder: &str, state: State) {
        let k = state.alpha();
        let [r, g, b, a] = self.fill(2.0);
        self.shape(rect, [r, g, b, a * k], 6.0);
        self.field_text(rect, field, placeholder, state, Some(6.0));
    }

    /// The lock screen's password pill (E6.4): `sheet` glass, fully round, without a shadow; the text stops `trailing`
    /// px early (room for the arrow).
    pub fn pill_field(&mut self, rect: Rect, field: &TextField, placeholder: &str, trailing: i32) {
        let spec = Material::Sheet.spec(self.dark);
        let [r, g, b] = spec.tint.map(|c| f32::from(c) / 255.0);
        let radius = f64::from(rect.size.h) / 2.0;
        self.glass.push(self.shaders.glass_shape(rect, [r, g, b, spec.opacity * self.glass_opacity], radius, true, self.dark, self.scale));
        let inner = Rectangle::new((rect.loc.x + 6, rect.loc.y).into(), (rect.size.w - 12 - trailing, rect.size.h).into());
        self.field_text(inner, field, placeholder, State::Focused, None);
    }

    /// A field's text, placeholder, selection and caret, and its D7 ring at `ring` radius, on a background drawn already.
    pub(super) fn field_text(&mut self, rect: Rect, field: &TextField, placeholder: &str, state: State, ring: Option<f64>) {
        let k = state.alpha();
        let large = rect.size.h >= 40;
        let (style, pad) = if large { (TITLE1, 16) } else { (BODY, 6) };
        let inner = Rectangle::new((rect.loc.x + pad, rect.loc.y).into(), (rect.size.w - 2 * pad, rect.size.h).into());
        let shown = field.display();
        if shown.is_empty() {
            self.text(placeholder, Style { color: fade(self.tertiary(), k), ..style }, inner, Align::Left);
        } else {
            // Scrolled so the caret stays inside: drop chars from the front until the text before it fits.
            // ponytail: one rasterised prefix per dropped char, fine for passwords and names; a glyph-advance query
            // on Text when fields hold long text.
            let caret = field.display_caret();
            let mut start = 0;
            while start < caret && self.width(&shown[start..caret], style) > inner.size.w {
                start += shown[start..].chars().next().map_or(1, char::len_utf8);
            }
            let w = self.text(&shown[start..], Style { color: fade(self.label(), k), ..style }, inner, Align::Left);
            if field.all {
                self.shape(Rectangle::new((inner.loc.x - 1, inner.loc.y + 3).into(), (w + 2, inner.size.h - 6).into()), self.accent(0.3), 2.0);
            }
            if state == State::Focused && !field.all {
                let x = inner.loc.x + self.width(&shown[start..caret], style).min(inner.size.w);
                let h = ts(i32::from(style.line_height));
                let caret = Rectangle::new((x, rect.loc.y + (rect.size.h - h) / 2).into(), (if large { 2 } else { 1 }, h).into());
                self.shape(caret, self.accent(1.0), 0.0);
            }
        }
        if shown.is_empty() && state == State::Focused {
            let h = ts(i32::from(style.line_height));
            self.shape(Rectangle::new((inner.loc.x, rect.loc.y + (rect.size.h - h) / 2).into(), (if large { 2 } else { 1 }, h).into()), self.accent(1.0), 0.0);
        }
        if let Some(radius) = ring {
            self.states(rect, radius, if state == State::Focused { State::Focused } else { State::Normal });
        }
    }

    pub fn width(&mut self, words: &str, style: Style) -> i32 {
        let piece = self.text.label(words, style, 4000, self.scale);
        self.logical(piece.width)
    }

    /// List row (D8): 28 px, a 16 px icon, 8 px gap; selected = accent fill and white text.
    pub fn list_row(&mut self, rect: Rect, symbol: Symbol, title: &str, selected: bool, state: State) {
        let k = state.alpha();
        if selected {
            self.shape(rect, self.accent(k), 6.0);
        }
        self.states(rect, 6.0, if state == State::Disabled { State::Normal } else { state });
        let color = fade(if selected { WHITE } else { self.label() }, k);
        let icon = Rectangle::new((rect.loc.x + 8, rect.loc.y + (rect.size.h - 16) / 2).into(), (16, 16).into());
        self.symbol(symbol, Weight::Regular, 16, icon, color);
        let x = icon.loc.x + 16 + 8;
        self.text(title, Style { color, ..BODY }, Rectangle::new((x, rect.loc.y).into(), (rect.loc.x + rect.size.w - 8 - x, rect.size.h).into()), Align::Left);
    }

    /// Menu (D8): `popover`, radius 8, padding 5, 22 px rows with 12 px side padding; the `hovered` row is an accent
    /// pill with white text; shortcuts right in `label-secondary`; separators 1 px with 5 px margins; submenus ›.
    /// Returns its rect.
    pub fn menu(&mut self, at: Point<i32, smithay::utils::Logical>, width: i32, rows: &[MenuItem], hovered: Option<usize>) -> Rect {
        use menu::{PAD, SEPARATOR};
        let (row_h, side_pad) = (ts(menu::ROW), ts(menu::SIDE));
        let height = rows.iter().map(|r| if r.is_some() { row_h } else { SEPARATOR }).sum::<i32>() + 2 * PAD;
        let rect = Rectangle::new(at, (width, height).into());
        self.material(rect, Material::Popover, None);
        let mut y = at.y + PAD;
        for (i, row) in rows.iter().enumerate() {
            let cell = Rectangle::new((at.x + PAD, y).into(), (width - 2 * PAD, if row.is_some() { row_h } else { SEPARATOR }).into());
            y += cell.size.h;
            let Some((label, shortcut, enabled, submenu)) = *row else {
                let line = separator(self.dark);
                self.shape(
                    Rectangle::new((cell.loc.x + side_pad - PAD, cell.loc.y + SEPARATOR / 2).into(), (cell.size.w - 2 * (side_pad - PAD), 1).into()),
                    line,
                    0.0,
                );
                continue;
            };
            let hot = enabled && hovered == Some(i);
            if hot {
                self.shape(cell, self.accent(1.0), 4.0);
            }
            let (main, side) = match (hot, enabled) {
                (true, _) => (WHITE, WHITE),
                (false, true) => (self.label(), self.secondary()),
                (false, false) => (fade(self.label(), 0.4), fade(self.label(), 0.4)),
            };
            let text = Rectangle::new((cell.loc.x + side_pad - PAD, cell.loc.y).into(), (cell.size.w - 2 * (side_pad - PAD), row_h).into());
            self.text(label, Style { color: main, ..BODY }, text, Align::Left);
            if submenu {
                let at = Rectangle::new((text.loc.x + text.size.w - 10, cell.loc.y + (row_h - 10) / 2).into(), (10, 10).into());
                self.symbol(Symbol::CaretRight, Weight::Regular, 10, at, side);
            } else if !shortcut.is_empty() {
                self.text(shortcut, Style { color: side, ..BODY }, text, Align::Right);
            }
        }
        rect
    }

    /// A tooltip's size around `words`: the text plus 6 × 8 padding, grown with the text size.
    pub fn tooltip_size(&mut self, words: &str) -> (i32, i32) {
        (self.width(words, CALLOUT) + ts(16), ts(i32::from(CALLOUT.line_height) + 12))
    }

    /// Tooltip (D8): `popover`, radius 6, Callout text, 6 × 8 padding, level-2 shadow; top-left at `at`.
    pub fn tooltip(&mut self, at: Point<i32, smithay::utils::Logical>, words: &str) {
        let rect = Rectangle::new(at, self.tooltip_size(words).into());
        let spec = Material::Popover.spec(self.dark);
        self.glass.extend(self.shaders.shadow(rect, 6.0, 2, self.dark, self.scale, false));
        let [r, g, b] = spec.tint.map(|c| f32::from(c) / 255.0);
        self.glass.push(self.shaders.glass_shape(rect, [r, g, b, spec.opacity * self.glass_opacity], 6.0, true, self.dark, self.scale));
        self.text(words, Style { color: self.label(), ..CALLOUT }, inset(rect, 6), Align::Centre);
    }

    /// Badge (D8): a red pill, at least 16 px, Caption white text, on the top-right corner of `icon`.
    pub fn badge(&mut self, icon: Rect, count: &str) {
        let h = ts(16);
        let w = (self.width(count, CAPTION) + ts(8)).max(h);
        let rect = Rectangle::new((icon.loc.x + icon.size.w + 4 - w, icon.loc.y - 4).into(), (w, h).into());
        self.shape(rect, RED, f64::from(h) / 2.0);
        self.text(count, Style { color: WHITE, ..CAPTION }, rect, Align::Centre);
    }

    /// Progress bar (D8): a 4 px pill, `fill` track, accent progress.
    pub fn progress(&mut self, track: Rect, percent: u8) {
        let track = Rectangle::new(track.loc, (track.size.w, 4).into());
        self.shape(track, self.fill(2.0), 2.0);
        let w = (track.size.w * i32::from(percent.min(100)) / 100).max(4);
        self.shape(Rectangle::new(track.loc, (w, 4).into()), self.accent(1.0), 2.0);
    }

    /// Segmented control (D8): 22 px, radius 6; the selected segment is `card` with its level-2 shadow.
    pub fn segmented(&mut self, rect: Rect, segments: &[&str], selected: usize) {
        self.shape(rect, self.fill(2.0), 6.0);
        let w = rect.size.w / segments.len().max(1) as i32;
        for (i, words) in segments.iter().enumerate() {
            let segment = Rectangle::new((rect.loc.x + i as i32 * w, rect.loc.y).into(), (w, rect.size.h).into());
            if i == selected {
                self.material(inset(segment, 2), Material::Card, Some(4.0));
            }
            self.text(words, Style { color: self.label(), ..BODY }, inset(segment, 4), Align::Centre);
        }
    }

    /// Alert (D8): `sheet`, 260 wide, radius 12; 64 px icon, Title 2 and Body centred, two buttons side by side
    /// (the right one default). Top-left at `at`; returns its rect.
    pub fn alert(&mut self, at: Point<i32, smithay::utils::Logical>, title: &str, body: &str, buttons: [&str; 2]) -> Rect {
        const WIDTH: i32 = 260;
        const PAD: i32 = 20;
        let inner = WIDTH - 2 * PAD;
        let title_piece = self.text.paragraph(title, Style { color: self.label(), ..TITLE2 }, inner as u32, self.scale);
        let body_piece = self.text.paragraph(body, Style { color: self.label(), ..BODY }, inner as u32, self.scale);
        let (title_h, body_h) = (self.logical(title_piece.height), self.logical(body_piece.height));
        let rect = Rectangle::new(at, (WIDTH, PAD + 64 + 12 + title_h + 8 + body_h + PAD + 28 + PAD).into());
        self.material(rect, Material::Sheet, None);
        let icon = Rectangle::new((at.x + (WIDTH - 64) / 2, at.y + PAD).into(), (64, 64).into());
        self.symbol(Symbol::Logo, Weight::Regular, 64, icon, self.label());
        let title_rect = Rectangle::new((at.x + PAD, icon.loc.y + 64 + 12).into(), (inner, title_h).into());
        let body_rect = Rectangle::new((at.x + PAD, title_rect.loc.y + title_h + 8).into(), (inner, body_h).into());
        self.pieces.push((title_piece, title_rect));
        self.pieces.push((body_piece, body_rect));
        let w = (inner - 8) / 2;
        let y = body_rect.loc.y + body_h + PAD;
        self.button(Rectangle::new((at.x + PAD, y).into(), (w, 28).into()), buttons[0], false, State::Normal);
        self.button(Rectangle::new((at.x + PAD + w + 8, y).into(), (w, 28).into()), buttons[1], true, State::Normal);
        rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_scale_is_ordered() {
        let scale = [TITLE1, TITLE2, TITLE3, BODY, CALLOUT, CAPTION];
        assert!(scale.windows(2).all(|w| w[0].size >= w[1].size && w[0].line_height >= w[1].line_height));
        assert_eq!((HEADLINE.size, HEADLINE.weight), (BODY.size, 600));
    }

    #[test]
    fn colours_match_d2() {
        assert_eq!(label(true), [255, 255, 255, 217]);
        assert_eq!(secondary(false), [0, 0, 0, 128]);
        assert_eq!(secondary(true), [255, 255, 255, 140]);
        assert_eq!(fade(label(false), 0.4), [0, 0, 0, 87]);
    }
}
