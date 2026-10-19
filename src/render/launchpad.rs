//! Launchpad (E4.4): the wallpaper dimmed and desaturated (no blur) under a grid of 80 px app icons with Callout
//! names, a `panel` search pill on top and page dots above the dock. On open the grid zooms 1.1 → 1 and fades in.

use std::sync::Arc;

use smithay::{
    backend::renderer::{
        element::{Kind, memory::MemoryRenderBufferRenderElement},
        gles::{GlesRenderer, element::PixelShaderElement},
    },
    utils::{Logical, Point, Rectangle, Size},
};

use super::{
    Material, SceneElement, Shaders,
    titlebar::{Label, keep_buffers},
};
use crate::{
    apps::{App, Icons},
    shell::launchpad::{Grid, pages},
    symbols::{Symbol, Symbols, Weight},
    text::{Rendered, Style, Text},
    ui::field::TextField,
    wm::Rect,
};

/// Drawn when an app's icon isn't in any theme.
const FALLBACK_ICON: &str = "application-x-executable";
/// The dim: a dark grey over the wallpaper at 60%. Mixing with grey takes out 60% of the colour too (desaturates),
/// and keeps white names above 4.5:1 on the brightest wallpaper (D2).
const DIM: [f32; 4] = [0.08, 0.08, 0.08, 0.6];
const WHITE: [u8; 4] = [255, 255, 255, 255];
const CALLOUT: Style = Style { size: 12, line_height: 15, weight: 400, color: WHITE, tabular: false };

/// What one frame shows.
pub struct Frame<'a> {
    pub apps: &'a [&'a App],
    pub grid: &'a Grid,
    pub output: Size<i32, Logical>,
    /// Page position in view (1.5: halfway from the second page to the third).
    pub position: f64,
    pub page: usize,
    /// 0 → 1 as it opens.
    pub shown: f64,
    pub field: &'a TextField,
}

/// Size, pages, page, shown, caret x, selection width, dark, opacity, accent, scale.
type Key = (Size<i32, Logical>, usize, usize, u64, i32, i32, bool, u32, [u8; 3], u64);

#[derive(Default)]
pub struct LaunchpadView {
    glass: Option<(Key, Vec<PixelShaderElement>)>,
    labels: Vec<Label>,
}

/// `rect` scaled by `s` about `centre`.
fn zoom(rect: Rect, centre: Point<f64, Logical>, s: f64) -> Rect {
    let at = |v: f64, c: f64| (c + (v - c) * s).round() as i32;
    let (x, y) = (at(f64::from(rect.loc.x), centre.x), at(f64::from(rect.loc.y), centre.y));
    Rectangle::new((x, y).into(), ((f64::from(rect.size.w) * s).round() as i32, (f64::from(rect.size.h) * s).round() as i32).into())
}

impl LaunchpadView {
    /// Front to back: icons and names, the field's text, caret and pill, the dots, the dim.
    #[expect(clippy::too_many_arguments, reason = "the frame's drawing context")]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        symbols: &mut Symbols,
        icons: &mut Icons,
        frame: &Frame,
        dark: bool,
        accent: [u8; 3],
        glass_opacity: f32,
        scale: f64,
    ) -> Vec<SceneElement> {
        let Frame { apps, grid, output, position, page, shown, field } = *frame;
        let per_page = grid.per_page();
        let count = pages(apps.len(), per_page);
        let logical = |px: u32| (f64::from(px) / scale).round() as i32;
        let alpha = shown.clamp(0.0, 1.0) as f32;
        let centre = Point::from((f64::from(output.w) / 2.0, f64::from(output.h) / 2.0));
        let s = 1.1 - 0.1 * shown;

        // (piece, rect it fills or is centred in, stretch to the rect, alpha)
        let mut pieces: Vec<(Arc<Rendered>, Rect, bool, f32)> = Vec::new();
        let px = (f64::from(grid.icon) * scale).ceil() as u32;
        let first = position.floor().max(0.0) as usize;
        let last = (position.ceil().max(0.0) as usize).min(count - 1);
        for p in first..=last {
            let dx = ((p as f64 - position) * f64::from(output.w)).round() as i32;
            for (i, app) in apps.iter().skip(p * per_page).take(per_page).enumerate() {
                let shift = |r: Rect| zoom(Rectangle::new((r.loc.x + dx, r.loc.y).into(), r.size), centre, s);
                let icon = shift(grid.icon_rect(i));
                if icon.loc.x + icon.size.w < 0 || icon.loc.x > output.w {
                    continue;
                }
                let image = icons.get(&app.icon, px).or_else(|| icons.get(FALLBACK_ICON, px)).unwrap_or_else(|| super::dock::placeholder(px));
                pieces.push((image, icon, true, alpha));
                let name_rect = shift(grid.name_rect(i));
                let name = text.label(&app.name, CALLOUT, grid.name_rect(i).size.w as u32, scale);
                pieces.push((name, name_rect, false, alpha));
            }
        }

        // The search pill: magnifier and "Search" centred while empty, else the magnifier left and the text after it.
        let ink = |a: f32| if dark { [255, 255, 255, (a * 255.0) as u8] } else { [0, 0, 0, (a * 255.0) as u8] };
        let body = Style { size: 13, line_height: 16, weight: 400, color: ink(0.85), tabular: false };
        let f = grid.field;
        let glyph = symbols.get(Symbol::MagnifyingGlass, Weight::Regular, 14, scale, ink(if dark { 0.55 } else { 0.5 }));
        let shown_text = field.display();
        let (words, style) =
            if shown_text.is_empty() { ("Search", Style { color: ink(if dark { 0.25 } else { 0.26 }), ..body }) } else { (shown_text.as_str(), body) };
        let room = f.size.w - 2 * 10 - 14 - 6;
        let label = text.label(words, style, room as u32, scale);
        let w = logical(label.width).min(room);
        let glyph_x = if shown_text.is_empty() { f.loc.x + (f.size.w - 14 - 6 - w) / 2 } else { f.loc.x + 10 };
        let text_x = glyph_x + 14 + 6;
        pieces.push((glyph, Rectangle::new((glyph_x, f.loc.y + (f.size.h - 14) / 2).into(), (14, 14).into()), false, alpha));
        pieces.push((label, Rectangle::new((text_x, f.loc.y).into(), (w, f.size.h).into()), false, alpha));
        let caret_x =
            if shown_text.is_empty() { text_x } else { text_x + logical(text.label(&shown_text[..field.display_caret()], body, 4000, scale).width).min(room) };
        let selected = if field.all { w } else { 0 };

        let key = (output, count, page, shown.to_bits(), caret_x, selected, dark, glass_opacity.to_bits(), accent, scale.to_bits());
        if self.glass.as_ref().is_none_or(|(k, _)| *k != key) {
            let [ar, ag, ab] = accent.map(|c| f32::from(c) / 255.0);
            let mut glass = Vec::new();
            // Caret (the field always has focus), or the Ctrl+A selection.
            if selected > 0 {
                glass.push(shaders.glass_shape(
                    Rectangle::new((text_x - 1, f.loc.y + 5).into(), (selected + 2, f.size.h - 10).into()),
                    [ar, ag, ab, 0.3 * alpha],
                    2.0,
                    false,
                    dark,
                    scale,
                ));
            } else {
                glass.push(shaders.glass_shape(
                    Rectangle::new((caret_x, f.loc.y + 6).into(), (1, f.size.h - 12).into()),
                    [ar, ag, ab, alpha],
                    0.0,
                    false,
                    dark,
                    scale,
                ));
            }
            let spec = Material::Panel.spec(dark);
            let [r, g, b] = spec.tint.map(|c| f32::from(c) / 255.0);
            glass.push(shaders.glass_shape(f, [r, g, b, spec.opacity * glass_opacity * alpha], f64::from(f.size.h) / 2.0, true, dark, scale));
            if count > 1 {
                for (i, dot) in grid.dots(count).into_iter().enumerate() {
                    let a = if i == page { 0.9 } else { 0.35 };
                    glass.push(shaders.light(dot, [1.0, 1.0, 1.0, a * alpha], false, scale));
                }
            }
            let [r, g, b, a] = DIM;
            glass.push(shaders.glass_shape(Rectangle::from_size(output), [r, g, b, a * alpha], 0.0, false, dark, scale));
            self.glass = Some((key, glass));
        }

        self.labels = keep_buffers(&self.labels, pieces.iter().map(|(p, ..)| p.clone()));
        let mut out = Vec::new();
        for ((rendered, buffer), (_, rect, stretch, a)) in self.labels.iter().zip(&pieces) {
            let Some(buffer) = buffer else { continue };
            let (size, src) = if *stretch {
                (rect.size, Some(Rectangle::from_size((f64::from(rendered.width), f64::from(rendered.height)).into())))
            } else {
                (Size::from((logical(rendered.width), logical(rendered.height))), None)
            };
            let loc = Point::from((rect.loc.x + (rect.size.w - size.w) / 2, rect.loc.y + (rect.size.h - size.h) / 2)).to_f64().to_physical(scale);
            match MemoryRenderBufferRenderElement::from_buffer(renderer, loc, buffer, Some(*a), src, Some(size), Kind::Unspecified) {
                Ok(element) => out.push(SceneElement::Memory(element)),
                Err(err) => tracing::warn!("launchpad: {err}"),
            }
        }
        if let Some((_, glass)) = &self.glass {
            out.extend(glass.iter().cloned().map(SceneElement::Shader));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zooms_about_the_centre() {
        let r = Rectangle::new((100, 100).into(), (80, 80).into());
        assert_eq!(zoom(r, (0.0, 0.0).into(), 1.0), r);
        assert_eq!(zoom(r, (0.0, 0.0).into(), 1.1), Rectangle::new((110, 110).into(), (88, 88).into()));
        let c = Rectangle::new((-40, -40).into(), (80, 80).into());
        assert_eq!(zoom(c, (0.0, 0.0).into(), 1.1), Rectangle::new((-44, -44).into(), (88, 88).into()), "a centred rect stays centred");
    }
}
