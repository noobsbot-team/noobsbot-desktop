//! Desktop icons (E7.2): 64 px file icons on the wallpaper with white two-line names over a soft shadow, as on macOS.
//! Selected: a translucent grey square behind the icon and an accent pill behind the name. Renaming: a text box.

use std::sync::Arc;

use smithay::{
    backend::renderer::{
        element::{Kind, memory::MemoryRenderBufferRenderElement},
        gles::{GlesRenderer, element::PixelShaderElement},
    },
    utils::{Point, Rectangle, Size},
};

use super::{
    SceneElement, Shaders,
    titlebar::{Label, keep_buffers},
};
use crate::{
    apps::Icons,
    shell::desktop::{FALLBACK_ICON, Frame, ICON, LABEL_LINE, LABEL_W, icon_rect, label_rect},
    text::{Rendered, Style, Text},
    wm::Rect,
};

const NAME: Style = Style { size: 12, line_height: LABEL_LINE as u16, weight: 500, color: [255, 255, 255, 255], tabular: false };
/// The name's shadow: black, 1 px down.
const SHADOW: [u8; 4] = [0, 0, 0, 140];

/// The shapes (rect, colour, radius), dark, scale.
type Key = (Vec<(Rect, [u8; 4], i32)>, bool, u64);

#[derive(Default)]
pub struct DesktopView {
    glass: Option<(Key, Vec<PixelShaderElement>)>,
    labels: Vec<Label>,
}

impl DesktopView {
    /// Front to back: names, their shadows, icons, then the selection and rename shapes.
    #[expect(clippy::too_many_arguments, reason = "the frame's drawing context")]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        icons: &mut Icons,
        frame: &Frame,
        dark: bool,
        accent: [u8; 3],
        scale: f64,
    ) -> Vec<SceneElement> {
        let logical = |px: u32| (f64::from(px) / scale).round() as i32;
        let px = (f64::from(ICON) * scale).ceil() as u32;
        let [ar, ag, ab] = accent;
        // (piece, rect: filled when `stretch`, else its top-left corner)
        let mut names: Vec<(Arc<Rendered>, Rect, bool)> = Vec::new();
        let mut shadows = Vec::new();
        let mut images = Vec::new();
        // Shapes, front to back: (rect, straight RGBA, radius).
        let mut shapes: Vec<(Rect, [u8; 4], i32)> = Vec::new();
        for (i, (entry, cell)) in frame.icons.iter().enumerate() {
            let selected = frame.selected == Some(i);
            let label = label_rect(*cell);
            if let Some(image) = icons.get(&entry.icon, px).or_else(|| icons.get(FALLBACK_ICON, px)) {
                images.push((image, icon_rect(*cell), true));
            }
            if selected {
                shapes.push((Rectangle::new(icon_rect(*cell).loc - Point::from((4, 4)), (ICON + 8, ICON + 8).into()), [0, 0, 0, 64], 6));
            }
            match frame.renaming.as_ref().filter(|(r, _)| *r == i) {
                // The text box: one line, the name selected or a caret.
                Some((_, field)) => {
                    let ink = if dark { [255, 255, 255, 230] } else { [0, 0, 0, 230] };
                    let style = Style { color: ink, weight: 400, ..NAME };
                    let piece = text.label(&field.text, style, LABEL_W as u32 + 4, scale);
                    let w = logical(piece.width);
                    let boxed = Rectangle::new((label.loc.x - 4, label.loc.y - 1).into(), (LABEL_W + 8, LABEL_LINE + 4).into());
                    let x = boxed.loc.x + ((boxed.size.w - w) / 2).max(2);
                    let y = boxed.loc.y + 2;
                    if field.all {
                        shapes.push((Rectangle::new((x - 1, y).into(), (w + 2, LABEL_LINE).into()), [ar, ag, ab, 110], 2));
                    } else {
                        let before = logical(text.label(&field.text[..field.caret], style, 4000, scale).width).min(w);
                        shapes.push((Rectangle::new((x + before, y).into(), (1, LABEL_LINE).into()), [ar, ag, ab, 255], 0));
                    }
                    shapes.push((boxed, if dark { [30, 30, 30, 255] } else { [255, 255, 255, 255] }, 3));
                    names.push((piece, Rectangle::new((x, y).into(), (w, LABEL_LINE).into()), false));
                }
                None => {
                    let piece = text.centred(&entry.name, NAME, LABEL_W as u32, 2, scale);
                    let at = Rectangle::new(label.loc, (LABEL_W, logical(piece.height)).into());
                    if selected {
                        // Tight around a one-line name, the label's width for two.
                        let w = logical(text.label(&entry.name, NAME, LABEL_W as u32, scale).width).min(LABEL_W) + 8;
                        let pill = Rectangle::new((label.loc.x + (LABEL_W - w) / 2, label.loc.y - 1).into(), (w, logical(piece.height) + 2).into());
                        shapes.push((pill, [ar, ag, ab, 255], 4));
                    } else {
                        let shadow = text.centred(&entry.name, Style { color: SHADOW, ..NAME }, LABEL_W as u32, 2, scale);
                        shadows.push((shadow, Rectangle::new(at.loc + Point::from((0, 1)), at.size), false));
                    }
                    names.push((piece, at, false));
                }
            }
        }

        let key = (shapes.clone(), dark, scale.to_bits());
        if self.glass.as_ref().is_none_or(|(k, _)| *k != key) {
            let glass = shapes
                .iter()
                .map(|(rect, c, radius)| shaders.glass_shape(*rect, c.map(|v| f32::from(v) / 255.0), f64::from(*radius), false, dark, scale))
                .collect();
            self.glass = Some((key, glass));
        }

        let pieces: Vec<_> = names.into_iter().chain(shadows).chain(images).collect();
        self.labels = keep_buffers(&self.labels, pieces.iter().map(|(p, ..)| p.clone()));
        let mut out = Vec::new();
        for ((rendered, buffer), (_, rect, stretch)) in self.labels.iter().zip(&pieces) {
            let Some(buffer) = buffer else { continue };
            let (size, src) = if *stretch {
                (rect.size, Some(Rectangle::from_size((f64::from(rendered.width), f64::from(rendered.height)).into())))
            } else {
                (Size::from((logical(rendered.width), logical(rendered.height))), None)
            };
            let loc = rect.loc.to_f64().to_physical(scale);
            match MemoryRenderBufferRenderElement::from_buffer(renderer, loc, buffer, None, src, Some(size), Kind::Unspecified) {
                Ok(element) => out.push(SceneElement::Memory(element)),
                Err(err) => tracing::warn!("desktop icon: {err}"),
            }
        }
        if let Some((_, glass)) = &self.glass {
            out.extend(glass.iter().cloned().map(SceneElement::Shader));
        }
        out
    }
}
