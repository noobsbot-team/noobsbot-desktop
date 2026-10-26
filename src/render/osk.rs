//! The on-screen keyboard (E8.6): a `panel` glass slab, radius 14, with keys on `fill` × 2 (radius 6) and their
//! labels; latched modifiers on the accent.

use smithay::backend::renderer::gles::{GlesRenderer, element::PixelShaderElement};

use super::{
    Material, SceneElement, Shaders,
    titlebar::{Label, keep_buffers, label_element},
    ui::{Align, BODY, CALLOUT, Ui},
};
use crate::{
    shell::osk::{self, RADIUS},
    symbols::Symbols,
    text::{Rendered, Style, Text},
    wm::Rect,
};

/// Output size, labels, latched, dark, accent, opacity, scale.
type Key = ((i32, i32), Vec<String>, Vec<u32>, bool, [u8; 3], u32, u64);
type Drawn = (Key, Vec<PixelShaderElement>, Vec<(std::sync::Arc<Rendered>, Rect)>);

#[derive(Default)]
pub struct OnScreenKeyboard {
    cache: Option<Drawn>,
    labels: Vec<Label>,
}

impl OnScreenKeyboard {
    /// Front to back: labels, keys, the panel. Rebuilt only when what it shows changes.
    #[expect(clippy::too_many_arguments, reason = "the frame's drawing context")]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        symbols: &mut Symbols,
        output: smithay::utils::Size<i32, smithay::utils::Logical>,
        key_labels: Vec<String>,
        latched: &[u32],
        dark: bool,
        accent: [u8; 3],
        glass_opacity: f32,
        scale: f64,
    ) -> Vec<SceneElement> {
        let key = ((output.w, output.h), key_labels, latched.to_vec(), dark, accent, glass_opacity.to_bits(), scale.to_bits());
        if self.cache.as_ref().is_none_or(|(k, ..)| *k != key) {
            let mut ui = Ui { shaders, text, symbols, dark, accent, glass_opacity, scale, glass: Vec::new(), pieces: Vec::new() };
            ui.material(osk::panel(output), Material::Panel, Some(RADIUS));
            let [r, g, b] = accent.map(|c| f32::from(c) / 255.0);
            for ((rect, (code, _, own)), label) in osk::key_rects(output).into_iter().zip(osk::keys()).zip(&key.1) {
                let on = latched.contains(&code);
                ui.shape(rect, if on { [r, g, b, 1.0] } else { ui.fill(2.0) }, 6.0);
                // Letters in the body size, the named keys (and the space bar's empty label) smaller.
                let style = if own.is_empty() { BODY } else { CALLOUT };
                let color = if on { [255; 4] } else { ui.label() };
                ui.text(label, Style { color, ..style }, rect, Align::Centre);
            }
            let (mut glass, pieces) = (ui.glass, ui.pieces);
            glass.reverse();
            self.cache = Some((key, glass, pieces));
        }
        let Some((_, glass, pieces)) = &self.cache else { return Vec::new() };
        self.labels = keep_buffers(&self.labels, pieces.iter().map(|(p, _)| p.clone()));
        let mut out: Vec<SceneElement> = self.labels.iter().zip(pieces).filter_map(|(label, (_, r))| label_element(renderer, label, *r, scale)).collect();
        out.extend(glass.iter().cloned().map(SceneElement::Shader));
        out
    }
}
