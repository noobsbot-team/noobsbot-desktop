//! Text labels (C8, E1.3): cosmic-text shaping + swash rasterising into premultiplied RGBA, with an LRU cache.

use std::{collections::HashMap, sync::Arc, time::Instant};

use cosmic_text::{
    Align, Attrs, Buffer, Color, Ellipsize, EllipsizeHeightLimit, Family, FeatureTag, FontFeatures, FontSystem, Metrics, Shaping, SwashCache, Weight, Wrap,
};
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
    /// Equal-width digits (Inter `tnum`), for clocks and percentages that must not jiggle (Part D).
    pub tabular: bool,
}

/// A rasterised label: premultiplied RGBA8 (byte order R, G, B, A), in physical px.
#[derive(Debug, PartialEq)]
pub struct Rendered {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Baseline, in px from the top.
    pub baseline: u32,
    /// The text did not fit `max_width` and ends in "…".
    pub truncated: bool,
}

/// Text, style, width, scale, wrapping (see `cached`), in the emoji font.
type Key = (String, Style, u32, u64, Option<(u8, bool)>, bool);

/// Colour emoji fonts, best first (E7.2).
const EMOJI_FONTS: [&str; 3] = ["Noto Color Emoji", "Twemoji", "JoyPixels"];

/// Owns the font database and glyph rasteriser.
///
/// Fonts load lazily on the first `label` call, or on `preload` (a one-time system font scan: ~20 ms release, ~90 ms
/// debug, see the `font_load_time` test); the compositor preloads on a worker.
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
        self.cached(text, style, max_width, scale, None, false)
    }

    /// One emoji in the colour emoji font, so text-style ones (❤️, ⚽) and ones the text font happens to have
    /// come out in colour too; `style`'s colour only shows where no emoji font has the glyph.
    pub fn emoji(&mut self, emoji: &str, style: Style, scale: f64) -> Arc<Rendered> {
        self.cached(emoji, style, 1000, scale, None, true)
    }

    /// `text` word-wrapped to `width` logical px, every line centred in it (alerts); as wide as `width`.
    pub fn paragraph(&mut self, text: &str, style: Style, width: u32, scale: f64) -> Arc<Rendered> {
        self.cached(text, style, width, scale, Some((0, true)), false)
    }

    /// `text` word-wrapped to `width` logical px, every line centred, at most `lines` lines (desktop icon names).
    pub fn centred(&mut self, text: &str, style: Style, width: u32, lines: u8, scale: f64) -> Arc<Rendered> {
        self.cached(text, style, width, scale, Some((lines.max(1), true)), false)
    }

    /// `text` word-wrapped to `width` logical px, left-aligned, at most `lines` lines (the last ends in "…" when
    /// cut); as wide as `width`. Its height tells how many lines it took.
    pub fn wrapped(&mut self, text: &str, style: Style, width: u32, lines: u8, scale: f64) -> Arc<Rendered> {
        self.cached(text, style, width, scale, Some((lines.max(1), false)), false)
    }

    /// `wrap`: `None` one line, else (at most n lines, 0 = any; centred).
    fn cached(&mut self, text: &str, style: Style, max_width: u32, scale: f64, wrap: Option<(u8, bool)>, emoji: bool) -> Arc<Rendered> {
        self.tick += 1;
        let key = (text.to_owned(), style, max_width, scale.to_bits(), wrap, emoji);
        if let Some((rendered, used)) = self.cache.get_mut(&key) {
            *used = self.tick;
            return rendered.clone();
        }
        let rendered = Arc::new(match wrap {
            Some((lines, centre)) => self.rasterise_wrapped(text, style, max_width, lines, centre, scale),
            None => self.rasterise(text, style, max_width, scale, emoji),
        });
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

    /// Loads the fonts now instead of on the first `label`, and shapes a sample in the title (600) and glyph (700)
    /// weights: the first shaping of a face parses it, another ~20 ms.
    pub fn preload(&mut self) {
        for weight in [600, 700] {
            self.rasterise("Aa×–⤢", Style { size: 13, line_height: 16, weight, color: [0, 0, 0, 255], tabular: false }, 100, 1.0, false);
        }
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

    /// At most `lines` lines (0: as many as it takes), centred or left-aligned.
    fn rasterise_wrapped(&mut self, text: &str, style: Style, width: u32, lines: u8, centre: bool, scale: f64) -> Rendered {
        let (fonts, swash, family) = self.fonts();
        let scale = scale as f32;
        let metrics = Metrics::new(f32::from(style.size.max(1)) * scale, f32::from(style.line_height.max(1)) * scale);
        let family = if family.is_empty() { Family::SansSerif } else { Family::Name(family) };
        let attrs = Attrs::new().family(family).weight(Weight(style.weight));
        let limit = width as f32 * scale;
        let mut buffer = Buffer::new(fonts, metrics);
        let mut buffer = buffer.borrow_with(fonts);
        // A word wider than the line breaks between letters.
        buffer.set_wrap(Wrap::WordOrGlyph);
        buffer.set_size(Some(limit), None);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        if centre {
            for line in &mut buffer.lines {
                line.set_align(Some(Align::Center));
            }
        }
        buffer.shape_until_scroll(false);
        // Over the cap: cut after the last line that fits and end in "…", dropping words until it fits. (cosmic-text
        // 0.19's own ellipsis keeps the space at each line break, which indents the next line.)
        let cap = usize::from(lines);
        if cap > 0 && buffer.layout_runs().count() > cap {
            let starts: Vec<usize> = text.split('\n').scan(0, |at, line| Some(std::mem::replace(at, *at + line.len() + 1))).collect();
            let end = buffer.layout_runs().nth(cap - 1).map_or(0, |run| starts[run.line_i] + run.glyphs.last().map_or(0, |g| g.end));
            let mut cut = text[..end].trim_end().to_owned();
            loop {
                buffer.set_text(&format!("{cut}…"), &attrs, Shaping::Advanced, None);
                if centre {
                    for line in &mut buffer.lines {
                        line.set_align(Some(Align::Center));
                    }
                }
                buffer.shape_until_scroll(false);
                if buffer.layout_runs().count() <= cap || cut.is_empty() {
                    break;
                }
                // A word at a time, or a char at a time through one long word (file names).
                match cut.trim_end().rfind(char::is_whitespace) {
                    Some(at) => cut.truncate(at),
                    None => _ = cut.pop(),
                }
            }
        }
        let lines = buffer.layout_runs().count() as u32;
        let width = limit.floor() as u32;
        let height = if text.is_empty() { 0 } else { lines * metrics.line_height.ceil() as u32 };
        let baseline = buffer.layout_runs().next().map_or(0.0, |run| run.line_y);
        let pixels = draw(&mut buffer, swash, style.color, width, height);
        Rendered { pixels, width, height, baseline: baseline.round() as u32, truncated: false }
    }

    fn rasterise(&mut self, text: &str, style: Style, max_width: u32, scale: f64, emoji: bool) -> Rendered {
        let (fonts, swash, family) = self.fonts();
        let scale = scale as f32;
        let metrics = Metrics::new(f32::from(style.size.max(1)) * scale, f32::from(style.line_height.max(1)) * scale);
        let installed = |name: &str| fonts.db().faces().any(|f| f.families.iter().any(|(n, _)| n == name));
        let emoji_font = EMOJI_FONTS.into_iter().filter(|_| emoji).find(|name| installed(name));
        let family = match emoji_font {
            Some(name) => Family::Name(name),
            None if family.is_empty() => Family::SansSerif,
            None => Family::Name(family),
        };
        let [r, g, b, a] = style.color;
        let mut features = FontFeatures::new();
        if style.tabular {
            features.enable(FeatureTag::new(b"tnum"));
        }
        let attrs = Attrs::new().family(family).weight(Weight(style.weight)).font_features(features);
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
        let pixels = draw(&mut buffer, swash, [r, g, b, a], width, height);
        Rendered { pixels, width, height, baseline: baseline.round() as u32, truncated }
    }
}

/// Rasterises a laid-out buffer into `width` × `height` premultiplied RGBA in `color`.
fn draw(buffer: &mut cosmic_text::BorrowedWithFontSystem<Buffer>, swash: &mut SwashCache, [r, g, b, a]: [u8; 4], width: u32, height: u32) -> Vec<u8> {
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    buffer.draw(swash, Color::rgba(r, g, b, a), |x, y, w, h, c| {
        // cosmic-text hands over glyph coverage alone; the colour's own alpha is ours to apply.
        let alpha = u32::from(c.a()) * u32::from(a) / 255;
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
    pixels
}

/// Upload target for a label; `None` for an empty one. Keep it while the `Arc<Rendered>` is unchanged,
/// so the texture is not re-imported every frame. Draw it with
/// `MemoryRenderBufferRenderElement::from_buffer(renderer, physical_location, &buf, None, Some(whole), Some(logical_size), Kind::Unspecified)`,
/// where `whole` is the pixel size as a rect and `logical_size` the pixel size divided by the output scale.
pub fn to_buffer(rendered: &Rendered) -> Option<MemoryRenderBuffer> {
    // Memory order R, G, B, A is DRM's little-endian Abgr8888; GLES treats memory buffers as premultiplied.
    (rendered.width > 0).then(|| {
        MemoryRenderBuffer::from_slice(&rendered.pixels, Fourcc::Abgr8888, (rendered.width as i32, rendered.height as i32), 1, Transform::Normal, None)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADLINE: Style = Style { size: 13, line_height: 16, weight: 600, color: [0, 0, 0, 255], tabular: false };

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
    fn wrapped_text_stops_at_its_line_cap() {
        let Some(mut text) = text() else { return };
        let long = "Download complete: report.pdf (2.4 MB) was saved to your Downloads folder, where it waits for you.";
        assert_eq!(text.wrapped("Short", HEADLINE, 200, 2, 1.0).height, 16);
        assert_eq!(text.wrapped(long, HEADLINE, 200, 2, 1.0).height, 32);
        assert!(text.wrapped(long, HEADLINE, 200, 9, 1.0).height > 32);
        // Left-aligned: ink starts at the left edge.
        let one = text.wrapped("Short", HEADLINE, 200, 2, 1.0);
        let first_ink = one.pixels.as_chunks::<4>().0.iter().position(|p| p[3] > 0).map(|i| i as u32 % 200);
        assert!(first_ink.is_some_and(|x| x < 4), "{first_ink:?}");
        // The second line starts at the left edge too (no space carried over from the break).
        let two = text.wrapped(long, HEADLINE, 200, 2, 1.0);
        let left = (16..32).filter_map(|y| (0..200).find(|x| two.pixels[((y * 200 + x) * 4 + 3) as usize] > 0)).min();
        assert!(left.is_some_and(|x| x < 4), "{left:?}");
    }

    #[test]
    fn paragraphs_wrap_to_their_width() {
        let Some(mut text) = text() else { return };
        let one = text.paragraph("Restart", HEADLINE, 220, 1.0);
        let many = text.paragraph("Are you sure you want to restart your computer now? It restarts by itself soon.", HEADLINE, 220, 1.0);
        assert_eq!((one.width, one.height), (220, 16));
        assert!(many.height >= 48, "{} px tall", many.height);
        // Centred: a short line leaves its left edge empty.
        let first_ink = one.pixels.as_chunks::<4>().0.iter().position(|p| p[3] > 0).map(|i| i as u32 % 220);
        assert!(first_ink.is_some_and(|x| x > 60), "{first_ink:?}");
    }

    #[test]
    fn colour_alpha_fades_the_text() {
        let Some(mut text) = text() else { return };
        let max = |r: &Rendered| r.pixels.as_chunks::<4>().0.iter().map(|p| p[3]).max().unwrap_or(0);
        let solid = text.label("8", HEADLINE, 100, 1.0);
        let faded = text.label("8", Style { color: [0, 0, 0, 66], ..HEADLINE }, 100, 1.0);
        assert!(max(&faded) < max(&solid) / 2, "{} vs {}", max(&faded), max(&solid));
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
    fn tabular_digits_have_one_width() {
        let Some(mut text) = text() else { return };
        let style = Style { tabular: true, ..HEADLINE };
        assert_eq!(text.label("11:11", style, 300, 1.0).width, text.label("00:00", style, 300, 1.0).width);
    }

    #[test]
    fn empty_is_zero_width() {
        let Some(mut text) = text() else { return };
        let r = text.label("", HEADLINE, 300, 1.0);
        assert_eq!((r.width, r.pixels.len()), (0, 0));
        assert!(to_buffer(&r).is_none());
    }

    #[test]
    fn emoji_render_in_colour() {
        let Some(mut text) = text() else { return };
        // Noto Color Emoji, Twemoji: cosmic-text's fallback finds either.
        if !text.fonts().0.db().faces().any(|f| f.families.iter().any(|(name, _)| name.to_lowercase().contains("emoji"))) {
            return eprintln!("no colour emoji font installed, skipping");
        }
        let r = text.emoji("❤️", Style { size: 24, line_height: 30, ..HEADLINE }, 1.0);
        let coloured = r.pixels.as_chunks::<4>().0.iter().filter(|p| p[3] > 128 && p[0].abs_diff(p[2]) > 60).count();
        eprintln!("emoji {}x{}, coloured px {coloured}", r.width, r.height);
        assert!(coloured > 20, "no colour emoji font: install noto-fonts-emoji");
    }

    #[test]
    fn font_load_time() {
        let start = Instant::now();
        let Some(mut text) = text() else { return };
        eprintln!("font load: {:?}, family {:?}", start.elapsed(), text.fonts().2);
    }
}
