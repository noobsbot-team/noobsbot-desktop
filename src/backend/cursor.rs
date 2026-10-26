//! Named pointer images from the XCursor theme, for the real screen (nested, the host draws the pointer).

use std::path::PathBuf;

/// One cursor picture: premultiplied ARGB8888, little-endian (B, G, R, A bytes), as DRM's `Argb8888`.
pub struct Image {
    pub size: (u32, u32),
    pub hotspot: (u32, u32),
    pub pixels: Vec<u8>,
}

const IMAGE: u32 = 0xfffd_0002;

/// The picture nearest `size` px in an XCursor file; the first frame of an animated one.
pub fn parse(data: &[u8], size: u32) -> Option<Image> {
    let at = |i: usize| data.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    if data.get(..4)? != b"Xcur" {
        return None;
    }
    // Table of contents after the 16-byte header: (type, nominal size, position) per chunk.
    let mut best: Option<(u32, usize)> = None;
    for entry in (0..at(12)? as usize).map(|i| 16 + i * 12) {
        let nominal = at(entry + 4)?;
        if at(entry)? == IMAGE && best.is_none_or(|(n, _)| nominal.abs_diff(size) < n.abs_diff(size)) {
            best = Some((nominal, at(entry + 8)? as usize));
        }
    }
    // An image chunk: header, type, size, version, width, height, x hot, y hot, delay, then the pixels.
    let (_, pos) = best?;
    let (w, h) = (at(pos + 16)?, at(pos + 20)?);
    if w == 0 || h == 0 || w > 1024 || h > 1024 {
        return None;
    }
    let pixels = data.get(pos + 36..pos + 36 + (w * h * 4) as usize)?.to_vec();
    Some(Image { size: (w, h), hotspot: (at(pos + 24)?, at(pos + 28)?), pixels })
}

/// The first of `names` the theme has, at about `size` px: `$XCURSOR_THEME` (else "default"), the themes it
/// inherits, then Adwaita.
pub fn load(names: &[&str], size: u32) -> Option<Image> {
    let dirs = icon_dirs();
    let mut themes = vec![std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".into())];
    let mut i = 0;
    while let Some(theme) = themes.get(i).cloned().filter(|_| i < 8) {
        i += 1;
        for dir in dirs.iter().map(|d| d.join(&theme)) {
            let found = names.iter().find_map(|name| std::fs::read(dir.join("cursors").join(name)).ok().and_then(|data| parse(&data, size)));
            if found.is_some() {
                return found;
            }
            let index = std::fs::read_to_string(dir.join("index.theme")).unwrap_or_default();
            themes.extend(inherits(&index).filter(|parent| !themes.contains(parent)).collect::<Vec<_>>());
        }
        if i == themes.len() && !themes.iter().any(|t| t == "Adwaita") {
            themes.push("Adwaita".into());
        }
    }
    None
}

/// The pointer size in logical px: `configured` (`[appearance] pointer_size`), else `$XCURSOR_SIZE`, else 24.
pub fn size(configured: u32) -> u32 {
    Some(configured)
        .filter(|s| *s > 0)
        .or_else(|| std::env::var("XCURSOR_SIZE").ok().and_then(|s| s.parse().ok()))
        .filter(|s| (8..=256).contains(s))
        .unwrap_or(24)
}

/// The `Inherits=` themes of an `index.theme`.
fn inherits(index: &str) -> impl Iterator<Item = String> + '_ {
    index
        .lines()
        .find_map(|l| l.trim().strip_prefix("Inherits="))
        .into_iter()
        .flat_map(|v| v.split([',', ';']))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(String::from)
}

/// `$XCURSOR_PATH`, else the usual icon folders.
fn icon_dirs() -> Vec<PathBuf> {
    if let Some(path) = std::env::var_os("XCURSOR_PATH") {
        return std::env::split_paths(&path).collect();
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let data = std::env::var_os("XDG_DATA_HOME").map_or_else(|| home.join(".local/share"), PathBuf::from);
    vec![data.join("icons"), home.join(".icons"), "/usr/share/icons".into(), "/usr/share/pixmaps".into()]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An XCursor file with one 2×1 image per nominal size.
    fn file(sizes: &[u32]) -> Vec<u8> {
        let words = |v: &[u32]| v.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<u8>>();
        let mut out = b"Xcur".to_vec();
        out.extend(words(&[16, 0x1_0000, sizes.len() as u32]));
        let toc_end = 16 + sizes.len() * 12;
        for (i, &size) in sizes.iter().enumerate() {
            out.extend(words(&[IMAGE, size, (toc_end + i * 44) as u32]));
        }
        for &size in sizes {
            out.extend(words(&[36, IMAGE, size, 1, 2, 1, size, 1, 0, 0xff00_00ff, size]));
        }
        out
    }

    #[test]
    fn picks_the_nearest_size() {
        let data = file(&[24, 48, 32]);
        let image = parse(&data, 36).unwrap();
        assert_eq!((image.size, image.hotspot), ((2, 1), (32, 1)), "32 is nearer 36 than 48");
        assert_eq!(&image.pixels[..4], &[0xff, 0, 0, 0xff]);
        assert_eq!(parse(&data, 100).unwrap().hotspot.0, 48);
        assert!(parse(b"nope", 24).is_none());
        assert!(parse(&data[..data.len() - 1], 32).is_none(), "truncated: no image, no panic");
    }

    #[test]
    fn reads_inherited_themes() {
        let index = "[Icon Theme]\nName=Default\nInherits=breeze_cursors, Adwaita;\n";
        assert_eq!(inherits(index).collect::<Vec<_>>(), ["breeze_cursors", "Adwaita"]);
        assert_eq!(inherits("").count(), 0);
    }
}
