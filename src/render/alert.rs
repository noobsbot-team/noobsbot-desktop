//! Alerts (D8): `sheet` glass, 260 px wide, radius 12, centred on the output; the system icon at 64 px, Title 2 and
//! Body centred, a text field when it asks for one (Wi-Fi password), then Cancel and the default (accent) button
//! side by side.

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

/// What the glass depends on: sheet and confirm-button rects, dark, accent, opacity, scale.
type Key = (Rect, Rect, bool, [u8; 3], u32, u64);
/// The field drawn (its state, rect, dark, accent, scale): glass front to back, and its text pieces.
type FieldDrawn = ((TextField, Rect, bool, [u8; 3], u64), Vec<PixelShaderElement>, Vec<(Arc<Rendered>, Rect)>);

#[derive(Default)]
pub struct AlertSheet {
    glass: Option<(Key, Vec<PixelShaderElement>)>,
    labels: Vec<Label>,
    field: Option<FieldDrawn>,
    field_labels: Vec<Label>,
    /// The sheet and its (cancel, confirm) buttons as last drawn, output-relative.
    layout: Option<(Rect, [Rect; 2])>,
}

impl AlertSheet {
    pub fn rect(&self) -> Option<Rect> {
        self.layout.map(|(rect, _)| rect)
    }

    pub fn button_at(&self, location: Point<f64, Logical>) -> Option<Button> {
        let (_, [cancel, confirm]) = self.layout?;
        [(cancel, Button::Cancel), (confirm, Button::Confirm)].into_iter().find(|(r, _)| r.to_f64().contains(location)).map(|(_, b)| b)
    }

    /// The alert is gone: no more clicks land on it.
    pub fn hide(&mut self) {
        self.layout = None;
    }

    /// Front to back: icon, text and button labels, the buttons, the sheet, its shadow.
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
    ) -> Vec<SceneElement> {
        let label = if dark { [255, 255, 255, 217] } else { [0, 0, 0, 217] };
        let title2 = Style { size: 17, line_height: 22, weight: 600, color: label, tabular: false };
        let body = Style { size: 13, line_height: 16, weight: 400, color: label, tabular: false };
        let title = text.paragraph(&alert.title(), title2, INNER as u32, scale);
        let words = text.paragraph(&alert.body(Instant::now()), body, INNER as u32, scale);
        let logical = |px: u32| (f64::from(px) / scale).round() as i32;
        let (title_h, body_h) = (logical(title.height), logical(words.height));

        let field_h = if alert.field().is_some() { 12 + FIELD_H } else { 0 };
        let height = PAD + ICON + 12 + title_h + 8 + body_h + field_h + PAD + BUTTON_H + PAD;
        let x = (output.w - WIDTH) / 2;
        let y = ((output.h - height) / 2).max(0);
        let rect = Rectangle::new((x, y).into(), (WIDTH, height).into());
        let icon = Rectangle::new((x + (WIDTH - ICON) / 2, y + PAD).into(), (ICON, ICON).into());
        let title_rect = Rectangle::new((x + PAD, icon.loc.y + ICON + 12).into(), (INNER, title_h).into());
        let body_rect = Rectangle::new((x + PAD, title_rect.loc.y + title_h + 8).into(), (INNER, body_h).into());
        let button_w = if alert.cancellable() { (INNER - BUTTON_GAP) / 2 } else { INNER };
        // Shaken sideways after a wrong password (polkit).
        let field_rect = Rectangle::new((x + PAD + alert.shake().unwrap_or(0), body_rect.loc.y + body_h + 12).into(), (INNER, FIELD_H).into());
        let button_y = body_rect.loc.y + body_h + field_h + PAD;
        let cancel = Rectangle::new((x + PAD, button_y).into(), (button_w, BUTTON_H).into());
        let confirm = Rectangle::new((x + PAD + INNER - button_w, button_y).into(), (button_w, BUTTON_H).into());

        let key = (rect, confirm, dark, accent, glass_opacity.to_bits(), scale.to_bits());
        if self.glass.as_ref().is_none_or(|(k, _)| *k != key) {
            let [r, g, b] = accent.map(|c| f32::from(c) / 255.0);
            // D8 secondary button: `fill` (at 2×, so it shows on the sheet).
            let fill = if dark { [1.0, 1.0, 1.0, 0.16] } else { [0.0, 0.0, 0.0, 0.10] };
            let spec = Material::Sheet.spec(dark);
            let [sr, sg, sb] = spec.tint.map(|c| f32::from(c) / 255.0);
            let mut glass = vec![shaders.glass_shape(confirm, [r, g, b, 1.0], 6.0, false, dark, scale)];
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
        ];
        self.labels = keep_buffers(&self.labels, pieces);
        let rects = [icon, title_rect, body_rect, cancel, confirm];
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
        self.layout = Some((rect, [cancel, confirm]));
        out
    }
}
