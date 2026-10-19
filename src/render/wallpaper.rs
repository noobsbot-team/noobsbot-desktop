//! The wallpaper (E1.5, D11): a PNG/JPEG decoded once on a worker thread, uploaded once, drawn to cover the output;
//! the built-in opal gradient when no file is set or the file can't be used.

use std::{
    io::Cursor,
    path::{Path, PathBuf},
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

use super::{SceneElement, Shaders, is_dark};
use crate::config::Appearance;

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

pub struct Wallpaper {
    tx: Sender<Decoded>,
    /// The file asked for; `None` = the opal gradient.
    wanted: Option<PathBuf>,
    /// The largest output seen, in physical px: the picture is decoded no bigger than needed to cover it (C20).
    limit: Size<i32, Buffer>,
    /// The limit the latest decode was asked with; `None` = nothing asked yet.
    asked: Option<Size<i32, Buffer>>,
    texture: Option<(TextureBuffer<GlesTexture>, Size<i32, Buffer>, bool)>,
    opal: Vec<(Size<i32, Logical>, bool, PixelShaderElement)>,
}

impl Wallpaper {
    /// Decoded pictures come back through `tx`; hand each to `receive`.
    pub fn new(tx: Sender<Decoded>) -> Self {
        Self { tx, wanted: None, limit: Size::default(), asked: None, texture: None, opal: Vec::new() }
    }

    /// Picks the wallpaper for `appearance` (light or dark file). A new file is decoded on a worker thread once an
    /// output's size is known; until it arrives, or if it fails, the opal gradient shows.
    pub fn set(&mut self, appearance: &Appearance) {
        let file = if is_dark(appearance) { &appearance.wallpaper_dark } else { &appearance.wallpaper };
        let wanted = (!file.is_empty()).then(|| expand_home(file));
        if wanted == self.wanted {
            return;
        }
        self.texture = None;
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

    /// Uploads a decoded picture if it is still the one wanted; a failure is logged and the opal gradient stays.
    pub fn receive(&mut self, renderer: &mut GlesRenderer, decoded: Decoded) {
        if self.wanted.as_ref() != Some(&decoded.path) || self.asked != Some(decoded.limit) {
            return;
        }
        let uploaded = decoded.image.and_then(|image| {
            let texture = renderer.import_memory(&image.rgbx, Fourcc::Xbgr8888, image.size, false).map_err(|e| e.to_string())?;
            Ok((TextureBuffer::from_texture(renderer, texture, 1, Transform::Normal, Some(vec![Rectangle::from_size(image.size)])), image.size, image.shrunk))
        });
        match uploaded {
            Ok(texture) => self.texture = Some(texture),
            Err(err) => tracing::warn!("wallpaper {}: {err}, using the built-in one", decoded.path.display()),
        }
    }

    /// The bottom element of an output of `size` at `scale`: the picture scaled to cover it, else the opal gradient.
    pub fn element(&mut self, shaders: &Shaders, size: Size<i32, Logical>, scale: f64, dark: bool) -> SceneElement {
        let physical = size.to_f64().to_physical(scale).to_i32_round::<i32>();
        if physical.w > self.limit.w || physical.h > self.limit.h {
            self.limit = (physical.w.max(self.limit.w), physical.h.max(self.limit.h)).into();
            // A picture shrunk for a smaller output would be upscaled on this one: decode it again at the new limit.
            if self.texture.as_ref().is_none_or(|(_, _, shrunk)| *shrunk) {
                self.decode();
            }
        }
        if let Some((texture, image_size, _)) = &self.texture {
            let src = cover(*image_size, size);
            return SceneElement::Texture(TextureRenderElement::from_texture_buffer((0.0, 0.0), texture, None, Some(src), Some(size), Kind::Unspecified));
        }
        // One gradient per output size (C10): resizing a shared one every frame redrew every output every frame.
        if let Some((_, _, opal)) = self.opal.iter().find(|(s, d, _)| (*s, *d) == (size, dark)) {
            return SceneElement::Shader(opal.clone());
        }
        if self.opal.len() >= MAX_OPALS {
            self.opal.clear();
        }
        let opal = shaders.opal(Rectangle::from_size(size), dark);
        self.opal.push((size, dark, opal.clone()));
        SceneElement::Shader(opal)
    }
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
