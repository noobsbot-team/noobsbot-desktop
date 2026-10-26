//! The dock (E3.1) in Plank's Transparent-Tight look: square, borderless, an opaque rgb(41) → rgb(80) gradient bar
//! behind the lower half of the icons, a small dot under each running app. The same in light and dark (it's a theme).

use smithay::{
    backend::renderer::{
        element::{Kind, memory::MemoryRenderBufferRenderElement},
        gles::{GlesRenderer, element::PixelShaderElement},
    },
    utils::{Logical, Point, Rectangle, Size},
};

use std::{collections::HashMap, sync::Arc};

use super::{
    SceneElement, Shaders, grow,
    titlebar::{Label, keep_buffers, label_element},
    ui::{CALLOUT, Ui},
};
use crate::{
    apps::Icons,
    services::launcher::Entry,
    shell::{dock::Layout, dock_fx::Shade},
    symbols::Symbols,
    text::{Rendered, Text},
    wm::Rect,
};

/// FillStartColor / FillEndColor.
const TOP: f32 = 41.0 / 255.0;
const BOTTOM: f32 = 80.0 / 255.0;
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
    pub focused: bool,
    pub shade: Shade,
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
    /// Lightened or darkened copies of icons, by icon, size and shade in 1/20 steps.
    shaded: HashMap<(usize, u32, u8, u8), Arc<Rendered>>,
    /// The tooltip, made for these words, top-left, mode and scale.
    tip: Option<(TipKey, Vec<PixelShaderElement>, Vec<Piece>)>,
    tip_labels: Vec<Label>,
    /// The line before the minimised windows' tiles.
    separator: Option<(Rect, PixelShaderElement)>,
}

type TipKey = (String, Point<i32, Logical>, bool, u64);

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

    /// The hovered app's name (D8 tooltip), centred over `at` (its bottom edge).
    #[expect(clippy::too_many_arguments, reason = "the frame's drawing context")]
    pub fn tooltip(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        symbols: &mut Symbols,
        tip: Option<&(String, Point<i32, Logical>)>,
        dark: bool,
        scale: f64,
    ) -> Vec<SceneElement> {
        let Some((words, at)) = tip else {
            (self.tip, self.tip_labels) = (None, Vec::new());
            return Vec::new();
        };
        let key = (words.clone(), *at, dark, scale.to_bits());
        if self.tip.as_ref().is_none_or(|(k, _, _)| *k != key) {
            let mut ui = Ui { shaders, text, symbols, dark, accent: [0, 0, 0], glass_opacity: 1.0, scale, glass: Vec::new(), pieces: Vec::new() };
            let (w, h) = (ui.width(words, CALLOUT) + 16, i32::from(CALLOUT.line_height) + 12);
            ui.tooltip((at.x - w / 2, at.y - h).into(), words);
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
        let plain: Vec<_> = items.iter().map(|i| icons.get(i.name, px).or_else(|| icons.get(FALLBACK_ICON, px)).unwrap_or_else(|| placeholder(px))).collect();
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
        let mut buffers = self.icons.iter();
        let mut out = Vec::new();
        for (rect, item) in layout.icons.iter().zip(items) {
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
        }

        // IndicatorSize 5: a small light dot centred under the icon, in the band below it, two (s/16 either side) for
        // several windows; the focused app's icon sits on a faint highlight (Plank's active-window glow, without its
        // 300 ms fade).
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
                marks.push((Rectangle::new((centre + dx - dot / 2, y).into(), (dot, dot).into()), true));
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
            _ => self.bar = Some((scale.to_bits(), shaders.gradient(layout.bar, TOP, BOTTOM, scale))),
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
