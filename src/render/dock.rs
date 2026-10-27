//! The dock (E3.1) in Plank's Transparent-Tight look: square, borderless, an opaque rgb(41) → rgb(80) gradient bar
//! behind the lower half of the icons, a small dot under each running app. The same in light and dark (it's a theme).
//! `[dock] style` swaps the bar for the `whitesur-dark` or `glass` preset (`Look`).

use smithay::{
    backend::renderer::{
        element::{Kind, memory::MemoryRenderBufferRenderElement},
        gles::{GlesRenderer, element::PixelShaderElement},
    },
    desktop::{Space, Window},
    utils::{Logical, Point, Rectangle, Size},
};

use std::{collections::HashMap, sync::Arc};

use super::{
    Material, SceneElement, Shaders, grow,
    mission::{Shape, material, rgba, shapes},
    titlebar::{Label, keep_buffers, label_element},
    transformed,
    ui::Ui,
    window_elements,
};
use crate::{
    apps::Icons,
    config::DockStyle,
    mission::close_button,
    services::launcher::Entry,
    shell::{
        dock::{Layout, Peek},
        dock_fx::Shade,
    },
    symbols::Symbols,
    text::{Rendered, Style, Text},
    wm::Rect,
};

/// A `[dock] style`: the bar's grey gradient, top to bottom (0–1, `bottom` < 1), its opacity, corner radius and a
/// 1 px light edge inside it (white alpha). No blur: the renderer has no blur pass (D3 glass is tint only).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Look {
    top: f32,
    bottom: f32,
    opacity: f32,
    radius: f64,
    edge: f32,
}

impl Look {
    fn of(style: DockStyle) -> Look {
        match style {
            // FillStartColor / FillEndColor, alpha 255, square, LineWidth 0.
            DockStyle::Default => Look { top: 41.0 / 255.0, bottom: 80.0 / 255.0, opacity: 1.0, radius: 0.0, edge: 0.0 },
            DockStyle::WhitesurDark => Look { top: 30.0 / 255.0, bottom: 30.0 / 255.0, opacity: 0.85, radius: 0.0, edge: 0.10 },
            // The D5 dock radius for glass.
            DockStyle::Glass => Look { top: 96.0 / 255.0, bottom: 72.0 / 255.0, opacity: 0.45, radius: 18.0, edge: 0.25 },
        }
    }

    /// The bar over `rect`. The glass shader's sheen paints white at strength `s` fading downwards; a negative `s`
    /// darkens instead: over `bottom` it gives `bottom + s (bottom - 1)` at the top edge (opaque), which is `top` for
    /// the `s` below.
    fn bar(&self, shaders: &Shaders, rect: Rect, scale: f64) -> PixelShaderElement {
        let sheen = (self.top - self.bottom) / (1.0 - self.bottom);
        let radius = self.radius.min(f64::from(rect.size.h) / 2.0);
        shaders.glass_element(rect, [self.bottom, self.bottom, self.bottom, self.opacity], radius, [sheen, self.edge, 0.0, 0.0], 0.0, [0.0; 4], scale)
    }
}
/// Drawn when an app's icon isn't in any theme.
const FALLBACK_ICON: &str = "application-x-executable";

/// A grey rounded square, `px` wide, for when the theme has no `FALLBACK_ICON` either: an app is never iconless.
pub fn placeholder(px: u32) -> Arc<Rendered> {
    // ponytail: a few sizes ever get asked for (one per zoom level and scale), so a list stands in for a map.
    static CACHE: std::sync::Mutex<Vec<(u32, Arc<Rendered>)>> = std::sync::Mutex::new(Vec::new());
    let mut cache = CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, r)) = cache.iter().find(|(p, _)| *p == px) {
        return r.clone();
    }
    let (size, radius) = (px as f64, px as f64 * 0.22);
    let mut pixels = Vec::with_capacity((px * px * 4) as usize);
    for y in 0..px {
        for x in 0..px {
            let (cx, cy) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
            // Distance past the corner circle's centre, 0 along the edges.
            let dx = (radius - cx).max(cx - (size - radius)).max(0.0);
            let dy = (radius - cy).max(cy - (size - radius)).max(0.0);
            let a = ((radius + 0.5 - dx.hypot(dy)).clamp(0.0, 1.0) * 255.0).round() as u8;
            pixels.extend([(u16::from(a) * 128 / 255) as u8, (u16::from(a) * 128 / 255) as u8, (u16::from(a) * 128 / 255) as u8, a]);
        }
    }
    let r = Arc::new(Rendered { pixels, width: px, height: px, baseline: px, truncated: false });
    cache.push((px, r.clone()));
    r
}

/// What one icon needs from the dock's items.
pub struct Icon<'a> {
    pub name: &'a str,
    /// Open windows: one dot, two for several (Plank).
    pub windows: usize,
    pub shade: Shade,
}

/// The dock's pieces, kept between frames so a still dock damages nothing.
#[derive(Default)]
pub struct DockView {
    /// The bar, made at this scale; resized as zoom widens it.
    bar: Option<(u64, PixelShaderElement)>,
    /// Running dots, by their rect.
    marks: Vec<(Rect, PixelShaderElement)>,
    icons: Vec<Label>,
    /// The icons' shadows, for those that have one yet.
    shadows: Vec<Label>,
    /// Badges and progress bars: built for these icons and entries, accent and scale.
    badges: Option<(BadgeKey, Vec<PixelShaderElement>, Vec<Piece>)>,
    badge_labels: Vec<Label>,
    /// Lightened or darkened copies of icons, by icon, size and shade in 1/20 steps.
    shaded: HashMap<(usize, u32, u8, u8), Arc<Rendered>>,
    /// The tooltips, made for these words and bottom centres, mode and scale.
    tip: Option<(TipKey, Vec<PixelShaderElement>, Vec<Piece>)>,
    tip_labels: Vec<Label>,
    /// The window previews' glass, ✕ label, shadow and the dim behind the hovered window.
    peek: Option<(Vec<Shape>, bool, u64, Vec<PixelShaderElement>)>,
    peek_labels: Vec<Label>,
    peek_shadow: Option<((Rect, bool, u64), Option<PixelShaderElement>)>,
    peek_dim: Option<((Rect, bool, u64), PixelShaderElement)>,
    /// The line before the minimised windows' tiles.
    separator: Option<(Rect, PixelShaderElement)>,
    /// The bar and highlights are made in this style.
    style: DockStyle,
}

/// The last `u32` is the text scale, so a showing tooltip or badge resizes when it changes.
type TipKey = (Vec<(String, Point<i32, Logical>)>, bool, u64, u32);

type BadgeKey = (Vec<(Rect, Option<i64>, Option<u8>)>, [u8; 3], u64, u32);
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
        let key = (shown, accent, scale.to_bits(), crate::text::factor().to_bits());
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

    /// The hovered app's name (D8 tooltip) and the Super+n number pills, each centred over its point (bottom edge).
    #[expect(clippy::too_many_arguments, reason = "the frame's drawing context")]
    pub fn tooltip(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        symbols: &mut Symbols,
        tips: &[(String, Point<i32, Logical>)],
        dark: bool,
        scale: f64,
    ) -> Vec<SceneElement> {
        if tips.is_empty() {
            (self.tip, self.tip_labels) = (None, Vec::new());
            return Vec::new();
        }
        let key = (tips.to_vec(), dark, scale.to_bits(), crate::text::factor().to_bits());
        if self.tip.as_ref().is_none_or(|(k, _, _)| *k != key) {
            let mut ui = Ui { shaders, text, symbols, dark, accent: [0, 0, 0], glass_opacity: 1.0, scale, glass: Vec::new(), pieces: Vec::new() };
            for (words, at) in tips {
                let (w, h) = ui.tooltip_size(words);
                ui.tooltip((at.x - w / 2, at.y - h).into(), words);
            }
            let (mut glass, pieces) = (ui.glass, ui.pieces);
            glass.reverse();
            self.tip = Some((key, glass, pieces));
        }
        let Some((_, glass, pieces)) = &self.tip else { return Vec::new() };
        self.tip_labels = keep_buffers(&self.tip_labels, pieces.iter().map(|(p, _)| p.clone()));
        let mut out: Vec<SceneElement> = self.tip_labels.iter().zip(pieces).filter_map(|(label, (_, r))| label_element(renderer, label, *r, scale)).collect();
        out.extend(glass.iter().cloned().map(SceneElement::Shader));
        out
    }

    /// The window previews (E8.3), front to back: the hovered thumbnail's ✕ and accent ring, the live thumbnails
    /// (Mission Control's scaled windows), the `hud` strip and its shadow. Second, to go behind the dock: the hovered
    /// window again over a dim, which fades the others (when it is on the shown Space).
    #[expect(clippy::too_many_arguments, reason = "the frame's drawing context")]
    pub fn peek(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        space: &Space<Window>,
        peek: Option<&(Peek, Vec<(Window, Rect)>)>,
        hover: Option<usize>,
        output: Rect,
        dark: bool,
        accent: [u8; 3],
        glass_opacity: f32,
        scale: f64,
    ) -> (Vec<SceneElement>, Vec<SceneElement>) {
        let Some((peek, windows)) = peek else {
            self.peek_labels = Vec::new();
            return (Vec::new(), Vec::new());
        };
        let hover = hover.filter(|i| *i < windows.len().min(peek.thumbs.len()));
        let phys = |p: Point<i32, Logical>| p.to_f64().to_physical(scale).to_i32_round();
        let close = hover.map(|i| close_button(peek.thumbs[i]));
        let label_rgb = if dark { [255, 255, 255, 217] } else { [0, 0, 0, 217] };
        let glyph = Style { size: 15, line_height: 18, weight: 500, color: label_rgb, tabular: false };
        let pieces: Vec<_> = close.iter().map(|r| text.label("×", glyph, r.size.w as u32, scale)).collect();
        self.peek_labels = keep_buffers(&self.peek_labels, pieces);
        let mut out: Vec<SceneElement> = self.peek_labels.iter().zip(close).filter_map(|(l, r)| label_element(renderer, l, r, scale)).collect();
        let mut front: Vec<Shape> = Vec::new();
        let hud = material(Material::Hud, dark, glass_opacity);
        if let Some(rect) = close {
            front.push((rect, hud, f64::from(rect.size.w) / 2.0, true, 0));
        }
        if let Some(i) = hover {
            front.push((grow(peek.thumbs[i], 3), rgba(accent, 1.0), 6.0, false, 3));
        }
        let strip_at = front.len();
        front.push((peek.strip, hud, Material::Hud.spec(dark).radius, true, 0));
        let glass = shapes(&mut self.peek, shaders, front, dark, scale);
        out.extend(glass[..strip_at].iter().cloned().map(SceneElement::Shader));
        for ((window, from), to) in windows.iter().zip(&peek.thumbs) {
            let factor = f64::from(to.size.w) / f64::from(from.size.w.max(1));
            let origin = phys(from.loc);
            let elements = window_elements(renderer, shaders, text, space, window, output.loc, scale, dark);
            out.extend(elements.into_iter().filter_map(|e| transformed(e, origin, factor, phys(to.loc) - origin)));
        }
        out.extend(glass[strip_at..].iter().cloned().map(SceneElement::Shader));
        let key = (peek.strip, dark, scale.to_bits());
        if self.peek_shadow.as_ref().is_none_or(|(k, _)| *k != key) {
            self.peek_shadow = Some((key, shaders.shadow(peek.strip, Material::Hud.spec(dark).radius, Material::Hud.spec(dark).elevation, dark, scale, false)));
        }
        out.extend(self.peek_shadow.as_ref().and_then(|(_, s)| s.clone()).map(SceneElement::Shader));

        let screen = Rectangle::from_size(output.size);
        let Some((window, _)) = hover.map(|i| &windows[i]).filter(|(_, frame)| frame.overlaps(screen)) else { return (out, Vec::new()) };
        let mut behind = window_elements(renderer, shaders, text, space, window, output.loc, scale, dark);
        let key = (screen, dark, scale.to_bits());
        if self.peek_dim.as_ref().is_none_or(|(k, _)| *k != key) {
            self.peek_dim = Some((key, shaders.glass_shape(screen, [0.0, 0.0, 0.0, 0.35], 0.0, false, dark, scale)));
        }
        behind.extend(self.peek_dim.as_ref().map(|(_, e)| SceneElement::Shader(e.clone())));
        (out, behind)
    }

    /// An icon dragged off the dock vanishing: a ring of soft puffs spreading and fading where it was let go
    /// (drawn, not Plank's poof sprite). New each frame, as it only lasts 300 ms.
    pub fn poofs(shaders: &Shaders, poofs: &[(Point<f64, Logical>, f64)], icon: f64, scale: f64) -> Vec<SceneElement> {
        let mut out = Vec::new();
        for (at, t) in poofs {
            let spread = icon * (0.15 + 0.35 * t);
            let size = (icon * (0.35 + 0.25 * t)).round() as i32;
            let alpha = (0.8 * (1.0 - t)) as f32;
            for k in 0..6 {
                let angle = std::f64::consts::TAU * f64::from(k) / 6.0;
                let (x, y) = ((at.x + spread * angle.cos()) as i32, (at.y + spread * angle.sin()) as i32);
                let rect = Rectangle::new((x - size / 2, y - size / 2).into(), (size, size).into());
                out.push(SceneElement::Shader(shaders.light(rect, [0.92, 0.92, 0.92, alpha], false, scale)));
            }
        }
        out
    }
}

impl DockView {
    /// Front to back: icons, their shadows, dots, the bar. Icons are rasterised once at
    /// `px` (the zoomed size) and scaled down by the GPU when smaller; so are their shadows, blurred to match.
    #[expect(clippy::too_many_arguments, reason = "the frame's renderer, shaders and caches, like every shell piece")]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        icons: &mut Icons,
        items: &[Icon],
        layout: &Layout,
        px: u32,
        style: DockStyle,
        scale: f64,
    ) -> Vec<SceneElement> {
        if self.style != style {
            (self.style, self.bar) = (style, None);
        }
        let look = Look::of(style);
        // The small shadow under each icon (Plank's IconShadowSize 1): D6 level 1, dark like the dock, in the zoomed
        // bitmap's px. Its σ is blur / 2 at rest; three box passes of radius r give σ ≈ r + ½.
        let d6 = super::Shadow::of(1, true).expect("D6 level 1 casts a shadow");
        let r = (f64::from(d6.blur) / 2.0 * f64::from(px) / layout.icon - 0.5).round().max(1.0) as u32;
        let mut shadows = Vec::with_capacity(items.len());
        let plain: Vec<_> = items
            .iter()
            .map(|i| {
                let found = [i.name, FALLBACK_ICON].into_iter().find_map(|name| Some((name, icons.get(name, px)?)));
                shadows.push(found.as_ref().and_then(|(name, _)| icons.shadow(name, px, r)));
                found.map_or_else(|| placeholder(px), |(_, icon)| icon)
            })
            .collect();
        // Hovered icons lighten, clicked ones darken (Plank's lighten and darken effects), from cached copies.
        let old = std::mem::take(&mut self.shaded);
        let rendered: Vec<_> = plain
            .into_iter()
            .zip(items)
            .map(|(icon, item)| {
                let steps = |v: f32| (v.clamp(0.0, 1.0) * 20.0).round() as u8;
                let (l, d) = (steps(item.shade.lighten), steps(item.shade.darken));
                if (l, d) == (0, 0) {
                    return icon;
                }
                let key = (Arc::as_ptr(&icon) as usize, px, l, d);
                let copy = old.get(&key).cloned().unwrap_or_else(|| Arc::new(shade(&icon, f32::from(l) / 20.0, f32::from(d) / 20.0)));
                self.shaded.insert(key, copy.clone());
                copy
            })
            .collect();
        self.icons = keep_buffers(&self.icons, rendered.iter().cloned());
        self.shadows = keep_buffers(&self.shadows, shadows.iter().flatten().cloned());
        let (mut buffers, mut shadow_buffers) = (self.icons.iter(), self.shadows.iter());
        let (mut out, mut behind) = (Vec::new(), Vec::new());
        for ((rect, item), shadow) in layout.icons.iter().zip(items).zip(&shadows) {
            let shadow = shadow.as_ref().and_then(|_| shadow_buffers.next());
            let Some((pixels, Some(buffer))) = buffers.next() else { continue };
            if item.shade.alpha <= 0.0 {
                continue;
            }
            let alpha = (item.shade.alpha < 1.0).then_some(item.shade.alpha);
            let loc = rect.loc.to_physical(scale);
            let size = Size::from((rect.size.w.round() as i32, rect.size.h.round() as i32));
            // The whole buffer (its scale is 1, so its logical size is its pixel size), scaled into `size`.
            let src = Rectangle::from_size((f64::from(pixels.width), f64::from(pixels.height)).into());
            match MemoryRenderBufferRenderElement::from_buffer(renderer, loc, buffer, alpha, Some(src), Some(size), Kind::Unspecified) {
                Ok(element) => out.push(SceneElement::Memory(element)),
                Err(err) => tracing::warn!("dock icon: {err}"),
            }
            // `rect` holds the shadow's middle, inside the 3r it blurs out to; it drops by the offset, zoomed with the icon.
            let Some((s, Some(buffer))) = shadow else { continue };
            let (pad, (w, h)) = (f64::from(3 * r), (f64::from(s.width), f64::from(s.height)));
            let (kx, ky) = (rect.size.w / (w - 2.0 * pad), rect.size.h / (h - 2.0 * pad));
            let drop = f64::from(d6.offset_y) * rect.size.h / layout.icon;
            let loc = Point::<f64, Logical>::from((rect.loc.x - pad * kx, rect.loc.y - pad * ky + drop)).to_physical(scale);
            let size = Size::from(((w * kx).round() as i32, (h * ky).round() as i32));
            let src = Rectangle::from_size((w, h).into());
            let alpha = Some(d6.opacity * item.shade.alpha);
            match MemoryRenderBufferRenderElement::from_buffer(renderer, loc, buffer, alpha, Some(src), Some(size), Kind::Unspecified) {
                Ok(element) => behind.push(SceneElement::Memory(element)),
                Err(err) => tracing::warn!("dock icon shadow: {err}"),
            }
        }
        out.extend(behind);

        // IndicatorSize 5: a small light dot centred under the icon, in the band below it, two (s/16 either side) for
        // several windows.
        let dot = (layout.icon / 16.0).round().max(4.0) as i32;
        let floor = f64::from(layout.zone.loc.y + layout.zone.size.h);
        let band = f64::from(layout.zone.size.h) - layout.icon;
        let apart = (layout.icon / 16.0).round() as i32;
        let mut marks = Vec::new();
        for (item, (left, right)) in items.iter().zip(&layout.columns) {
            let centre = ((left + right) / 2.0).round() as i32;
            let y = (floor - (band + f64::from(dot)) / 2.0).round() as i32;
            let offsets: &[i32] = match item.windows {
                0 => &[],
                1 => &[0],
                _ => &[-apart, apart],
            };
            for dx in offsets {
                marks.push(Rectangle::new((centre + dx - dot / 2, y).into(), (dot, dot).into()));
            }
        }
        let old = std::mem::take(&mut self.marks);
        self.marks = marks
            .into_iter()
            .map(|rect| {
                let element = old.iter().find(|(r, _)| *r == rect).map(|(_, e)| e.clone());
                (rect, element.unwrap_or_else(|| shaders.light(rect, [1.0, 1.0, 1.0, 0.9], false, scale)))
            })
            .collect();
        out.extend(self.marks.iter().map(|(_, e)| SceneElement::Shader(e.clone())));
        // A thin line down the bar, a sixth of it clear at either end.
        let line = layout.separator.map(|x| {
            let (top, h) = (layout.bar.loc.y, layout.bar.size.h);
            Rectangle::new(((x - 0.5).round() as i32, top + h / 6).into(), (1, h - h / 3).into())
        });
        if self.separator.as_ref().map(|(r, _)| *r) != line {
            self.separator = line.map(|rect| (rect, shaders.light(rect, [1.0, 1.0, 1.0, 0.35], false, scale)));
        }
        out.extend(self.separator.as_ref().map(|(_, e)| SceneElement::Shader(e.clone())));

        match self.bar.as_mut() {
            Some((s, element)) if *s == scale.to_bits() => element.resize(grow(layout.bar, 1), None),
            _ => self.bar = Some((scale.to_bits(), look.bar(shaders, layout.bar, scale))),
        }
        out.extend(self.bar.as_ref().map(|(_, e)| SceneElement::Shader(e.clone())));
        out
    }
}

/// `icon` lightened by `lighten` (Plank adds the icon to itself) and darkened by `darken` (towards black).
fn shade(icon: &Rendered, lighten: f32, darken: f32) -> Rendered {
    let mut pixels = icon.pixels.clone();
    for px in pixels.as_chunks_mut::<4>().0 {
        // Premultiplied: colour never exceeds alpha.
        let a = f32::from(px[3]);
        for c in &mut px[..3] {
            let v = f32::from(*c);
            *c = ((v + v * lighten).min(a) * (1.0 - darken)).round() as u8;
        }
    }
    Rendered { pixels, ..*icon }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_style_is_transparent_tight() {
        let look = Look::of(DockStyle::default());
        assert_eq!((look.top, look.bottom, look.opacity, look.radius, look.edge), (41.0 / 255.0, 80.0 / 255.0, 1.0, 0.0, 0.0));
        assert!([DockStyle::WhitesurDark, DockStyle::Glass].iter().all(|&s| Look::of(s).bottom < 1.0), "the sheen divides by 1 - bottom");
    }

    #[test]
    fn shading_stays_premultiplied() {
        let icon = Rendered { pixels: vec![100, 200, 50, 200, 0, 0, 0, 0], width: 2, height: 1, baseline: 1, truncated: false };
        assert_eq!(shade(&icon, 0.2, 0.0).pixels, [120, 200, 60, 200, 0, 0, 0, 0]);
        assert_eq!(shade(&icon, 0.0, 0.5).pixels, [50, 100, 25, 200, 0, 0, 0, 0]);
    }

    #[test]
    fn placeholder_is_a_rounded_grey_square() {
        let p = placeholder(32);
        let at = |x: usize, y: usize| &p.pixels[(y * 32 + x) * 4..][..4];
        assert_eq!((p.width, p.height, p.pixels.len()), (32, 32, 32 * 32 * 4));
        assert_eq!(at(16, 16), [128, 128, 128, 255]);
        assert_eq!(at(16, 0), [128, 128, 128, 255], "the edge midpoint is inside");
        assert_eq!(at(0, 0), [0, 0, 0, 0], "the corner is cut");
        assert!(Arc::ptr_eq(&p, &placeholder(32)), "cached");
    }
}
