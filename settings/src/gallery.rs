//! Appearance ▸ Wallpaper gallery (E8.4): the PNG and JPEG pictures in the usual wallpaper folders, as small
//! thumbnails decoded once when the page opens. The desktop draws only PNG and JPEG, so nothing else is offered.

use std::path::{Path, PathBuf};

use iced::widget::image::Handle;

/// Thumbnail size (16:10), drawn at half size for 2× screens.
pub const W: u32 = 192;
pub const H: u32 = 120;
/// Pictures offered; a big Pictures folder must not stall the page.
const MAX: usize = 48;
/// A picture longer than this a side is skipped rather than decoded into hundreds of MB (as the desktop does).
const MAX_SIDE: usize = 8192;

/// The config key of the folders added with "+ Add Folder…", and the text field key of that button's path field.
pub const FOLDERS: &str = "appearance.gallery_folders";
pub const ADD: &str = "gallery.add";

/// `folders` (`:`-separated) with `path` added; `~/` is the home folder.
pub fn add(folders: &str, path: &str) -> String {
    let path = match path.trim().strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
        None => path.trim().to_owned(),
    };
    folders.split(':').map(str::trim).filter(|f| !f.is_empty() && *f != path).chain([path.as_str()]).collect::<Vec<_>>().join(":")
}

/// The folders searched: the ones added with "+ Add Folder…" (`:`-separated), then the system's, then the user's.
fn folders(added: &str) -> Vec<PathBuf> {
    let mut all: Vec<PathBuf> = added.split(':').filter(|f| !f.trim().is_empty()).map(|f| PathBuf::from(f.trim())).collect();
    all.extend([PathBuf::from("/usr/share/backgrounds"), PathBuf::from("/usr/share/wallpapers")]);
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        all.extend([home.join("Pictures/Wallpapers"), home.join(".local/share/wallpapers"), home.join("Pictures")]);
    }
    all
}

fn is_picture(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg"))
}

/// Up to `MAX` pictures, a few folders deep, in a stable order.
pub fn find(added: &str) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for path in entries {
            if out.len() >= MAX {
                return;
            }
            if path.is_dir() && depth > 0 {
                walk(&path, depth - 1, out);
            } else if is_picture(&path) && !out.contains(&path) {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    for folder in folders(added) {
        walk(&folder, 4, &mut out);
    }
    out
}

/// Every picture with its thumbnail; ones that don't decode are left out.
pub async fn load(added: String) -> Vec<(PathBuf, Handle)> {
    let paths = find(&added);
    std::thread::scope(|s| {
        let jobs: Vec<_> = paths.into_iter().map(|path| s.spawn(move || thumbnail(&path).map(|rgba| (path, Handle::from_rgba(W, H, rgba))))).collect();
        jobs.into_iter().filter_map(|j| j.join().ok().flatten()).collect()
    })
}

/// `W`×`H` RGBA covering the picture, centred (as the desktop's "fill").
fn thumbnail(path: &Path) -> Option<Vec<u8>> {
    cover(path, W, H)
}

/// `w`×`h` RGBA covering the picture at `path`, centred.
pub fn cover(path: &Path, w: u32, h: u32) -> Option<Vec<u8>> {
    let (width, height, rgba) = decode(path)?;
    let scale = (f64::from(w) / width as f64).max(f64::from(h) / height as f64);
    let (x0, y0) = ((width as f64 - f64::from(w) / scale) / 2.0, (height as f64 - f64::from(h) / scale) / 2.0);
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let sx = ((x0 + (f64::from(x) + 0.5) / scale) as usize).min(width - 1);
            let sy = ((y0 + (f64::from(y) + 0.5) / scale) as usize).min(height - 1);
            let i = (sy * width + sx) * 4;
            out.extend_from_slice(&rgba[i..i + 4]);
        }
    }
    Some(out)
}

/// PNG by its signature, anything else as JPEG, to 8-bit RGBA.
fn decode(path: &Path) -> Option<(usize, usize, Vec<u8>)> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.starts_with(b"\x89PNG") {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(&bytes));
        decoder.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
        let mut reader = decoder.read_info().ok()?;
        let (w, h) = (reader.info().width as usize, reader.info().height as usize);
        if w > MAX_SIDE || h > MAX_SIDE {
            return None;
        }
        let mut pixels = vec![0; reader.output_buffer_size()?];
        let info = reader.next_frame(&mut pixels).ok()?;
        let rgba = match info.color_type {
            png::ColorType::Rgba => pixels,
            png::ColorType::GrayscaleAlpha => pixels.as_chunks::<2>().0.iter().flat_map(|&[g, a]| [g, g, g, a]).collect(),
            _ => return None,
        };
        (rgba.len() >= w * h * 4).then_some((w, h, rgba))
    } else {
        use zune_jpeg::zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
        let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
        let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(&bytes), options);
        decoder.decode_headers().ok()?;
        let (w, h) = decoder.dimensions()?;
        if w > MAX_SIDE || h > MAX_SIDE {
            return None;
        }
        let rgba = decoder.decode().ok()?;
        (rgba.len() >= w * h * 4).then_some((w, h, rgba))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn added_folders_join_once() {
        assert_eq!(add("", "/a"), "/a");
        assert_eq!(add("/a: /b", " /a "), "/b:/a");
    }

    #[test]
    fn png_thumbnails_cover_the_tile() {
        let dir = std::env::temp_dir().join(format!("noobsbot-settings-gallery-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("w.png");
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, 4, 2);
        encoder.set_color(png::ColorType::Rgb);
        // Left half red, right half blue.
        encoder.write_header().unwrap().write_image_data(&[255, 0, 0, 255, 0, 0, 0, 0, 255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 0, 0, 255, 0, 0, 255]).unwrap();
        std::fs::write(&file, bytes).unwrap();
        let thumb = thumbnail(&file).unwrap();
        assert_eq!(thumb.len(), (W * H * 4) as usize);
        assert_eq!(&thumb[..4], &[255, 0, 0, 255]);
        assert_eq!(&thumb[thumb.len() - 4..], &[0, 0, 255, 255]);
        assert!(is_picture(&file) && !is_picture(Path::new("a.jxl")));
        // An added folder comes first.
        assert_eq!(find(&format!(" :{}", dir.display())).first(), Some(&file));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
