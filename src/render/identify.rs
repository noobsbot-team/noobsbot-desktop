//! Displays ▸ Identify (E6.2): each display's name in big text on a `hud` glass plate in its middle, for
//! `SHOWN_FOR`, so the arrangement in System Settings can be matched to the screens.

use std::{collections::HashMap, time::Duration};

use smithay::{
    backend::renderer::gles::GlesRenderer,
    utils::{Logical, Rectangle, Size},
};

use super::{
    Material, SceneElement, Shaders,
    titlebar::{Label, keep_buffers, label_element},
};
use crate::{
    text::{Style, Text},
    wm::Rect,
};

pub const SHOWN_FOR: Duration = Duration::from_secs(2);
const TEXT: u16 = 56;
const PAD: (i32, i32) = (48, 28);

/// The plate around a `label` (logical px) on an output of `size`: centred, the label's size plus padding, never
/// wider than the output less 32 px each side.
pub fn plate(size: Size<i32, Logical>, label: (i32, i32)) -> Rect {
    let w = (label.0 + 2 * PAD.0).min(size.w - 64).max(0);
    let h = label.1 + 2 * PAD.1;
    Rectangle::new(((size.w - w) / 2, (size.h - h) / 2).into(), (w, h).into())
}

/// Each output's label, by output name, kept while it shows.
#[derive(Default)]
pub struct IdentifyLabels(HashMap<String, Vec<Label>>);

impl IdentifyLabels {
    /// `title` on its plate in the middle of an output `size` big, front to back; `None` frees the buffers.
    #[allow(clippy::too_many_arguments)]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        output: &str,
        title: Option<&str>,
        size: Size<i32, Logical>,
        dark: bool,
        glass_opacity: f32,
        scale: f64,
    ) -> Vec<SceneElement> {
        let Some(title) = title else {
            self.0.clear();
            return Vec::new();
        };
        let color = if dark { [255, 255, 255, 230] } else { [0, 0, 0, 217] };
        let style = Style { size: TEXT, line_height: TEXT + 12, weight: 600, color, tabular: false };
        let line = text.label(title, style, (size.w - 64 - 2 * PAD.0).max(1) as u32, scale);
        let label = ((f64::from(line.width) / scale).round() as i32, (f64::from(line.height) / scale).round() as i32);
        let labels = self.0.entry(output.to_owned()).or_default();
        *labels = keep_buffers(labels, [line]);
        let rect = plate(size, label);
        let spec = Material::Hud.spec(dark);
        let [r, g, b] = spec.tint.map(|c| f32::from(c) / 255.0);
        let radius = 24.0;
        let mut out: Vec<SceneElement> = labels.iter().filter_map(|l| label_element(renderer, l, rect, scale)).collect();
        out.push(SceneElement::Shader(shaders.glass_shape(rect, [r, g, b, spec.opacity * glass_opacity], radius, true, dark, scale)));
        out.extend(shaders.shadow(rect, radius, spec.elevation, dark, scale, false).map(SceneElement::Shader));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plate_is_centred_and_fits() {
        let size = Size::from((1536, 960));
        let r = plate(size, (400, 68));
        assert_eq!((r.size.w, r.size.h), (496, 124));
        assert_eq!((r.loc.x, r.loc.y), ((1536 - 496) / 2, (960 - 124) / 2));
        // A name wider than a small display stays 32 px in from each side.
        let r = plate(Size::from((640, 480)), (900, 68));
        assert_eq!((r.loc.x, r.size.w), (32, 576));
    }
}
