//! The dock (E3.1) in Plank's Transparent-Tight look: square, borderless, an opaque rgb(41) → rgb(80) gradient bar
//! behind the lower half of the icons, a small dot under each running app. The same in light and dark (it's a theme).

use smithay::{
    backend::renderer::{
        element::{Kind, memory::MemoryRenderBufferRenderElement},
        gles::{GlesRenderer, element::PixelShaderElement},
    },
    utils::{Point, Rectangle, Size},
};

use super::{
    SceneElement, Shaders, grow,
    titlebar::{Label, keep_buffers},
};
use crate::{apps::Icons, shell::dock::Layout, wm::Rect};

/// FillStartColor / FillEndColor.
const TOP: f32 = 41.0 / 255.0;
const BOTTOM: f32 = 80.0 / 255.0;
/// Drawn when an app's icon isn't in any theme.
const FALLBACK_ICON: &str = "application-x-executable";

/// What one icon needs from the dock's items.
pub struct Icon<'a> {
    pub name: &'a str,
    pub running: bool,
    pub focused: bool,
}

/// The dock's pieces, kept between frames so a still dock damages nothing.
#[derive(Default)]
pub struct DockView {
    /// The bar, made at this scale; resized as zoom widens it.
    bar: Option<(u64, PixelShaderElement)>,
    /// Running dots and the focused app's highlight, by their rect.
    marks: Vec<((Rect, bool), PixelShaderElement)>,
    icons: Vec<Label>,
}

impl DockView {
    /// Front to back: icons, dots, the focused app's highlight, the bar. Icons are rasterised once at `px` (the zoomed
    /// size) and scaled down by the GPU when smaller.
    #[expect(clippy::too_many_arguments, reason = "the frame's renderer, shaders and caches, like every shell piece")]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        icons: &mut Icons,
        items: &[Icon],
        layout: &Layout,
        px: u32,
        scale: f64,
    ) -> Vec<SceneElement> {
        let rendered: Vec<_> = items.iter().map(|i| icons.get(i.name, px).or_else(|| icons.get(FALLBACK_ICON, px))).collect();
        self.icons = keep_buffers(&self.icons, rendered.iter().flatten().cloned());
        let mut buffers = self.icons.iter();
        let mut out = Vec::new();
        for (icon, rect) in rendered.iter().zip(&layout.icons) {
            let Some((pixels, Some(buffer))) = icon.as_ref().and_then(|_| buffers.next()) else { continue };
            let loc = rect.loc.to_physical(scale);
            let size = Size::from((rect.size.w.round() as i32, rect.size.h.round() as i32));
            // The whole buffer (its scale is 1, so its logical size is its pixel size), scaled into `size`.
            let src = Rectangle::from_size((f64::from(pixels.width), f64::from(pixels.height)).into());
            match MemoryRenderBufferRenderElement::from_buffer(renderer, loc, buffer, None, Some(src), Some(size), Kind::Unspecified) {
                Ok(element) => out.push(SceneElement::Memory(element)),
                Err(err) => tracing::warn!("dock icon: {err}"),
            }
        }

        // IndicatorSize 5: a small light dot centred under the icon, in the band below it; the focused app's icon sits
        // on a faint highlight (Plank's active-window glow, without its 300 ms fade).
        let dot = (layout.icon / 16.0).round().max(4.0) as i32;
        let floor = f64::from(layout.zone.loc.y + layout.zone.size.h);
        let band = f64::from(layout.zone.size.h) - layout.icon;
        let mut marks = Vec::new();
        for (item, (left, right)) in items.iter().zip(&layout.columns) {
            let centre = ((left + right) / 2.0).round() as i32;
            if item.running {
                let y = (floor - (band + f64::from(dot)) / 2.0).round() as i32;
                marks.push((Rectangle::new((centre - dot / 2, y).into(), (dot, dot).into()), true));
            }
            if item.focused {
                let (x, w) = (left.round() as i32, (right - left).round() as i32);
                marks.push((Rectangle::new(Point::from((x, layout.bar.loc.y)), (w, layout.bar.size.h).into()), false));
            }
        }
        // Dots in front of highlights.
        marks.sort_by_key(|(_, dot)| !dot);
        let old = std::mem::take(&mut self.marks);
        self.marks = marks
            .into_iter()
            .map(|key| {
                let element = old.iter().find(|(k, _)| *k == key).map(|(_, e)| e.clone()).unwrap_or_else(|| match key {
                    (rect, true) => shaders.light(rect, [1.0, 1.0, 1.0, 0.9], false, scale),
                    (rect, false) => shaders.glass_shape(rect, [1.0, 1.0, 1.0, 0.10], 0.0, false, true, scale),
                });
                (key, element)
            })
            .collect();
        out.extend(self.marks.iter().map(|(_, e)| SceneElement::Shader(e.clone())));

        match self.bar.as_mut() {
            Some((s, element)) if *s == scale.to_bits() => element.resize(grow(layout.bar, 1), None),
            _ => self.bar = Some((scale.to_bits(), shaders.gradient(layout.bar, TOP, BOTTOM, scale))),
        }
        out.extend(self.bar.as_ref().map(|(_, e)| SceneElement::Shader(e.clone())));
        out
    }
}
