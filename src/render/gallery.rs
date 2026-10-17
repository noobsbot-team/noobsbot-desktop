//! The E1.4 test scene (`--test-scene`): every D3 material in light (left half) and dark (right half), each with its
//! D6 shadow and its name, over the wallpaper and under any windows. Compare against the mockup's Materials board.

use smithay::{
    backend::renderer::gles::GlesRenderer,
    utils::{Logical, Rectangle, Size},
};

use super::{
    Material, SceneElement, Shaders,
    titlebar::{Label, label_element},
};
use crate::text::{self, Style, Text};

const COLUMNS: i32 = 3;
const GAP: i32 = 32;
const TOP: i32 = 80;
const CARD_HEIGHT: i32 = 130;

#[derive(Default)]
pub struct Gallery {
    /// Name labels per (dark, material), rebuilt when the scale changes.
    labels: Vec<Label>,
    scale: u64,
}

impl Gallery {
    /// Front to back: per card its label, glass and shadow.
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        output: Size<i32, Logical>,
        glass_opacity: f32,
        scale: f64,
    ) -> Vec<SceneElement> {
        if self.scale != scale.to_bits() || self.labels.is_empty() {
            self.scale = scale.to_bits();
            self.labels = [false, true]
                .into_iter()
                .flat_map(|dark| Material::ALL.map(move |m| (dark, m)))
                .map(|(dark, material)| {
                    let spec = material.spec(dark);
                    let color = if dark { [255, 255, 255, 217] } else { [0, 0, 0, 217] };
                    let name = format!("{material:?} · {:.0}% · r{}", spec.opacity * 100.0, spec.radius);
                    let rendered = text.label(&name, Style { size: 13, line_height: 16, weight: 600, color }, 200, scale);
                    let buffer = text::to_buffer(&rendered);
                    (rendered, buffer)
                })
                .collect();
        }
        let half = output.w / 2;
        let width = (half - (COLUMNS + 1) * GAP) / COLUMNS;
        let mut out = Vec::new();
        for (i, (dark, material)) in [false, true].into_iter().flat_map(|dark| Material::ALL.map(move |m| (dark, m))).enumerate() {
            let n = i as i32 % Material::ALL.len() as i32;
            let x = if dark { half } else { 0 } + GAP + (n % COLUMNS) * (width + GAP);
            let y = TOP + (n / COLUMNS) * (CARD_HEIGHT + GAP);
            let rect = Rectangle::new((x, y).into(), (width, CARD_HEIGHT).into());
            let spec = material.spec(dark);
            out.extend(label_element(renderer, &self.labels[i], rect, scale));
            out.push(SceneElement::Shader(shaders.glass(rect, material, dark, glass_opacity, scale)));
            out.extend(shaders.shadow(rect, spec.radius, spec.elevation, dark, scale, false).map(SceneElement::Shader));
        }
        out
    }
}
