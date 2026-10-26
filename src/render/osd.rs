//! The on-screen display for media keys (E2.7): a `hud` pill under the bar at the top right, with a 24 px symbol and a
//! progress bar plus the value, or a line of text (the track for play/pause).

use smithay::{
    backend::renderer::gles::{GlesRenderer, element::PixelShaderElement},
    utils::Rectangle,
};

use super::{
    Material, SceneElement, Shaders,
    titlebar::{Label, keep_buffers, label_element},
};
use crate::{
    shell::{self, Osd},
    symbols::{Symbols, Weight},
    text::{Style, Text, ts},
    wm::Rect,
};

const PAD: i32 = 14;
const SYMBOL: i32 = 24;
const GAP: i32 = 12;
const VALUE: i32 = 32;
const BAR_H: i32 = 4;

/// What the pill, its bar and shadow depend on: rect, level, dark, accent, opacity, scale.
type Key = (Rect, Option<u8>, bool, [u8; 3], u32, u64);

#[derive(Default)]
pub struct OsdPopover {
    glass: Option<(Key, Vec<PixelShaderElement>)>,
    labels: Vec<Label>,
}

impl OsdPopover {
    /// Front to back: symbol and value, the progress bar, the pill, its shadow.
    #[allow(clippy::too_many_arguments)]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        symbols: &mut Symbols,
        osd: &Osd,
        output_width: i32,
        dark: bool,
        accent: [u8; 3],
        glass_opacity: f32,
        scale: f64,
    ) -> Vec<SceneElement> {
        let rect = shell::osd_rect(output_width);
        let (x, y, h) = (rect.loc.x, rect.loc.y, rect.size.h);
        let symbol = Rectangle::new((x + PAD, y + (h - SYMBOL) / 2).into(), (SYMBOL, SYMBOL).into());
        let content_x = x + PAD + SYMBOL + GAP;
        // The pill's 48 px hold a line of text up to 2×; only the value's column has to widen with it.
        let value_w = ts(VALUE);
        let value = Rectangle::new((x + rect.size.w - PAD - value_w, y).into(), (value_w, h).into());
        let track = Rectangle::new((content_x, y + (h - BAR_H) / 2).into(), (value.loc.x - GAP - content_x, BAR_H).into());

        let key = (rect, osd.level, dark, accent, glass_opacity.to_bits(), scale.to_bits());
        if self.glass.as_ref().is_none_or(|(k, _)| *k != key) {
            let mut glass = Vec::new();
            if let Some(level) = osd.level {
                // D8 progress bar: accent on a `fill` track (at 2×, as toggles use it, so it shows on hud glass).
                let filled = track.size.w * i32::from(level.min(100)) / 100;
                if filled > 0 {
                    let [r, g, b] = accent.map(|c| f32::from(c) / 255.0);
                    let bar = Rectangle::new(track.loc, (filled.max(BAR_H), BAR_H).into());
                    glass.push(shaders.glass_shape(bar, [r, g, b, 1.0], f64::from(BAR_H) / 2.0, false, dark, scale));
                }
                let fill = if dark { [1.0, 1.0, 1.0, 0.16] } else { [0.0, 0.0, 0.0, 0.10] };
                glass.push(shaders.glass_shape(track, fill, f64::from(BAR_H) / 2.0, false, dark, scale));
            }
            let spec = Material::Hud.spec(dark);
            let [r, g, b] = spec.tint.map(|c| f32::from(c) / 255.0);
            let radius = f64::from(h) / 2.0;
            glass.push(shaders.glass_shape(rect, [r, g, b, spec.opacity * glass_opacity], radius, true, dark, scale));
            glass.extend(shaders.shadow(rect, radius, spec.elevation, dark, scale, false));
            self.glass = Some((key, glass));
        }

        let color = if dark { [255, 255, 255, 217] } else { [0, 0, 0, 217] };
        let body = Style { size: 13, line_height: 16, weight: 400, color, tabular: true };
        let (words, place) = match (osd.level, &osd.text) {
            (Some(level), _) => (level.to_string(), value),
            (None, text) => (text.clone().unwrap_or_default(), Rectangle::new((content_x, y).into(), (value.loc.x + value_w - content_x, h).into())),
        };
        let line = text.label(&words, body, place.size.w as u32, scale);
        // The value is right-aligned, a line of text left-aligned.
        let w = (f64::from(line.width) / scale).round() as i32;
        let place = match osd.level {
            Some(_) => Rectangle::new((place.loc.x + place.size.w - w, y).into(), (w, h).into()),
            None => Rectangle::new(place.loc, (w, h).into()),
        };
        self.labels = keep_buffers(&self.labels, [symbols.get(osd.symbol, Weight::Fill, SYMBOL as u16, scale, color), line]);
        let mut out: Vec<SceneElement> =
            self.labels.iter().zip([symbol, place]).filter_map(|(label, rect)| label_element(renderer, label, rect, scale)).collect();
        if let Some((_, glass)) = &self.glass {
            out.extend(glass.iter().cloned().map(SceneElement::Shader));
        }
        out
    }
}
