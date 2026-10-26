//! Alerts (D8): `sheet` glass, 260 px wide, radius 12, centred on the output; the system icon at 64 px, Title 2 and
//! Body centred, a text field when it asks for one (Wi-Fi password), then Cancel and the default (accent) button
//! side by side. A power alert has a checkbox above the buttons: reopen windows at the next login (E8.8).

use std::{sync::Arc, time::Instant};

use smithay::{
    backend::renderer::gles::{GlesRenderer, element::PixelShaderElement},
    utils::{Logical, Point, Rectangle, Size},
};

use super::{
    Material, SceneElement, Shaders,
    titlebar::{Label, keep_buffers, label_element},
    ui::{State, Ui},
};
use crate::{
    shell::alert::{Alert, Button, Kind},
    symbols::{Symbol, Symbols, Weight},
    text::{Rendered, Style, Text},
    ui::field::TextField,
    wm::Rect,
};

const WIDTH: i32 = 260;
const PAD: i32 = 20;
const INNER: i32 = WIDTH - 2 * PAD;
const ICON: i32 = 64;
const BUTTON_H: i32 = 28;
const BUTTON_GAP: i32 = 8;
const RADIUS: f64 = 12.0;
const FIELD_H: i32 = 22;
const TITLE_LINES: u8 = 2;
const BODY_LINE: i32 = 16;
/// The checkbox, and the gap between it and its label.
const CHECK: i32 = 14;
const CHECK_GAP: i32 = 6;
/// The sheet keeps this far from the screen's top and bottom.
const MARGIN: i32 = 40;

/// Body lines that leave the buttons on a screen `output_h` tall, under a `title_h`-tall title (and a field):
/// the text is ellipsised past that.
fn body_lines(output_h: i32, title_h: i32, field_h: i32) -> u8 {
    let fixed = 2 * MARGIN + PAD + ICON + 12 + title_h + 8 + field_h + PAD + BUTTON_H + PAD;
    ((output_h - fixed) / BODY_LINE).clamp(1, i32::from(u8::MAX)) as u8
}

/// What the glass depends on: sheet and confirm-button rects, the checkbox's state, dark, accent, opacity, scale.
type Key = (Rect, Rect, Option<bool>, bool, [u8; 3], u32, u64);
/// The field drawn (its state, rect, dark, accent, scale): glass front to back, and its text pieces.
type FieldDrawn = ((TextField, Rect, bool, [u8; 3], u64), Vec<PixelShaderElement>, Vec<(Arc<Rendered>, Rect)>);

#[derive(Default)]
pub struct AlertSheet {
    glass: Option<(Key, Vec<PixelShaderElement>)>,
    labels: Vec<Label>,
    field: Option<FieldDrawn>,
    field_labels: Vec<Label>,
    /// The sheet, its (cancel, confirm) buttons and its checkbox row as last drawn, output-relative.
    layout: Option<(Rect, [Rect; 3])>,
}

impl AlertSheet {
    pub fn rect(&self) -> Option<Rect> {
        self.layout.map(|(rect, _)| rect)
    }

    pub fn button_at(&self, location: Point<f64, Logical>) -> Option<Button> {
        let (_, [cancel, confirm, reopen]) = self.layout?;
        [(cancel, Button::Cancel), (confirm, Button::Confirm), (reopen, Button::Reopen)]
            .into_iter()
            .find(|(r, _)| r.to_f64().contains(location))
            .map(|(_, b)| b)
    }

    /// The alert is gone: no more clicks land on it.
    pub fn hide(&mut self) {
        self.layout = None;
    }

    /// Front to back: icon, text and button labels, the buttons, the sheet, its shadow. `reopen`: the checkbox's
    /// state, shown on a power alert.
    #[allow(clippy::too_many_arguments)]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        symbols: &mut Symbols,
        alert: &Alert,
        output: Size<i32, Logical>,
        dark: bool,
        accent: [u8; 3],
        glass_opacity: f32,
        scale: f64,
        reopen: bool,
    ) -> Vec<SceneElement> {
        let label = if dark { [255, 255, 255, 217] } else { [0, 0, 0, 217] };
        let title2 = Style { size: 17, line_height: 22, weight: 600, color: label, tabular: false };
        let body = Style { size: 13, line_height: 16, weight: 400, color: label, tabular: false };
        let logical = |px: u32| (f64::from(px) / scale).round() as i32;
        let field_h = if alert.field().is_some() { 12 + FIELD_H } else { 0 };
        let title = text.centred(&alert.title(), title2, INNER as u32, TITLE_LINES, scale);
        let title_h = logical(title.height);
        let words = text.centred(&alert.body(Instant::now()), body, INNER as u32, body_lines(output.h, title_h, field_h), scale);
        let body_h = logical(words.height);
        let reopen = matches!(alert.kind, Kind::Power(_)).then_some(reopen);
        let small = Style { size: 11, line_height: 14, ..body };
        let check_label =
            text.wrapped(if reopen.is_some() { "Reopen windows when logging back in" } else { "" }, small, (INNER - CHECK - CHECK_GAP) as u32, 2, scale);
        let check_h = if reopen.is_some() { 12 + logical(check_label.height).max(CHECK) } else { 0 };

        let height = PAD + ICON + 12 + title_h + 8 + body_h + field_h + check_h + PAD + BUTTON_H + PAD;
        let x = (output.w - WIDTH) / 2;
        let y = ((output.h - height) / 2).max(0);
        let rect = Rectangle::new((x, y).into(), (WIDTH, height).into());
        let icon = Rectangle::new((x + (WIDTH - ICON) / 2, y + PAD).into(), (ICON, ICON).into());
        let title_rect = Rectangle::new((x + PAD, icon.loc.y + ICON + 12).into(), (INNER, title_h).into());
        let body_rect = Rectangle::new((x + PAD, title_rect.loc.y + title_h + 8).into(), (INNER, body_h).into());
        let button_w = if alert.cancellable() { (INNER - BUTTON_GAP) / 2 } else { INNER };
        // Shaken sideways after a wrong password (polkit).
        let field_rect = Rectangle::new((x + PAD + alert.shake().unwrap_or(0), body_rect.loc.y + body_h + 12).into(), (INNER, FIELD_H).into());
        let check_row = Rectangle::new((x + PAD, body_rect.loc.y + body_h + field_h + 12).into(), (INNER, check_h - 12).into());
        let check = Rectangle::new((check_row.loc.x, check_row.loc.y + (check_row.size.h - CHECK) / 2).into(), (CHECK, CHECK).into());
        let check_text = Rectangle::new(
            (check.loc.x + CHECK + CHECK_GAP, check_row.loc.y + (check_row.size.h - logical(check_label.height)) / 2).into(),
            (logical(check_label.width), logical(check_label.height)).into(),
        );
        let button_y = body_rect.loc.y + body_h + field_h + check_h + PAD;
        let cancel = Rectangle::new((x + PAD, button_y).into(), (button_w, BUTTON_H).into());
        let confirm = Rectangle::new((x + PAD + INNER - button_w, button_y).into(), (button_w, BUTTON_H).into());

        let key = (rect, confirm, reopen, dark, accent, glass_opacity.to_bits(), scale.to_bits());
        if self.glass.as_ref().is_none_or(|(k, _)| *k != key) {
            let [r, g, b] = accent.map(|c| f32::from(c) / 255.0);
            // D8 secondary button: `fill` (at 2×, so it shows on the sheet).
            let fill = if dark { [1.0, 1.0, 1.0, 0.16] } else { [0.0, 0.0, 0.0, 0.10] };
            let spec = Material::Sheet.spec(dark);
            let [sr, sg, sb] = spec.tint.map(|c| f32::from(c) / 255.0);
            let mut glass = vec![shaders.glass_shape(confirm, [r, g, b, 1.0], 6.0, false, dark, scale)];
            match reopen {
                Some(true) => glass.push(shaders.glass_shape(check, [r, g, b, 1.0], 4.0, false, dark, scale)),
                Some(false) => glass.push(shaders.glass_shape(check, fill, 4.0, false, dark, scale)),
                None => {}
            }
            if cancel != confirm {
                glass.push(shaders.glass_shape(cancel, fill, 6.0, false, dark, scale));
            }
            glass.push(shaders.glass_shape(rect, [sr, sg, sb, spec.opacity * glass_opacity], RADIUS, true, dark, scale));
            glass.extend(shaders.shadow(rect, RADIUS, spec.elevation, dark, scale, false));
            self.glass = Some((key, glass));
        }

        let pieces = [
            match alert.kind {
                Kind::Pair(_) | Kind::Pin { .. } => symbols.get(Symbol::Bluetooth, Weight::Regular, ICON as u16, scale, label),
                Kind::Wifi { .. } => symbols.get(Symbol::WifiHigh, Weight::Regular, ICON as u16, scale, label),
                Kind::Polkit { .. } => symbols.get(Symbol::Lock, Weight::Regular, ICON as u16, scale, label),
                Kind::ScreenRecording { .. } => symbols.get(Symbol::Record, Weight::Regular, ICON as u16, scale, label),
                Kind::Power(_) | Kind::NotResponding { .. } => symbols.get(Symbol::Logo, Weight::Regular, ICON as u16, scale, label),
            },
            title,
            words,
            text.label(if cancel == confirm { "" } else { alert.cancel() }, body, button_w as u32, scale),
            text.label(alert.confirm(), Style { color: [255, 255, 255, 255], ..body }, button_w as u32, scale),
            symbols.get(Symbol::Check, Weight::Regular, (CHECK - 4) as u16, scale, if reopen == Some(true) { [255, 255, 255, 255] } else { [0; 4] }),
            check_label,
        ];
        self.labels = keep_buffers(&self.labels, pieces);
        let rects = [icon, title_rect, body_rect, cancel, confirm, check, check_text];
        let mut out: Vec<SceneElement> = self.labels.iter().zip(rects).filter_map(|(label, rect)| label_element(renderer, label, rect, scale)).collect();
        match alert.field() {
            Some(field) => {
                let key = (field.clone(), field_rect, dark, accent, scale.to_bits());
                if self.field.as_ref().is_none_or(|(k, _, _)| *k != key) {
                    let mut ui = Ui { shaders, text, symbols, dark, accent, glass_opacity, scale, glass: Vec::new(), pieces: Vec::new() };
                    ui.field(field_rect, field, if field.secure { "Password" } else { "" }, State::Focused);
                    let (mut glass, pieces) = (ui.glass, ui.pieces);
                    glass.reverse();
                    self.field = Some((key, glass, pieces));
                }
                if let Some((_, glass, pieces)) = &self.field {
                    self.field_labels = keep_buffers(&self.field_labels, pieces.iter().map(|(p, _)| p.clone()));
                    out.extend(self.field_labels.iter().zip(pieces).filter_map(|(label, (_, r))| label_element(renderer, label, *r, scale)));
                    out.extend(glass.iter().cloned().map(SceneElement::Shader));
                }
            }
            None => self.field = None,
        }
        if let Some((_, glass)) = &self.glass {
            out.extend(glass.iter().cloned().map(SceneElement::Shader));
        }
        self.layout = Some((rect, [cancel, confirm, check_row]));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_body_gives_up_lines_before_the_buttons_leave_the_screen() {
        // A 768 px screen, two title lines, no field: as many lines as leave the sheet inside its margins.
        let lines = body_lines(768, 44, 0);
        let fixed = 2 * MARGIN + PAD + ICON + 12 + 44 + 8 + PAD + BUTTON_H + PAD;
        assert!(fixed + i32::from(lines) * BODY_LINE <= 768 && fixed + i32::from(lines + 1) * BODY_LINE > 768);
        assert!(body_lines(768, 44, 12 + FIELD_H) < lines, "a field costs lines");
        assert_eq!(body_lines(200, 44, 0), 1, "never no lines at all");
        assert!(body_lines(2160, 22, 0) > 100, "a tall screen hardly limits it");
    }
}
