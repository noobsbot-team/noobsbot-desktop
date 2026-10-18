//! The dock (E3.1) in Plank's Transparent-Tight look: square, borderless, an opaque rgb(41) → rgb(80) gradient bar
//! behind the lower half of the icons, a small dot under each running app. The same in light and dark (it's a theme).

use smithay::{
    backend::renderer::{
        element::{Kind, memory::MemoryRenderBufferRenderElement},
        gles::{GlesRenderer, element::PixelShaderElement},
    },
    utils::{Point, Rectangle, Size},
};

use std::sync::Arc;

use super::{
    SceneElement, Shaders, grow,
    titlebar::{Label, keep_buffers, label_element},
    ui::Ui,
};
use crate::{
    apps::Icons,
    services::launcher::Entry,
    shell::dock::Layout,
    symbols::Symbols,
    text::{Rendered, Text},
    wm::Rect,
};

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
    /// Badges and progress bars: built for these icons and entries, accent and scale.
    badges: Option<(BadgeKey, Vec<PixelShaderElement>, Vec<Piece>)>,
    badge_labels: Vec<Label>,
}

type BadgeKey = (Vec<(Rect, Option<i64>, Option<u8>)>, [u8; 3], u64);
type Piece = (Arc<Rendered>, Rect);

impl DockView {
    /// Front to back: badge counts, then their pills and the progress bars, over `icons` (D8 badge and progress).
    #[expect(clippy::too_many_arguments, reason = "the frame's drawing context")]
    pub fn badges(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        symbols: &mut Symbols,
        badges: &[(Rect, Entry)],
        accent: [u8; 3],
        scale: f64,
    ) -> Vec<SceneElement> {
        let shown = badges.iter().map(|(r, e)| (*r, e.count, e.progress.map(|p| (p * 100.0).round() as u8))).collect();
        let key = (shown, accent, scale.to_bits());
        if self.badges.as_ref().is_none_or(|(k, _, _)| *k != key) {
            // The dock is a dark theme in light and dark mode, so its controls are drawn dark.
            let mut ui = Ui { shaders, text, symbols, dark: true, accent, glass_opacity: 1.0, scale, glass: Vec::new(), pieces: Vec::new() };
            for (icon, count, percent) in &key.0 {
                if let Some(percent) = percent {
                    // Low across the icon, on a dark backing so the track shows over any icon.
                    let (w, h) = (icon.size.w, icon.size.h);
                    let track = Rectangle::new((icon.loc.x + w / 8, icon.loc.y + h * 4 / 5).into(), (w - w / 4, 4).into());
                    ui.shape(grow(track, 2), [0.0, 0.0, 0.0, 0.55], 4.0);
                    ui.progress(track, *percent);
                }
                if let Some(count) = count {
                    ui.badge(*icon, &count.to_string());
                }
            }
            let (mut glass, pieces) = (ui.glass, ui.pieces);
            glass.reverse();
            self.badges = Some((key, glass, pieces));
        }
        let Some((_, glass, pieces)) = &self.badges else { return Vec::new() };
        self.badge_labels = keep_buffers(&self.badge_labels, pieces.iter().map(|(p, _)| p.clone()));
        let mut out: Vec<SceneElement> = self.badge_labels.iter().zip(pieces).filter_map(|(label, (_, r))| label_element(renderer, label, *r, scale)).collect();
        out.extend(glass.iter().cloned().map(SceneElement::Shader));
        out
    }

    /// The urgent glow at the edge of a hidden dock: new each frame, as it only shows while it pulses.
    pub fn glows(shaders: &Shaders, glows: &[(Rect, [f32; 4])], scale: f64) -> Vec<SceneElement> {
        glows.iter().map(|(rect, colour)| SceneElement::Shader(shaders.light(*rect, *colour, false, scale))).collect()
    }
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
