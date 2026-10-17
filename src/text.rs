//! Text labels (C8, E1.3): cosmic-text shaping + swash rasterising into premultiplied RGBA, with an LRU cache.

use std::{collections::HashMap, sync::Arc, time::Instant};

use cosmic_text::{Attrs, Buffer, Color, Ellipsize, EllipsizeHeightLimit, Family, FontSystem, Metrics, Shaping, SwashCache, Weight, Wrap};
use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    utils::Transform,
};

const MAX_ENTRIES: usize = 256;
const MAX_BYTES: usize = 16 << 20;

/// How a label looks. Sizes are logical px (Part D: Headline = 13 / 16, weight 600).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Style {
    pub size: u16,
    pub line_height: u16,
    pub weight: u16,
    /// Straight (not premultiplied) RGBA.
    pub color: [u8; 4],
}

/// A rasterised label: premultiplied RGBA8 (byte order R, G, B, A), in physical px.
#[derive(Debug)]
pub struct Rendered {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Baseline, in px from the top.
    #[cfg_attr(not(test), expect(dead_code, reason = "for aligning labels with other text (menu bar, M2)"))]
    pub baseline: u32,
    /// The text did not fit `max_width` and ends in "…".
    #[cfg_attr(not(test), expect(dead_code, reason = "a truncated title shows in full on hover (later)"))]
    pub truncated: bool,
}

type Key = (String, Style, u32, u64);

/// Owns the font database and glyph rasteriser.
///
/// Fonts load lazily on the first `label` call (a one-time system font scan: ~20 ms release, ~90 ms debug, see the
/// `font_load_time` test): create it on a worker, or accept that first-frame cost.
#[derive(Default)]
pub struct Text {
    fonts: Option<(FontSystem, SwashCache, String)>,
    cache: HashMap<Key, (Arc<Rendered>, u64)>,
    bytes: usize,
    tick: u64,
}

impl Text {
    /// One line of `text`, ellipsised to `max_width` logical px. Unchanged labels come from the cache.
    pub fn label(&mut self, text: &str, style: Style, max_width: u32, scale: f64) -> Arc<Rendered> {
        self.tick += 1;
        let key = (text.to_owned(), style, max_width, scale.to_bits());
        if let Some((rendered, used)) = self.cache.get_mut(&key) {
            *used = self.tick;
            return rendered.clone();
        }
        let rendered = Arc::new(self.rasterise(text, style, max_width, scale));
        // ponytail: O(n) scan for the oldest entry, fine at 256 entries; a linked LRU if the cap grows.
        while !self.cache.is_empty() && (self.cache.len() >= MAX_ENTRIES || self.bytes + rendered.pixels.len() > MAX_BYTES) {
            let Some(oldest) = self.cache.iter().min_by_key(|(_, (_, used))| *used).map(|(k, _)| k.clone()) else { break };
            if let Some((old, _)) = self.cache.remove(&oldest) {
                self.bytes -= old.pixels.len();
            }
        }
        self.bytes += rendered.pixels.len();
        self.cache.insert(key, (rendered.clone(), self.tick));
        rendered
    }

    fn fonts(&mut self) -> &mut (FontSystem, SwashCache, String) {
        self.fonts.get_or_insert_with(|| {
            let start = Instant::now();
            let fonts = FontSystem::new();
            let inter =
                ["Inter", "Inter Variable"].into_iter().find(|name| fonts.db().faces().any(|face| face.families.iter().any(|(family, _)| family == name)));
            let family = match inter {
                Some(name) => name.to_owned(),
                None => {
                    tracing::warn!("font Inter not installed (package inter-font), using the system sans-serif");
                    String::new()
                }
            };
            tracing::debug!(faces = fonts.db().len(), "fonts loaded in {:?}", start.elapsed());
            (fonts, SwashCache::new(), family)
        })
    }

    fn rasterise(&mut self, text: &str, style: Style, max_width: u32, scale: f64) -> Rendered {
        let (fonts, swash, family) = self.fonts();
        let scale = scale as f32;
        let metrics = Metrics::new(f32::from(style.size.max(1)) * scale, f32::from(style.line_height.max(1)) * scale);
        let family = if family.is_empty() { Family::SansSerif } else { Family::Name(family) };
        let [r, g, b, a] = style.color;
        let attrs = Attrs::new().family(family).weight(Weight(style.weight));
        let mut buffer = Buffer::new(fonts, metrics);
        let mut buffer = buffer.borrow_with(fonts);
        buffer.set_wrap(Wrap::None);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        let natural = buffer.layout_runs().map(|run| run.line_w).fold(0.0, f32::max);
        let limit = max_width as f32 * scale;
        let truncated = natural > limit;
        if truncated {
            buffer.set_ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)));
            buffer.set_size(Some(limit), None);
        }
        let (line_w, baseline) = buffer.layout_runs().next().map_or((0.0, 0.0), |run| (run.line_w, run.line_y));
        let width = (line_w.ceil() as u32).min(limit.floor() as u32);
        let height = if width == 0 { 0 } else { metrics.line_height.ceil() as u32 };
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        buffer.draw(swash, Color::rgba(r, g, b, a), |x, y, w, h, c| {
            let alpha = u32::from(c.a());
            if alpha == 0 {
                return;
            }
            let src = [c.r(), c.g(), c.b()].map(|v| u32::from(v) * alpha / 255);
            for py in y.max(0)..(y + h as i32).min(height as i32) {
                for px in x.max(0)..(x + w as i32).min(width as i32) {
                    let i = (py as u32 * width + px as u32) as usize * 4;
                    let dst = &mut pixels[i..i + 4];
                    for (d, s) in dst.iter_mut().zip(src.into_iter().chain([alpha])) {
                        *d = (s + u32::from(*d) * (255 - alpha) / 255) as u8;
                    }
                }
            }
        });
        Rendered { pixels, width, height, baseline: baseline.round() as u32, truncated }
    }
}

/// Upload target for a label; `None` for an empty one. Keep it while the `Arc<Rendered>` is unchanged,
/// so the texture is not re-imported every frame. Draw it with
/// `MemoryRenderBufferRenderElement::from_buffer(renderer, physical_location, &buf, None, None, Some(logical_size), Kind::Unspecified)`,
/// where `logical_size` is the pixel size divided by the output scale.
pub fn to_buffer(rendered: &Rendered) -> Option<MemoryRenderBuffer> {
    // Memory order R, G, B, A is DRM's little-endian Abgr8888; GLES treats memory buffers as premultiplied.
    (rendered.width > 0).then(|| {
        MemoryRenderBuffer::from_slice(&rendered.pixels, Fourcc::Abgr8888, (rendered.width as i32, rendered.height as i32), 1, Transform::Normal, None)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADLINE: Style = Style { size: 13, line_height: 16, weight: 600, color: [0, 0, 0, 255] };

    /// A loaded `Text`, or `None` when the machine has no fonts at all.
    fn text() -> Option<Text> {
        let mut text = Text::default();
        if text.fonts().0.db().is_empty() {
            eprintln!("no fonts installed, skipping");
            return None;
        }
        Some(text)
    }

    #[test]
    fn truncates_with_ellipsis_to_fit() {
        let Some(mut text) = text() else { return };
        let long = "A very long window title that cannot possibly fit in a narrow title bar";
        let full = text.label(long, HEADLINE, 10_000, 1.0);
        assert!(!full.truncated && full.width > 100);
        assert!(full.baseline > 0 && full.baseline < full.height, "baseline {} of {}", full.baseline, full.height);
        let cut = text.label(long, HEADLINE, 100, 1.0);
        assert!(cut.truncated && cut.width > 0 && cut.width <= 100, "{} px", cut.width);
        // The last glyph is "…", the same one an ellipsis on its own shapes to.
        let (fonts, _, family) = text.fonts();
        let attrs = Attrs::new().family(Family::Name(family)).weight(Weight(600));
        let glyph = |s: &str, width: Option<f32>, fonts: &mut FontSystem| {
            let mut b = Buffer::new(fonts, Metrics::new(13.0, 16.0));
            let mut b = b.borrow_with(fonts);
            b.set_wrap(Wrap::None);
            b.set_ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)));
            b.set_size(width, None);
            b.set_text(s, &attrs, Shaping::Advanced, None);
            b.layout_runs().next().and_then(|run| run.glyphs.last().map(|g| g.glyph_id))
        };
        assert_eq!(glyph(long, Some(100.0), fonts), glyph("…", None, fonts));
    }

    #[test]
    fn cache_hit_returns_same_buffer() {
        let Some(mut text) = text() else { return };
        let a = text.label("Documents", HEADLINE, 300, 2.0);
        let b = text.label("Documents", HEADLINE, 300, 2.0);
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(text.cache.len(), 1);
        assert!(!Arc::ptr_eq(&a, &text.label("Documents", HEADLINE, 300, 1.0)));
    }

    #[test]
    fn cache_is_capped() {
        let Some(mut text) = text() else { return };
        for i in 0..MAX_ENTRIES + 10 {
            text.label(&i.to_string(), HEADLINE, 300, 1.0);
        }
        assert_eq!(text.cache.len(), MAX_ENTRIES);
        assert!(text.bytes <= MAX_BYTES);
    }

    #[test]
    fn empty_is_zero_width() {
        let Some(mut text) = text() else { return };
        let r = text.label("", HEADLINE, 300, 1.0);
        assert_eq!((r.width, r.pixels.len()), (0, 0));
        assert!(to_buffer(&r).is_none());
    }

    #[test]
    fn font_load_time() {
        let start = Instant::now();
        let Some(mut text) = text() else { return };
        eprintln!("font load: {:?}, family {:?}", start.elapsed(), text.fonts().2);
    }
}
