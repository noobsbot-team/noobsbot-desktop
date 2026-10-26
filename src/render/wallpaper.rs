//! The wallpaper (E1.5, D11, E8.4): a PNG/JPEG decoded once on a worker thread, uploaded once, placed by the fit
//! mode; the built-in opal gradient when no file is set or the file can't be used. A slideshow folder replaces the
//! files; every change crossfades over a second.

use std::{
    io::Cursor,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            ImportMem,
            element::{
                Kind,
                texture::{TextureBuffer, TextureRenderElement},
            },
            gles::{GlesRenderer, GlesTexture, element::PixelShaderElement},
        },
    },
    reexports::calloop::channel::Sender,
    utils::{Buffer, Logical, Rectangle, Size, Transform},
};
use zune_jpeg::{
    JpegDecoder,
    zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions},
};

use super::{SceneElement, Shaders};
use crate::config::{Appearance, WallpaperFit};

/// The longest side a wallpaper file may have; a bigger one is refused rather than decoded into hundreds of MB.
const MAX_SIDE: usize = 8192;
/// Opal gradients kept, one per (output size, dark); the outputs of one desktop are a handful.
const MAX_OPALS: usize = 8;

/// A decoded picture on its way from the worker thread to the GPU.
pub struct Decoded {
    path: PathBuf,
    /// The output size it was shrunk to cover; an older decode is dropped when a bigger output has turned up.
    limit: Size<i32, Buffer>,
    image: Result<Image, String>,
}

/// 8-bit RGBX pixels.
pub(super) struct Image {
    pub(super) size: Size<i32, Buffer>,
    pub(super) rgbx: Vec<u8>,
    /// True when the file was bigger than `size`: a larger output needs it decoded again.
    pub(super) shrunk: bool,
}

/// How long any wallpaper change crossfades. Reduce motion keeps it: a fade is already the reduced form (D10).
const FADE: Duration = Duration::from_secs(1);

/// What a crossfade starts from: a picture, or the opal gradient (dark or not).
enum Shown {
    Opal(bool),
    Picture(TextureBuffer<GlesTexture>, Size<i32, Buffer>),
}

/// An uploaded picture: texture, size, whether it was shrunk, and its file.
type Texture = (TextureBuffer<GlesTexture>, Size<i32, Buffer>, bool, PathBuf);
type Cached = Option<(Size<i32, Logical>, bool, PixelShaderElement)>;

pub struct Wallpaper {
    tx: Sender<Decoded>,
    /// The files, fit and slideshow asked for.
    appearance: Appearance,
    /// The slideshow's picture and when it was picked.
    slide: Option<(PathBuf, Instant)>,
    /// The file asked for; `None` = the opal gradient.
    wanted: Option<PathBuf>,
    /// The largest output seen, in physical px: the picture is decoded no bigger than needed to cover it (C20).
    limit: Size<i32, Buffer>,
    /// The limit the latest decode was asked with; `None` = nothing asked yet.
    asked: Option<Size<i32, Buffer>>,
    /// The picture shown; kept until the next one has arrived, so a change fades instead of flashing the opal.
    texture: Option<Texture>,
    opal: Vec<(Size<i32, Logical>, bool, PixelShaderElement)>,
    /// The opal gradient drawn last (dark or not), so a light/dark flip crossfades too.
    opal_dark: Option<bool>,
    /// What is fading out, and since when.
    fade: Option<(Shown, Instant)>,
    /// The desktop colour around a picture that doesn't cover the output, kept like the opal gradient.
    backdrop: Cached,
    /// How many elements the latest `element` returned: desktop things go just in front of them.
    pub drawn: usize,
}

impl Wallpaper {
    /// Decoded pictures come back through `tx`; hand each to `receive`.
    pub fn new(tx: Sender<Decoded>) -> Self {
        Self {
            tx,
            appearance: Appearance::default(),
            slide: None,
            wanted: None,
            limit: Size::default(),
            asked: None,
            texture: None,
            opal: Vec::new(),
            opal_dark: None,
            fade: None,
            backdrop: None,
            drawn: 0,
        }
    }

    /// Takes the wallpaper settings; the next frame picks the file (decoded on a worker thread once an output's size
    /// is known, then crossfaded in). A new slideshow folder picks its first picture now.
    // ponytail: the folder is listed on the frame thread (one read_dir); move it to the decode thread if a huge one stalls.
    pub fn set(&mut self, appearance: &Appearance) {
        let new_folder = appearance.wallpaper_folder != self.appearance.wallpaper_folder;
        self.appearance = appearance.clone();
        if new_folder {
            self.slide = None;
            self.advance();
        }
    }

    /// Slideshow: moves to the next picture when its time is up; true when it did (redraw to show it).
    pub fn tick(&mut self) -> bool {
        let interval = Duration::from_secs(u64::from(self.appearance.wallpaper_interval) * 60);
        if self.appearance.wallpaper_folder.is_empty() || interval.is_zero() || self.slide.as_ref().is_some_and(|(_, at)| at.elapsed() < interval) {
            return false;
        }
        let was = self.slide.as_ref().map(|(p, _)| p.clone());
        self.advance();
        self.slide.as_ref().map(|(p, _)| p) != was.as_ref()
    }

    fn advance(&mut self) {
        let folder = &self.appearance.wallpaper_folder;
        let files = if folder.is_empty() { Vec::new() } else { pictures(&expand_home(folder)) };
        let seed = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos() as usize);
        let next = pick(&files, self.slide.as_ref().map(|(p, _)| p.as_path()), self.appearance.wallpaper_shuffle, seed).cloned();
        self.slide = next.map(|p| (p, Instant::now()));
    }

    /// Whether a crossfade needs more frames.
    pub fn fading(&self) -> bool {
        self.fade.is_some()
    }

    /// The file for `dark` mode: the slideshow's picture, else the light or dark one. A new one starts its decode.
    fn want(&mut self, dark: bool) {
        let file = if dark { &self.appearance.wallpaper_dark } else { &self.appearance.wallpaper };
        let wanted = self.slide.as_ref().map(|(p, _)| p.clone()).or_else(|| (!file.is_empty()).then(|| expand_home(file)));
        if wanted == self.wanted {
            return;
        }
        if wanted.is_none()
            && let Some((texture, size, ..)) = self.texture.take()
        {
            self.fade = Some((Shown::Picture(texture, size), Instant::now()));
        }
        self.asked = None;
        self.wanted = wanted;
        self.decode();
    }

    fn decode(&mut self) {
        let Some(path) = self.wanted.clone().filter(|_| self.limit.w > 0 && self.asked != Some(self.limit)) else { return };
        let (tx, limit) = (self.tx.clone(), self.limit);
        self.asked = Some(limit);
        let spawned = std::thread::Builder::new().name("wallpaper-decode".into()).spawn(move || {
            let image = decode(&path, limit);
            let _ = tx.send(Decoded { path, limit, image });
        });
        if let Err(err) = spawned {
            tracing::warn!("wallpaper: {err}, using the built-in one");
        }
    }

    /// Uploads a decoded picture if it is still the one wanted and crossfades to it; a failure is logged and fades to
    /// the opal gradient.
    pub fn receive(&mut self, renderer: &mut GlesRenderer, decoded: Decoded) {
        if self.wanted.as_ref() != Some(&decoded.path) || self.asked != Some(decoded.limit) {
            return;
        }
        let Decoded { path, image, .. } = decoded;
        let uploaded = image.and_then(|image| {
            let texture = renderer.import_memory(&image.rgbx, Fourcc::Xbgr8888, image.size, false).map_err(|e| e.to_string())?;
            Ok((TextureBuffer::from_texture(renderer, texture, 1, Transform::Normal, Some(vec![Rectangle::from_size(image.size)])), image.size, image.shrunk))
        });
        let old = match uploaded {
            Ok((texture, size, shrunk)) => self.texture.replace((texture, size, shrunk, path)),
            Err(err) => {
                tracing::warn!("wallpaper {}: {err}, using the built-in one", path.display());
                self.texture.take()
            }
        };
        let from = match old {
            // The same picture decoded again for a bigger output just swaps.
            Some((.., was)) if self.texture.as_ref().is_some_and(|t| t.3 == was) => return,
            Some((texture, size, ..)) => Shown::Picture(texture, size),
            None if self.texture.is_none() => return,
            None => Shown::Opal(self.opal_dark.unwrap_or(false)),
        };
        self.fade = Some((from, Instant::now()));
    }

    /// The bottom elements of an output of `size` at `scale`, front to back: what is fading out, then the picture
    /// placed by `fit` (with the desktop colour where it doesn't reach), else the opal gradient.
    pub fn element(&mut self, shaders: &Shaders, size: Size<i32, Logical>, scale: f64, dark: bool) -> Vec<SceneElement> {
        self.want(dark);
        let physical = size.to_f64().to_physical(scale).to_i32_round::<i32>();
        if physical.w > self.limit.w || physical.h > self.limit.h {
            self.limit = (physical.w.max(self.limit.w), physical.h.max(self.limit.h)).into();
            // A picture shrunk for a smaller output would be upscaled on this one: decode it again at the new limit.
            if self.texture.as_ref().is_none_or(|(_, _, shrunk, _)| *shrunk) {
                self.decode();
            }
        }
        let fit = self.appearance.wallpaper_fit;
        let mut out = match &self.texture {
            Some((texture, image, ..)) => picture(shaders, fit, texture, *image, size, scale, dark, 1.0, &mut self.backdrop),
            None => {
                if let Some(was) = self.opal_dark.filter(|was| *was != dark && self.fade.is_none()) {
                    self.fade = Some((Shown::Opal(was), Instant::now()));
                }
                self.opal_dark = Some(dark);
                vec![self.opal(shaders, size, dark)]
            }
        };
        if let Some((from, start)) = &self.fade {
            let t = start.elapsed().as_secs_f32() / FADE.as_secs_f32();
            if t >= 1.0 {
                self.fade = None;
            } else {
                // Eased in and out (smoothstep).
                let alpha = 1.0 - t * t * (3.0 - 2.0 * t);
                let old = match from {
                    Shown::Opal(was) => vec![SceneElement::Shader(shaders.opal(Rectangle::from_size(size), *was, alpha))],
                    Shown::Picture(texture, image) => picture(shaders, fit, texture, *image, size, scale, dark, alpha, &mut None),
                };
                out.splice(0..0, old);
            }
        }
        self.drawn = out.len();
        out
    }

    /// One gradient per output size (C10): resizing a shared one every frame redrew every output every frame.
    fn opal(&mut self, shaders: &Shaders, size: Size<i32, Logical>, dark: bool) -> SceneElement {
        if let Some((_, _, opal)) = self.opal.iter().find(|(s, d, _)| (*s, *d) == (size, dark)) {
            return SceneElement::Shader(opal.clone());
        }
        if self.opal.len() >= MAX_OPALS {
            self.opal.clear();
        }
        let opal = shaders.opal(Rectangle::from_size(size), dark, 1.0);
        self.opal.push((size, dark, opal.clone()));
        SceneElement::Shader(opal)
    }
}

/// A picture placed by `fit` at `alpha`, over the desktop colour where it doesn't reach (kept in `backdrop` while opaque).
#[allow(clippy::too_many_arguments)]
fn picture(
    shaders: &Shaders,
    fit: WallpaperFit,
    texture: &TextureBuffer<GlesTexture>,
    image: Size<i32, Buffer>,
    size: Size<i32, Logical>,
    scale: f64,
    dark: bool,
    alpha: f32,
    backdrop: &mut Cached,
) -> Vec<SceneElement> {
    let rects = if fit == WallpaperFit::Tile { tiles(image, size, scale) } else { vec![place(fit, image, size, scale)] };
    let (_, dst) = rects[0];
    let mut out: Vec<SceneElement> = rects
        .into_iter()
        .map(|(src, dst)| {
            let at = dst.loc.to_physical(scale);
            SceneElement::Texture(TextureRenderElement::from_texture_buffer(
                at,
                texture,
                Some(alpha),
                Some(src),
                Some(dst.size.to_i32_round()),
                Kind::Unspecified,
            ))
        })
        .collect();
    if fit != WallpaperFit::Tile && (dst.size.to_i32_round() != size || dst.loc != (0.0, 0.0).into()) {
        if alpha < 1.0 || backdrop.as_ref().is_none_or(|(s, d, _)| (*s, *d) != (size, dark)) {
            let [r, g, b] = if dark { [0.11, 0.11, 0.12] } else { [0.82, 0.82, 0.84] };
            *backdrop = Some((size, dark, shaders.glass_shape(Rectangle::from_size(size), [r, g, b, alpha], 0.0, false, dark, scale)));
        }
        out.extend(backdrop.as_ref().map(|(_, _, e)| SceneElement::Shader(e.clone())));
    }
    out
}

/// The PNG and JPEG files in `folder`, by name.
fn pictures(folder: &Path) -> Vec<PathBuf> {
    let picture = |p: &Path| p.extension().and_then(|e| e.to_str()).is_some_and(|e| ["png", "jpg", "jpeg"].contains(&e.to_ascii_lowercase().as_str()));
    let mut files: Vec<PathBuf> = std::fs::read_dir(folder).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| picture(p)).collect();
    files.sort();
    files
}

/// The slideshow's next picture after `current`: the following one by name (wrapping), or with `shuffle` any other
/// one chosen by `seed`. Without a current one: the first, or a random one.
// ponytail: shuffle picks independently each time, so a picture can come back before all were shown.
fn pick<'a>(files: &'a [PathBuf], current: Option<&Path>, shuffle: bool, seed: usize) -> Option<&'a PathBuf> {
    let n = files.len();
    let i = match files.iter().position(|f| Some(f.as_path()) == current) {
        _ if n == 0 => return None,
        Some(i) if shuffle && n > 1 => (i + 1 + seed % (n - 1)) % n,
        Some(i) => (i + 1) % n,
        None if shuffle => seed % n,
        None => 0,
    };
    files.get(i)
}

/// `~/x` → `$HOME/x`.
fn expand_home(file: &str) -> PathBuf {
    match (file.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(file),
    }
}

/// The part of an image of `image` px that fills `output` without stretching, centred ("cover").
pub fn cover(image: Size<i32, Buffer>, output: Size<i32, Logical>) -> Rectangle<f64, Logical> {
    let (iw, ih, ow, oh) = (f64::from(image.w), f64::from(image.h), f64::from(output.w), f64::from(output.h));
    if iw <= 0.0 || ih <= 0.0 || ow <= 0.0 || oh <= 0.0 {
        return Rectangle::from_size((iw.max(1.0), ih.max(1.0)).into());
    }
    let scale = (ow / iw).max(oh / ih);
    let (w, h) = (ow / scale, oh / scale);
    Rectangle::new(((iw - w) / 2.0, (ih - h) / 2.0).into(), (w, h).into())
}

/// Where a picture of `image` px goes on an output of `output` at `scale`: the part of the picture shown, and the
/// rectangle it fills, by `fit`.
pub fn place(fit: WallpaperFit, image: Size<i32, Buffer>, output: Size<i32, Logical>, scale: f64) -> (Rectangle<f64, Logical>, Rectangle<f64, Logical>) {
    let (iw, ih, ow, oh) = (f64::from(image.w).max(1.0), f64::from(image.h).max(1.0), f64::from(output.w), f64::from(output.h));
    let whole = Rectangle::from_size((iw, ih).into());
    let centred = |w: f64, h: f64| Rectangle::new(((ow - w) / 2.0, (oh - h) / 2.0).into(), (w, h).into());
    match fit {
        WallpaperFit::Fill => (cover(image, output), Rectangle::from_size((ow, oh).into())),
        WallpaperFit::Stretch => (whole, Rectangle::from_size((ow, oh).into())),
        WallpaperFit::Fit => {
            let s = (ow / iw).min(oh / ih);
            (whole, centred(iw * s, ih * s))
        }
        WallpaperFit::Tile => tiles(image, output, scale).first().copied().unwrap_or_default(),
        WallpaperFit::Centre => {
            // One picture pixel per screen pixel; cropped where it is bigger than the output.
            let (w, h) = ((iw / scale).min(ow), (ih / scale).min(oh));
            let src = Rectangle::new(((iw - w * scale) / 2.0, (ih - h * scale) / 2.0).into(), (w * scale, h * scale).into());
            (src, centred(w, h))
        }
    }
}

/// Tile: the picture at one pixel per screen pixel, repeated rightwards and down from the top-left corner, the last
/// row and column cropped to the output; (part of the picture, where it goes) for each copy.
// ponytail: one texture element per copy, so a tiny pattern on a big screen makes thousands; repeat it into a bigger
// texture on upload if that shows.
pub fn tiles(image: Size<i32, Buffer>, output: Size<i32, Logical>, scale: f64) -> Vec<(Rectangle<f64, Logical>, Rectangle<f64, Logical>)> {
    let (tw, th) = (f64::from(image.w).max(1.0) / scale, f64::from(image.h).max(1.0) / scale);
    let (ow, oh) = (f64::from(output.w), f64::from(output.h));
    let steps = |tile: f64, end: f64| (0..).map(move |i| f64::from(i) * tile).take_while(move |at| *at < end);
    steps(th, oh)
        .flat_map(|y| steps(tw, ow).map(move |x| (x, y)))
        .map(|(x, y)| {
            let (w, h) = (tw.min(ow - x), th.min(oh - y));
            (Rectangle::from_size((w * scale, h * scale).into()), Rectangle::new((x, y).into(), (w, h).into()))
        })
        .collect()
}

/// PNG by its signature, anything else as JPEG. Alpha is dropped: a wallpaper is opaque. A picture more than twice
/// the size needed to cover `limit` is box-filtered down by a whole factor, so the GPU scales (and holds) a
/// screen-sized texture, not a 40 MB photo; one longer than `MAX_SIDE` is refused before any pixel is decoded.
pub(super) fn decode(path: &Path, limit: Size<i32, Buffer>) -> Result<Image, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let too_big = |w: usize, h: usize| (w > MAX_SIDE || h > MAX_SIDE).then(|| format!("{w}×{h} is more than {MAX_SIDE} px a side"));
    let (width, height, channels, pixels) = if bytes.starts_with(b"\x89PNG") {
        let mut decoder = png::Decoder::new(Cursor::new(&bytes));
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
        if let Some(err) = too_big(reader.info().width as usize, reader.info().height as usize) {
            return Err(err);
        }
        let mut pixels = vec![0; reader.output_buffer_size().ok_or("image too large")?];
        let info = reader.next_frame(&mut pixels).map_err(|e| e.to_string())?;
        let channels = match info.color_type {
            png::ColorType::Grayscale => 1,
            png::ColorType::GrayscaleAlpha => 2,
            png::ColorType::Rgb => 3,
            png::ColorType::Rgba => 4,
            png::ColorType::Indexed => return Err("unexpanded palette".into()),
        };
        pixels.truncate(info.buffer_size());
        (info.width as usize, info.height as usize, channels, pixels)
    } else {
        let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
        let mut decoder = JpegDecoder::new_with_options(ZCursor::new(&bytes), options);
        decoder.decode_headers().map_err(|e| format!("{e:?}"))?;
        let (width, height) = decoder.dimensions().ok_or("no image size")?;
        if let Some(err) = too_big(width, height) {
            return Err(err);
        }
        let pixels = decoder.decode().map_err(|e| format!("{e:?}"))?;
        (width, height, 4, pixels)
    };
    if pixels.len() < width * height * channels {
        return Err("truncated image".into());
    }
    // The whole factor that leaves the picture still covering the limit; 1 keeps it as is.
    let factor = match (usize::try_from(limit.w), usize::try_from(limit.h)) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => (width / w).min(height / h).max(1),
        _ => 1,
    };
    let (out_w, out_h) = (width / factor, height / factor);
    let size = Size::from((i32::try_from(out_w).map_err(|e| e.to_string())?, i32::try_from(out_h).map_err(|e| e.to_string())?));
    let at = |x: usize, y: usize, c: usize| u32::from(pixels[(y * width + x) * channels + c]);
    let mut rgbx = Vec::with_capacity(out_w * out_h * 4);
    for y in 0..out_h {
        for x in 0..out_w {
            let mut sum = [0u32; 3];
            for (dy, dx) in (0..factor).flat_map(|dy| (0..factor).map(move |dx| (dy, dx))) {
                let (sx, sy) = (x * factor + dx, y * factor + dy);
                sum = match channels {
                    1 | 2 => [sum[0] + at(sx, sy, 0); 3],
                    _ => [sum[0] + at(sx, sy, 0), sum[1] + at(sx, sy, 1), sum[2] + at(sx, sy, 2)],
                };
            }
            let n = (factor * factor) as u32;
            rgbx.extend([(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8, 255]);
        }
    }
    Ok(Image { size, rgbx, shrunk: factor > 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_crops_the_long_side_and_centres() {
        // 4:3 picture on a 16:9 screen: full width, a centred band of the height.
        let src = cover((4000, 3000).into(), (1920, 1080).into());
        assert_eq!(src, Rectangle::new((0.0, 375.0).into(), (4000.0, 2250.0).into()));
        // A tall picture: full height, centred width.
        let src = cover((1000, 2000).into(), (1000, 1000).into());
        assert_eq!(src, Rectangle::new((0.0, 500.0).into(), (1000.0, 1000.0).into()));
        let src = cover((3000, 1000).into(), (1000, 1000).into());
        assert_eq!(src, Rectangle::new((1000.0, 0.0).into(), (1000.0, 1000.0).into()));
        // Degenerate sizes don't divide by zero.
        assert_eq!(cover((0, 0).into(), (1920, 1080).into()).size, (1.0, 1.0).into());
    }

    #[test]
    fn slideshow_picks_the_next_picture() {
        let files: Vec<PathBuf> = ["a.jpg", "b.png", "c.jpeg"].map(PathBuf::from).into();
        let at = |i: usize| Some(files[i].as_path());
        // In order, wrapping; an unknown or missing current starts at the first.
        assert_eq!(pick(&files, None, false, 7), Some(&files[0]));
        assert_eq!(pick(&files, at(0), false, 7), Some(&files[1]));
        assert_eq!(pick(&files, at(2), false, 7), Some(&files[0]));
        assert_eq!(pick(&files, Some(Path::new("gone.png")), false, 7), Some(&files[0]));
        // Shuffled: never the current one, every other one reachable.
        for seed in 0..10 {
            assert_ne!(pick(&files, at(1), true, seed), Some(&files[1]));
        }
        assert_eq!(pick(&files, at(1), true, 0), Some(&files[2]));
        assert_eq!(pick(&files, at(1), true, 1), Some(&files[0]));
        assert_eq!(pick(&files, None, true, 4), Some(&files[1]));
        // One picture stays; none gives none.
        assert_eq!(pick(&files[..1], at(0), true, 3), Some(&files[0]));
        assert_eq!(pick(&[], None, true, 3), None);
    }

    #[test]
    fn slideshow_lists_pictures_by_name() {
        let dir = std::env::temp_dir().join(format!("noobsbot-desktop-test-slides-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["b.JPG", "a.png", "notes.txt", "c.jpeg"] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        assert_eq!(pictures(&dir), ["a.png", "b.JPG", "c.jpeg"].map(|n| dir.join(n)));
        assert!(pictures(&dir.join("missing")).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn fit_modes_place_the_picture() {
        let (image, output) = (Size::from((2000, 1000)), Size::from((1000, 1000)));
        let screen = Rectangle::from_size((1000.0, 1000.0).into());
        assert_eq!(place(WallpaperFit::Fill, image, output, 1.0), (cover(image, output), screen));
        assert_eq!(place(WallpaperFit::Stretch, image, output, 1.0), (Rectangle::from_size((2000.0, 1000.0).into()), screen));
        // Fit: full width, a centred band.
        assert_eq!(place(WallpaperFit::Fit, image, output, 1.0).1, Rectangle::new((0.0, 250.0).into(), (1000.0, 500.0).into()));
        // Centre at 2×: a 2000 px picture is 1000 logical px, the whole width, half the height.
        let (src, dst) = place(WallpaperFit::Centre, image, output, 2.0);
        assert_eq!((src, dst), (Rectangle::from_size((2000.0, 1000.0).into()), Rectangle::new((0.0, 250.0).into(), (1000.0, 500.0).into())));
        // Centre at 1×: cropped to the middle 1000 px.
        assert_eq!(place(WallpaperFit::Centre, image, output, 1.0).0, Rectangle::new((500.0, 0.0).into(), (1000.0, 1000.0).into()));
    }

    #[test]
    fn tiles_repeat_from_the_corner_and_crop_the_last() {
        // A 400×300 px picture at 2× is 200×150 logical: 3 columns (the last 100 wide) by 2 rows on 500×300.
        let tiles = tiles(Size::from((400, 300)), Size::from((500, 300)), 2.0);
        assert_eq!(tiles.len(), 6);
        assert_eq!(tiles[0], (Rectangle::from_size((400.0, 300.0).into()), Rectangle::from_size((200.0, 150.0).into())));
        assert_eq!(tiles[5], (Rectangle::from_size((200.0, 300.0).into()), Rectangle::new((400.0, 150.0).into(), (100.0, 150.0).into())));
        assert_eq!(place(WallpaperFit::Tile, Size::from((400, 300)), Size::from((500, 300)), 2.0), tiles[0]);
    }

    fn png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let mut png = Vec::new();
        let mut encoder = png::Encoder::new(&mut png, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.write_header().unwrap().write_image_data(rgba).unwrap();
        png
    }

    #[test]
    fn decodes_png_and_rejects_garbage() {
        let dir = std::env::temp_dir().join(format!("noobsbot-desktop-test-wallpaper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("w.png");
        std::fs::write(&file, png(2, 1, &[255, 0, 0, 0, 0, 255, 0, 128])).unwrap();
        let image = decode(&file, (0, 0).into()).unwrap();
        assert_eq!((image.size, image.shrunk), ((2, 1).into(), false));
        assert_eq!(image.rgbx, [255, 0, 0, 255, 0, 255, 0, 255]);
        // Twice the size an output needs: averaged down by 2, still covering it.
        std::fs::write(
            &file,
            png(4, 2, &[100, 0, 0, 255, 200, 0, 0, 255, 0, 10, 0, 255, 0, 30, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255]),
        )
        .unwrap();
        let image = decode(&file, (2, 1).into()).unwrap();
        assert_eq!((image.size, image.shrunk), ((2, 1).into(), true));
        assert_eq!(image.rgbx, [75, 0, 0, 255, 0, 10, 0, 255]);
        assert!(!decode(&file, (3, 1).into()).unwrap().shrunk, "a factor under 2 keeps it");

        std::fs::write(&file, png(MAX_SIDE as u32 + 1, 1, &vec![0; (MAX_SIDE + 1) * 4])).unwrap();
        assert!(decode(&file, (0, 0).into()).err().is_some_and(|e| e.contains("8192")));
        std::fs::write(&file, b"not a picture").unwrap();
        assert!(decode(&file, (0, 0).into()).is_err());
        assert!(decode(&dir.join("missing.jpg"), (0, 0).into()).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
