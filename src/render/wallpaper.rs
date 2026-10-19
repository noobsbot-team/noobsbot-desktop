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

/// A decoded picture on its way from the worker thread to the GPU.
pub struct Decoded {
    path: PathBuf,
    image: Result<Image, String>,
}

/// 8-bit RGBX pixels.
pub(super) struct Image {
    pub(super) size: Size<i32, Buffer>,
    pub(super) rgbx: Vec<u8>,
}

pub struct Wallpaper {
    tx: Sender<Decoded>,
    /// The file asked for; `None` = the opal gradient.
    wanted: Option<PathBuf>,
    texture: Option<(TextureBuffer<GlesTexture>, Size<i32, Buffer>)>,
    opal: Vec<(Size<i32, Logical>, bool, PixelShaderElement)>,
}

/// Opal gradients kept, one per (output size, dark); the outputs of one desktop are a handful.
const MAX_OPALS: usize = 8;

impl Wallpaper {
    /// Decoded pictures come back through `tx`; hand each to `receive`.
    pub fn new(tx: Sender<Decoded>) -> Self {
        Self { tx, wanted: None, texture: None, opal: Vec::new() }
    }

    /// Picks the wallpaper for `appearance` (light or dark file). A new file is decoded on a worker thread;
    /// until it arrives, or if it fails, the opal gradient shows.
    pub fn set(&mut self, appearance: &Appearance) {
        let file = if is_dark(appearance) { &appearance.wallpaper_dark } else { &appearance.wallpaper };
        let wanted = (!file.is_empty()).then(|| expand_home(file));
        if wanted == self.wanted {
            return;
        }
        self.texture = None;
        self.wanted = wanted.clone();
        let Some(path) = wanted else { return };
        let tx = self.tx.clone();
        let spawned = std::thread::Builder::new().name("wallpaper-decode".into()).spawn(move || {
            let image = decode(&path);
            let _ = tx.send(Decoded { path, image });
        });
        if let Err(err) = spawned {
            tracing::warn!("wallpaper: {err}, using the built-in one");
        }
    }

    /// Uploads a decoded picture if it is still the one wanted; a failure is logged and the opal gradient stays.
    pub fn receive(&mut self, renderer: &mut GlesRenderer, decoded: Decoded) {
        if self.wanted.as_ref() != Some(&decoded.path) {
            return;
        }
        let uploaded = decoded.image.and_then(|image| {
            let texture = renderer.import_memory(&image.rgbx, Fourcc::Xbgr8888, image.size, false).map_err(|e| e.to_string())?;
            Ok((TextureBuffer::from_texture(renderer, texture, 1, Transform::Normal, Some(vec![Rectangle::from_size(image.size)])), image.size))
        });
        match uploaded {
            Ok(texture) => self.texture = Some(texture),
            Err(err) => tracing::warn!("wallpaper {}: {err}, using the built-in one", decoded.path.display()),
        }
    }

    /// The bottom element of an output of `size`: the picture scaled to cover it, else the opal gradient.
    pub fn element(&mut self, shaders: &Shaders, size: Size<i32, Logical>, dark: bool) -> SceneElement {
        if let Some((texture, image_size)) = &self.texture {
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

/// PNG by its signature, anything else as JPEG. Alpha is dropped: a wallpaper is opaque.
/// ponytail: the GPU scales the full-size picture every frame it is damaged (linear filter, no mipmaps); downscale
/// once per output size here if a huge photo shimmers or costs too much.
pub(super) fn decode(path: &Path) -> Result<Image, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let (width, height, channels, pixels) = if bytes.starts_with(b"\x89PNG") {
        let mut decoder = png::Decoder::new(Cursor::new(&bytes));
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
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
        let pixels = decoder.decode().map_err(|e| format!("{e:?}"))?;
        let (width, height) = decoder.dimensions().ok_or("no image size")?;
        (width, height, 4, pixels)
    };
    let size = Size::from((i32::try_from(width).map_err(|e| e.to_string())?, i32::try_from(height).map_err(|e| e.to_string())?));
    if pixels.len() < width * height * channels {
        return Err("truncated image".into());
    }
    let rgbx = pixels
        .chunks_exact(channels)
        .take(width * height)
        .flat_map(|p| match channels {
            1 | 2 => [p[0], p[0], p[0], 255],
            _ => [p[0], p[1], p[2], 255],
        })
        .collect();
    Ok(Image { size, rgbx })
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
    fn decodes_png_and_rejects_garbage() {
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 2, 1);
            encoder.set_color(png::ColorType::Rgba);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[255, 0, 0, 0, 0, 255, 0, 128]).unwrap();
        }
        let dir = std::env::temp_dir().join(format!("noobsbot-desktop-test-wallpaper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("w.png");
        std::fs::write(&file, &png).unwrap();
        let image = decode(&file).unwrap();
        assert_eq!(image.size, (2, 1).into());
        assert_eq!(image.rgbx, [255, 0, 0, 255, 0, 255, 0, 255]);

        std::fs::write(&file, b"not a picture").unwrap();
        assert!(decode(&file).is_err());
        assert!(decode(&dir.join("missing.jpg")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
