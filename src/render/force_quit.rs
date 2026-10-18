//! The Force Quit window (E4.7): `sheet` glass, radius 12, elevation 5, centred; traffic lights and the title, two
//! lines of Callout, the app list (28 px rows, 20 px icons, frozen apps marked in red) and the accent Force Quit button.

use smithay::{
    backend::renderer::gles::{GlesRenderer, element::PixelShaderElement},
    utils::{Logical, Rectangle, Size},
};

use super::{
    Material, SceneElement, Shaders,
    switcher::app_icon,
    titlebar::{Label, keep_buffers, label_element},
};
use crate::{
    shell::force_quit::{Entry, layout},
    text::{Style, Text},
};

const LIGHTS: [[u8; 3]; 3] = [[0xFF, 0x5F, 0x57], [0xFE, 0xBC, 0x2E], [0x28, 0xC8, 0x40]];
const RED: [u8; 4] = [0xFF, 0x3B, 0x30, 0xFF];

/// Entries, selected row, dark, accent, scale.
type Key = (Vec<Entry>, Option<usize>, bool, [u8; 3], u64);

#[derive(Default)]
pub struct ForceQuitWindow {
    glass: Option<(Key, Vec<PixelShaderElement>)>,
    labels: Vec<Label>,
}

impl ForceQuitWindow {
    /// Front to back: text, row icons, the selection and button, the list, lights, the sheet, its shadow.
    #[expect(clippy::too_many_arguments, reason = "the frame's drawing context")]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        entries: &[Entry],
        selected: Option<i32>,
        output: Size<i32, Logical>,
        dark: bool,
        accent: [u8; 3],
        scale: f64,
    ) -> Vec<SceneElement> {
        let l = layout(output, entries.len());
        let selected = entries.iter().position(|e| Some(e.pid) == selected).filter(|&i| i < l.rows.len());
        let icon = |row: crate::wm::Rect| {
            let y = row.loc.y + (row.size.h - layout::ICON) / 2;
            Rectangle::new((row.loc.x + 6, y).into(), (layout::ICON, layout::ICON).into())
        };
        let rows = &l.rows;
        let (tiles, letters): (Vec<_>, Vec<_>) = entries.iter().zip(rows).map(|(e, r)| app_icon(shaders, text, &e.name, icon(*r), dark, scale)).unzip();

        let key = (entries.to_vec(), selected, dark, accent, scale.to_bits());
        if self.glass.as_ref().is_none_or(|(k, _)| *k != key) {
            let [ar, ag, ab] = accent.map(|c| f32::from(c) / 255.0);
            let mut glass = tiles;
            if let Some(i) = selected {
                glass.push(shaders.glass_shape(rows[i], [ar, ag, ab, 1.0], 4.0, false, dark, scale));
            }
            glass.push(shaders.glass_shape(l.button, [ar, ag, ab, 1.0], 6.0, false, dark, scale));
            // The list: `fill` at 2×, so it shows on the sheet.
            let fill = if dark { [1.0, 1.0, 1.0, 0.08] } else { [0.0, 0.0, 0.0, 0.05] };
            glass.push(shaders.glass_shape(l.list, fill, 6.0, false, dark, scale));
            for (rect, rgb) in l.lights.iter().zip(LIGHTS) {
                let [r, g, b] = rgb.map(|c| f32::from(c) / 255.0);
                glass.push(shaders.light(*rect, [r, g, b, 1.0], true, scale));
            }
            let spec = Material::Sheet.spec(dark);
            let [r, g, b] = spec.tint.map(|c| f32::from(c) / 255.0);
            // Opaque, like a window: the explanation must stay readable over any app (contrast rule, D2).
            glass.push(shaders.glass_shape(l.rect, [r, g, b, 1.0], layout::RADIUS, true, dark, scale));
            glass.extend(shaders.shadow(l.rect, layout::RADIUS, spec.elevation, dark, scale, false));
            self.glass = Some((key, glass));
        }

        let ink = |alpha: u8| if dark { [255, 255, 255, alpha] } else { [0, 0, 0, alpha] };
        let (label, secondary, white) = (ink(217), ink(if dark { 140 } else { 128 }), [255, 255, 255, 255]);
        let body = Style { size: 13, line_height: 16, weight: 400, color: label, tabular: false };
        let callout = Style { size: 12, line_height: 15, color: secondary, ..body };
        let logical = |px: u32| (f64::from(px) / scale).round() as i32;
        // Left-aligned in `rect` (or right-aligned): shrink the rect to the text.
        let place = |rect: crate::wm::Rect, w: u32, right: bool| {
            let w = logical(w);
            let x = if right { rect.loc.x + rect.size.w - w } else { rect.loc.x };
            Rectangle::new((x, rect.loc.y).into(), (w, rect.size.h).into())
        };

        let mut pieces = vec![text.label("Force Quit Applications", Style { weight: 600, ..body }, layout::WIDTH as u32, scale)];
        let mut rects = vec![l.title];
        for (line, rect) in ["If an app doesn't respond for a while,", "select its name and click Force Quit."].iter().zip(l.lines) {
            let piece = text.label(line, callout, rect.size.w as u32, scale);
            rects.push(place(rect, piece.width, false));
            pieces.push(piece);
        }
        let button = text.label("Force Quit", Style { color: white, ..body }, l.button.size.w as u32, scale);
        rects.push(l.button);
        pieces.push(button);
        for (i, (entry, row)) in entries.iter().zip(rows).enumerate() {
            let on = Some(i) == selected;
            let name_rect = Rectangle::new((row.loc.x + 6 + layout::ICON + 8, row.loc.y).into(), (row.size.w - layout::ICON - 22, row.size.h).into());
            let name = text.label(&entry.name, Style { color: if on { white } else { label }, ..body }, name_rect.size.w as u32, scale);
            rects.push(place(name_rect, name.width, false));
            pieces.push(name);
            if entry.frozen {
                let note = text.label("(not responding)", Style { color: if on { white } else { RED }, ..body }, name_rect.size.w as u32, scale);
                rects.push(place(name_rect, note.width, true));
                pieces.push(note);
            }
        }
        rects.extend(rows.iter().take(letters.len()).map(|r| icon(*r)));
        self.labels = keep_buffers(&self.labels, pieces.into_iter().chain(letters));
        let mut out: Vec<SceneElement> = self.labels.iter().zip(rects).filter_map(|(label, rect)| label_element(renderer, label, rect, scale)).collect();
        if let Some((_, glass)) = &self.glass {
            out.extend(glass.iter().cloned().map(SceneElement::Shader));
        }
        out
    }
}
