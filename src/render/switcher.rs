//! The app switcher (E4.5): a centred `hud` panel, radius 18, with a 96 px icon per app; the selected one sits on a
//! white @ 20% highlight (radius 12) and its name is under the strip in Body.

use std::sync::Arc;

use smithay::{
    backend::renderer::gles::{GlesRenderer, element::PixelShaderElement},
    utils::{Logical, Rectangle, Size},
};

use super::{
    Material, SceneElement, Shaders,
    titlebar::{Label, keep_buffers, label_element},
};
use crate::{
    config::Accent,
    shell::switcher::layout::{self, RADIUS},
    text::{Rendered, Style, Text},
    wm::Rect,
};

/// An app's icon in `rect`: the glass behind it and the label on it.
// ponytail: a tile with the name's first letter until the app index (M3, dock) gives real icons; swap this one
// function for an icon lookup then.
pub(super) fn app_icon(shaders: &Shaders, text: &mut Text, name: &str, rect: Rect, dark: bool, scale: f64) -> (PixelShaderElement, Arc<Rendered>) {
    const HUES: [Accent; 7] = [Accent::Blue, Accent::Purple, Accent::Pink, Accent::Red, Accent::Orange, Accent::Green, Accent::Graphite];
    let hash = name.bytes().fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(usize::from(b)));
    let [r, g, b] = super::accent(HUES[hash % HUES.len()], dark).map(|c| f32::from(c) / 255.0);
    // WhiteSur squircles round about 22% of their size.
    let tile = shaders.glass_shape(rect, [r, g, b, 1.0], f64::from(rect.size.w) * 0.225, true, dark, scale);
    let letter = name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    let size = (rect.size.h / 2).max(8) as u16;
    let style = Style { size, line_height: size + size / 4, weight: 600, color: [255, 255, 255, 255], tabular: false };
    (tile, text.label(&letter, style, rect.size.w as u32, scale))
}

/// Panel, highlight, names, dark, opacity, scale: what the glass depends on.
type Key = (Rect, Rect, Vec<String>, bool, u32, u64);

#[derive(Default)]
pub struct SwitcherPanel {
    glass: Option<(Key, Vec<PixelShaderElement>)>,
    labels: Vec<Label>,
}

impl SwitcherPanel {
    /// Front to back: letters and the name, the icon tiles, the highlight, the panel, its shadow.
    #[expect(clippy::too_many_arguments, reason = "the frame's drawing context")]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        names: &[String],
        selected: usize,
        output: Size<i32, Logical>,
        dark: bool,
        glass_opacity: f32,
        scale: f64,
    ) -> Vec<SceneElement> {
        let (panel, cells, name_rect) = crate::shell::switcher::layout(output, names.len());
        let inset = (layout::CELL - layout::ICON) / 2;
        let icons: Vec<Rect> =
            cells.iter().map(|c| Rectangle::new((c.loc.x + inset, c.loc.y + inset).into(), (c.size.w - 2 * inset, c.size.h - 2 * inset).into())).collect();
        let highlight = cells.get(selected).copied().unwrap_or_default();

        // The tiles are cheap to make; they are kept only when the glass is rebuilt, so their damage ids last.
        let (tiles, letters): (Vec<_>, Vec<_>) = names.iter().zip(&icons).map(|(name, icon)| app_icon(shaders, text, name, *icon, dark, scale)).unzip();
        let key = (panel, highlight, names.to_vec(), dark, glass_opacity.to_bits(), scale.to_bits());
        if self.glass.as_ref().is_none_or(|(k, _)| *k != key) {
            let mut glass = tiles;
            // White @ 20% (E4.5) vanishes on light hud glass: there, black @ 10%.
            let fill = if dark { [1.0, 1.0, 1.0, 0.20] } else { [0.0, 0.0, 0.0, 0.10] };
            glass.push(shaders.glass_shape(highlight, fill, 12.0, false, dark, scale));
            let spec = Material::Hud.spec(dark);
            let [r, g, b] = spec.tint.map(|c| f32::from(c) / 255.0);
            glass.push(shaders.glass_shape(panel, [r, g, b, spec.opacity * glass_opacity], RADIUS, true, dark, scale));
            glass.extend(shaders.shadow(panel, RADIUS, spec.elevation, dark, scale, false));
            self.glass = Some((key, glass));
        }

        let label = if dark { [255, 255, 255, 217] } else { [0, 0, 0, 217] };
        let body = Style { size: 13, line_height: 16, weight: 400, color: label, tabular: false };
        let name = text.label(names.get(selected).map_or("", String::as_str), body, name_rect.size.w as u32, scale);
        let rects = icons.iter().copied().chain([name_rect]);
        self.labels = keep_buffers(&self.labels, letters.into_iter().chain([name]));
        let mut out: Vec<SceneElement> = self.labels.iter().zip(rects).filter_map(|(l, r)| label_element(renderer, l, r, scale)).collect();
        if let Some((_, glass)) = &self.glass {
            out.extend(glass.iter().cloned().map(SceneElement::Shader));
        }
        out
    }
}
