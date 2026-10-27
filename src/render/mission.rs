//! Mission Control (E5.2), a Space sliding in and the Space indicator (E5.1). Windows are drawn from the textures
//! the apps already gave us, scaled and moved (`transformed`); nothing is copied, and nothing is drawn while closed.

use smithay::{
    backend::renderer::gles::{GlesRenderer, element::PixelShaderElement},
    desktop::{Space, Window},
    utils::{Logical, Point, Rectangle, Size},
};

use super::{
    Material, SceneElement, Shaders, WINDOW_RADIUS,
    titlebar::{Label, keep_buffers, label_element},
    transformed, window_elements,
};
use crate::{
    mission::{NAME_H, Overview, close_button},
    text::{Style, Text, ts},
    wm::Rect,
};

/// One rounded shape: rect, fill, radius, glass effects, ring width (0: filled).
pub(super) type Shape = (Rect, [f32; 4], f64, bool, i32);

#[derive(Default)]
pub struct MissionView {
    /// The wallpaper dimming, by its alpha.
    dim: Option<(u32, PixelShaderElement)>,
    /// In front of the other Spaces' windows, and behind them; kept while their shapes are the same.
    front: Option<(Vec<Shape>, bool, u64, Vec<PixelShaderElement>)>,
    back: Option<(Vec<Shape>, bool, u64, Vec<PixelShaderElement>)>,
    labels: Vec<Label>,
    hud: Option<(Vec<Shape>, bool, u64, Vec<PixelShaderElement>)>,
    hud_shadow: Option<((Rect, bool, u64), Option<PixelShaderElement>)>,
}

/// The cached elements for `shapes`, rebuilt only when they change.
pub(super) fn shapes<'a>(
    cache: &'a mut Option<(Vec<Shape>, bool, u64, Vec<PixelShaderElement>)>,
    shaders: &Shaders,
    shapes: Vec<Shape>,
    dark: bool,
    scale: f64,
) -> &'a [PixelShaderElement] {
    if cache.as_ref().is_none_or(|(s, d, sc, _)| *s != shapes || *d != dark || *sc != scale.to_bits()) {
        let elements = shapes
            .iter()
            .map(
                |&(rect, fill, radius, effects, ring)| {
                    if ring > 0 { shaders.ring(rect, radius, ring, fill, scale) } else { shaders.glass_shape(rect, fill, radius, effects, dark, scale) }
                },
            )
            .collect();
        *cache = Some((shapes, dark, scale.to_bits(), elements));
    }
    cache.as_ref().map_or(&[], |(_, _, _, e)| e)
}

pub(super) fn rgba([r, g, b]: [u8; 3], a: f32) -> [f32; 4] {
    [f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0, a]
}

pub(super) fn material(material: Material, dark: bool, glass_opacity: f32) -> [f32; 4] {
    let spec = material.spec(dark);
    rgba(spec.tint, spec.opacity * glass_opacity)
}

impl MissionView {
    /// Mission Control's part of the frame, front to back: the Spaces bar's labels and buttons, the other Spaces'
    /// windows in their thumbnails, the hover outline and title, the windows, the bar's cells, the dimmed wallpaper.
    #[expect(clippy::too_many_arguments, reason = "the frame's drawing context")]
    pub fn windows(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        space: &Space<Window>,
        overview: &Overview,
        output: Rect,
        dark: bool,
        accent: [u8; 3],
        glass_opacity: f32,
        scale: f64,
    ) -> Vec<SceneElement> {
        let p = overview.progress as f32;
        let phys = |p: Point<i32, Logical>| p.to_f64().to_physical(scale).to_i32_round();
        let label_rgb = if dark { [255, 255, 255, 217] } else { [0, 0, 0, 217] };
        let callout = Style { size: 12, line_height: 15, weight: 400, color: label_rgb, tabular: false };
        let bar = &overview.bar;
        let removable = bar.cells.len() > 1 && bar.plus.is_some();

        // Labels: Space names (in the pills, or under the thumbnails), the hovered window's title, ✕ and +.
        let mut pieces = Vec::new();
        let mut rects = Vec::new();
        for (cell, name) in bar.cells.iter().zip(&overview.names) {
            let rect = if bar.expanded { Rectangle::new((cell.loc.x, cell.loc.y + cell.size.h + 4).into(), (cell.size.w, ts(NAME_H)).into()) } else { *cell };
            pieces.push(text.label(name, callout, (rect.size.w - 8).max(0) as u32, scale));
            rects.push(rect);
        }
        let title = overview.hover.as_ref().map(|(i, title)| {
            let thumb = overview.thumbs[*i].to;
            let label = text.label(title, Style { size: 13, line_height: 16, ..callout }, (thumb.size.w.max(120) - 24) as u32, scale);
            let w = (f64::from(label.width) / scale).round() as i32 + ts(24);
            let pill = Rectangle::new((thumb.loc.x + (thumb.size.w - w) / 2, thumb.loc.y + thumb.size.h + 8).into(), (w, ts(24)).into());
            pieces.push(label);
            rects.push(pill);
            (thumb, pill)
        });
        let close = overview.bar_hover.filter(|_| removable).and_then(|i| bar.cells.get(i)).map(|c| close_button(*c));
        let glyph = Style { size: 15, line_height: 18, weight: 500, color: label_rgb, tabular: false };
        for (rect, symbol) in close.iter().map(|r| (*r, "×")).chain(bar.plus.map(|r| (r, "+"))) {
            pieces.push(text.label(symbol, glyph, rect.size.w as u32, scale));
            rects.push(rect);
        }
        self.labels = keep_buffers(&self.labels, pieces);
        let mut out: Vec<SceneElement> = self.labels.iter().zip(rects).filter_map(|(l, r)| label_element(renderer, l, r, scale)).collect();

        let mut front: Vec<Shape> = Vec::new();
        let hud = material(Material::Hud, dark, glass_opacity);
        if let Some(rect) = close {
            front.push((rect, hud, f64::from(rect.size.w) / 2.0, true, 0));
        }
        if let Some(rect) = bar.plus {
            front.push((rect, hud, f64::from(rect.size.w.min(rect.size.h)) / 2.0, true, 0));
        }
        // The every-Space button: four small squares in a circle.
        let a = bar.all;
        let (sq, gap) = (a.size.w / 6, a.size.w / 12);
        let start = a.loc + Point::from(((a.size.w - 2 * sq - gap) / 2, (a.size.h - 2 * sq - gap) / 2));
        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let [r, g, b, al] = label_rgb;
            let fill = [f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0, f32::from(al) / 255.0];
            front.push((Rectangle::new(start + Point::from((dx * (sq + gap), dy * (sq + gap))), (sq, sq).into()), fill, 1.5, false, 0));
        }
        front.push((a, hud, f64::from(a.size.w) / 2.0, true, 0));
        if let Some((thumb, pill)) = title {
            front.push((thumb, rgba(accent, 1.0), WINDOW_RADIUS, false, 3));
            front.push((pill, hud, f64::from(pill.size.h) / 2.0, true, 0));
        }
        out.extend(shapes(&mut self.front, shaders, front, dark, scale).iter().cloned().map(SceneElement::Shader));

        // The other Spaces' windows, shrunk into their thumbnails.
        for (desk, window, origin) in overview.others.iter().rev() {
            let Some(cell) = bar.cells.get(*desk) else { continue };
            let factor = f64::from(cell.size.w) / f64::from(output.size.w.max(1));
            let elements = window_elements(renderer, shaders, text, space, window, *origin, scale, dark);
            out.extend(elements.into_iter().filter_map(|e| transformed(e, (0, 0).into(), factor, phys(cell.loc))));
        }

        // The windows, from where they are to their place in the grid (over the bar, so a dragged one and the
        // every-Space view's stay in sight).
        for thumb in &overview.thumbs {
            if thumb.from.size.w <= 0 {
                continue;
            }
            let factor = f64::from(thumb.to.size.w) / f64::from(thumb.from.size.w);
            let origin = phys(thumb.from.loc);
            let elements = window_elements(renderer, shaders, text, space, &thumb.window, output.loc, scale, dark);
            let moved = elements.into_iter().filter_map(|e| transformed(e, origin, factor, phys(thumb.to.loc) - origin)).collect();
            out.extend(faded(renderer, moved, thumb.alpha, scale));
        }
        // Reduce motion: behind them, the windows where they are, fading out.
        for thumb in overview.thumbs.iter().filter(|t| t.alpha < 1.0) {
            let elements = window_elements(renderer, shaders, text, space, &thumb.window, output.loc, scale, dark);
            out.extend(faded(renderer, elements, 1.0 - thumb.alpha, scale));
        }

        let mut back: Vec<Shape> = Vec::new();
        let radius = if bar.expanded { 8.0 } else { 12.0 };
        for (i, cell) in bar.cells.iter().enumerate() {
            if i == overview.current {
                back.push((*cell, rgba(accent, 1.0), radius, false, 2));
            }
            let fill =
                if overview.bar_hover == Some(i) { material(Material::Sheet, dark, glass_opacity) } else { material(Material::Card, dark, glass_opacity) };
            back.push((*cell, fill, radius, true, 0));
        }
        out.extend(shapes(&mut self.back, shaders, back, dark, scale).iter().cloned().map(SceneElement::Shader));

        // The wallpaper, dimmed 30%.
        let alpha = 0.3 * p;
        if self.dim.as_ref().is_none_or(|(a, _)| *a != alpha.to_bits()) {
            let area = Rectangle::from_size(output.size);
            self.dim = Some((alpha.to_bits(), shaders.glass_shape(area, [0.0, 0.0, 0.0, alpha], 0.0, false, dark, scale)));
        }
        out.extend(self.dim.as_ref().map(|(_, e)| SceneElement::Shader(e.clone())));
        out
    }

    /// The Space indicator (E5.1): a `hud` pill at the top centre with a dot per Space, the shown one in `label`, at
    /// `alpha` as it fades out.
    #[expect(clippy::too_many_arguments, reason = "the frame's drawing context")]
    pub fn indicator(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        (count, current, alpha): (usize, usize, f64),
        width: i32,
        dark: bool,
        glass_opacity: f32,
        scale: f64,
    ) -> Vec<SceneElement> {
        const DOT: i32 = 8;
        const STEP: i32 = 16;
        let n = count as i32;
        let size = Size::from((n * STEP + 12, 24));
        let pill = Rectangle::new(((width - size.w) / 2, super::menubar::height() + 8).into(), size);
        let label = if dark { [1.0, 1.0, 1.0, 0.85] } else { [0.0, 0.0, 0.0, 0.85] };
        let mut list: Vec<Shape> = (0..n)
            .map(|i| {
                let rect = Rectangle::new((pill.loc.x + 6 + i * STEP + (STEP - DOT) / 2, pill.loc.y + (size.h - DOT) / 2).into(), (DOT, DOT).into());
                let [r, g, b, a] = label;
                (rect, [r, g, b, if i as usize == current { a } else { a * 0.3 }], f64::from(DOT) / 2.0, false, 0)
            })
            .collect();
        list.push((pill, material(Material::Hud, dark, glass_opacity), 12.0, true, 0));
        let mut out: Vec<SceneElement> = shapes(&mut self.hud, shaders, list, dark, scale).iter().cloned().map(SceneElement::Shader).collect();
        let key = (pill, dark, scale.to_bits());
        if self.hud_shadow.as_ref().is_none_or(|(k, _)| *k != key) {
            self.hud_shadow = Some((key, shaders.shadow(pill, 12.0, Material::Hud.spec(dark).elevation, dark, scale, false)));
        }
        out.extend(self.hud_shadow.as_ref().and_then(|(_, s)| s.clone()).map(SceneElement::Shader));
        faded(renderer, out, alpha, scale)
    }
}

/// A Space sliding in: each window drawn against its own output origin, so the old Space slides out beside it;
/// one with alpha under 1 (the reduce-motion crossfade) is drawn as a snapshot at that alpha.
pub fn sliding(
    renderer: &mut GlesRenderer,
    shaders: &Shaders,
    text: &mut Text,
    space: &Space<Window>,
    windows: &[crate::spaces::SlideWindow],
    scale: f64,
    dark: bool,
) -> Vec<SceneElement> {
    let mut out = Vec::new();
    for (window, origin, alpha) in windows {
        let elements = window_elements(renderer, shaders, text, space, window, *origin, scale, dark);
        out.extend(faded(renderer, elements, *alpha, scale));
    }
    out
}

/// `elements` at `alpha`: as they are when opaque, else as one snapshot (`motion::transformed`).
fn faded(renderer: &mut GlesRenderer, elements: Vec<SceneElement>, alpha: f64, scale: f64) -> Vec<SceneElement> {
    if alpha >= 1.0 {
        return elements;
    }
    if alpha <= 0.0 {
        return Vec::new();
    }
    super::motion::transformed(renderer, elements, scale, |rect| crate::anim::Look { rect, alpha })
}
