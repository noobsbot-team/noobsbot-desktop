//! Launchpad (E4.4): the wallpaper dimmed and desaturated (no blur) under a grid of 80 px app icons with Callout
//! names, a `panel` search pill on top and page dots above the dock. On open the grid zooms 1.1 → 1 and fades in.

use std::sync::Arc;

use smithay::{
    backend::renderer::{
        element::{Kind, memory::MemoryRenderBufferRenderElement},
        gles::{GlesRenderer, element::PixelShaderElement},
    },
    utils::{Logical, Point, Rectangle, Size},
};

use super::{
    Material, SceneElement, Shaders,
    titlebar::{Label, keep_buffers},
    ui::{self, Ui},
};
use crate::{
    apps::Icons,
    shell::launchpad::{Folder, Grid, Tile, pages},
    symbols::{Symbol, Symbols, Weight},
    text::{Rendered, Style, Text},
    ui::field::TextField,
    wm::Rect,
};

/// Drawn when an app's icon isn't in any theme.
const FALLBACK_ICON: &str = "application-x-executable";
/// The dim: a dark grey over the wallpaper at 60%. Mixing with grey takes out 60% of the colour too (desaturates),
/// and keeps white names above 4.5:1 on the brightest wallpaper (D2).
const DIM: [f32; 4] = [0.08, 0.08, 0.08, 0.6];
const WHITE: [u8; 4] = [255, 255, 255, 255];
const CALLOUT: Style = Style { color: WHITE, ..ui::CALLOUT };

/// What one frame shows.
pub struct Frame<'a> {
    pub tiles: &'a [Tile<'a>],
    pub grid: &'a Grid,
    pub output: Size<i32, Logical>,
    /// Page position in view (1.5: halfway from the second page to the third).
    pub position: f64,
    pub page: usize,
    /// 0 → 1 as it opens.
    pub shown: f64,
    pub field: &'a TextField,
    /// The tile being dragged (in an open folder: and its app being dragged), and where its icon is.
    pub lifted: Option<(usize, Option<usize>, Rect)>,
    /// How far each tile is drawn off its cell (sliding aside); may be empty.
    pub offsets: &'a [Point<i32, Logical>],
    /// The tile a dragged icon is held over: dropping makes a folder.
    pub onto: Option<usize>,
    /// The open folder: its panel, the grid in it, the folder and how far it has faded in.
    pub folder: Option<(Rect, &'a Grid, &'a Folder, f64)>,
}

/// Size, search field, pages, page, shown, caret x, selection width, dark, opacity, accent, scale.
type Key = (Size<i32, Logical>, Rect, usize, usize, u64, i32, i32, bool, u32, [u8; 3], u64);

/// A picture, the rect it fills or is centred in, whether it is stretched to the rect, its alpha.
type Piece = (Arc<Rendered>, Rect, bool, f32);
/// A glass shape: rect, fill, radius, effects.
type Shape = (Rect, [f32; 4], f64, bool);

#[derive(Default)]
pub struct LaunchpadView {
    glass: Option<(Key, Vec<PixelShaderElement>)>,
    /// Shapes that move with the pages (folder tiles), rebuilt only when one changes.
    moving: Option<(Vec<Shape>, Vec<PixelShaderElement>)>,
    /// In front of the grid: a lifted folder's glass and the open folder's caret, panel and dim.
    front: Option<(Vec<Shape>, Vec<PixelShaderElement>)>,
    labels: Vec<Label>,
}

/// `rect` scaled by `s` about `centre`.
fn zoom(rect: Rect, centre: Point<f64, Logical>, s: f64) -> Rect {
    let at = |v: f64, c: f64| (c + (v - c) * s).round() as i32;
    let (x, y) = (at(f64::from(rect.loc.x), centre.x), at(f64::from(rect.loc.y), centre.y));
    Rectangle::new((x, y).into(), ((f64::from(rect.size.w) * s).round() as i32, (f64::from(rect.size.h) * s).round() as i32).into())
}

impl LaunchpadView {
    /// Front to back: icons and names, the field's text, caret and pill, the dots, the dim.
    #[expect(clippy::too_many_arguments, reason = "the frame's drawing context")]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        symbols: &mut Symbols,
        icons: &mut Icons,
        frame: &Frame,
        dark: bool,
        accent: [u8; 3],
        glass_opacity: f32,
        scale: f64,
    ) -> Vec<SceneElement> {
        let Frame { tiles, grid, output, position, page, shown, field, lifted, offsets, onto, folder } = *frame;
        let per_page = grid.per_page();
        let count = pages(tiles.len(), per_page);
        let spec = Material::Panel.spec(dark);
        let [tr, tg, tb] = spec.tint.map(|c| f32::from(c) / 255.0);
        let tint = |a: f32| [tr, tg, tb, spec.opacity * glass_opacity * a];
        let logical = |px: u32| (f64::from(px) / scale).round() as i32;
        let alpha = shown.clamp(0.0, 1.0) as f32;
        let centre = Point::from((f64::from(output.w) / 2.0, f64::from(output.h) / 2.0));
        let s = 1.1 - 0.1 * shown;

        // (piece, rect it fills or is centred in, stretch to the rect, alpha)
        let mut pieces: Vec<Piece> = Vec::new();
        let px = (f64::from(grid.icon) * scale).ceil() as u32;
        let mini_px = (f64::from(mini_grid(grid.icon_rect(0))[0].size.w) * scale).ceil() as u32;
        let mut moving: Vec<Shape> = Vec::new();
        // In front of the grid: the lifted icon, a little bigger, then the open folder.
        let (mut top, mut front): (Vec<Piece>, Vec<Shape>) = (Vec::new(), Vec::new());
        if let Some((tile, inner, rect)) = lifted.and_then(|(i, j, r)| Some((tiles.get(i)?, j, r))) {
            let big = zoom(rect, rect.loc.to_f64() + rect.size.to_f64().downscale(2.0).to_point(), 1.1);
            match (tile, inner) {
                (Tile::Folder(_, apps), Some(j)) => {
                    if let Some(app) = apps.get(j) {
                        top.push((icon_image(icons, &app.icon, px), big, true, alpha * 0.9));
                    }
                }
                _ => {
                    art(icons, tile, big, (px, mini_px), alpha * 0.9, tint(alpha), &mut top, &mut front);
                }
            }
        }
        // The folder app being dragged leaves its place empty.
        let dragged_in = lifted.and_then(|(_, j, _)| j);
        let lifted = lifted.filter(|(_, j, _)| j.is_none()).map(|(i, ..)| i);
        if let Some((panel, inner, open, faded)) = folder
            && let Some(Tile::Folder(_, apps)) = tiles.get(open.index)
        {
            let (name, alpha) = (&open.name, alpha * faded.clamp(0.0, 1.0) as f32);
            let (per, first) = (inner.per_page(), open.page * inner.per_page());
            for (j, app) in apps.iter().enumerate().skip(first).take(per).filter(|(j, _)| dragged_in != Some(*j)) {
                let k = j - first;
                top.push((icon_image(icons, &app.icon, px), inner.icon_rect(k), true, alpha));
                top.push((text.label(&app.name, CALLOUT, inner.name_rect(k).size.w as u32, scale), inner.name_rect(k), false, alpha));
            }
            let count = pages(apps.len(), per);
            if count > 1 {
                for (i, dot) in inner.dots(count).into_iter().enumerate() {
                    // On the panel's glass: ink of the panel's text colour.
                    let (c, a) = (if dark { 1.0 } else { 0.0 }, if i == open.page { 0.85 } else { 0.3 });
                    front.push((dot, [c, c, c, a * alpha], f64::from(dot.size.w) / 2.0, false));
                }
            }
            // The name, centred; once clicked, with the caret or the selection.
            let style = Style { size: 17, line_height: 22, weight: 600, ..CALLOUT };
            let f = inner.field;
            let label = text.label(&name.text, style, f.size.w as u32, scale);
            let w = logical(label.width).min(f.size.w);
            let x = f.loc.x + (f.size.w - w) / 2;
            top.push((label, Rectangle::new((x, f.loc.y).into(), (w, f.size.h).into()), false, alpha));
            let [ar, ag, ab] = accent.map(|c| f32::from(c) / 255.0);
            if open.editing && name.all {
                front.push((Rectangle::new((x - 1, f.loc.y + 3).into(), (w + 2, f.size.h - 6).into()), [ar, ag, ab, 0.3 * alpha], 2.0, false));
            } else if open.editing {
                let caret = x + logical(text.label(&name.text[..name.caret], style, 4000, scale).width).min(w);
                front.push((Rectangle::new((caret, f.loc.y + 4).into(), (1, f.size.h - 8).into()), [ar, ag, ab, alpha], 0.0, false));
            }
            front.push((panel, tint(alpha), spec.radius, true));
            let [r, g, b, a] = DIM;
            front.push((Rectangle::from_size(output), [r, g, b, a * alpha], 0.0, false));
        }
        let first = position.floor().max(0.0) as usize;
        let last = (position.ceil().max(0.0) as usize).min(count - 1);
        for p in first..=last {
            let dx = ((p as f64 - position) * f64::from(output.w)).round() as i32;
            for (i, tile) in tiles.iter().enumerate().skip(p * per_page).take(per_page).filter(|(i, _)| lifted != Some(*i)) {
                let off = offsets.get(i).copied().unwrap_or_default();
                let i = i % per_page;
                let shift = |r: Rect| zoom(Rectangle::new((r.loc.x + dx + off.x, r.loc.y + off.y).into(), r.size), centre, s);
                let mut icon = shift(grid.icon_rect(i));
                if icon.loc.x + icon.size.w < 0 || icon.loc.x > output.w {
                    continue;
                }
                // A folder target grows a little over a glass well.
                if onto == Some(p * per_page + i) {
                    let mid = icon.loc.to_f64() + icon.size.to_f64().downscale(2.0).to_point();
                    let well = zoom(icon, mid, 1.25);
                    moving.push((well, tint(alpha), f64::from(well.size.w) * 0.225, true));
                    icon = zoom(icon, mid, 1.1);
                }
                let name = art(icons, tile, icon, (px, mini_px), alpha, tint(alpha), &mut pieces, &mut moving);
                let name_rect = shift(grid.name_rect(i));
                let name = text.label(name, CALLOUT, grid.name_rect(i).size.w as u32, scale);
                pieces.push((name, name_rect, false, alpha));
            }
        }

        // The search pill: magnifier and "Search" centred while empty, else the magnifier left and the text after it.
        // D2 ink from a `Ui` made for the purpose (this view draws without one).
        let (label, secondary, tertiary) = {
            let ui = Ui { shaders, text: &mut *text, symbols: &mut *symbols, dark, accent, glass_opacity, scale, glass: Vec::new(), pieces: Vec::new() };
            (ui.label(), ui.secondary(), ui.tertiary())
        };
        let body = Style { color: label, ..ui::BODY };
        let f = grid.field;
        let glyph = symbols.get(Symbol::MagnifyingGlass, Weight::Regular, 14, scale, secondary);
        let shown_text = field.display();
        let (words, style) = if shown_text.is_empty() { ("Search", Style { color: tertiary, ..body }) } else { (shown_text.as_str(), body) };
        let room = f.size.w - 2 * 10 - 14 - 6;
        let label = text.label(words, style, room as u32, scale);
        let w = logical(label.width).min(room);
        let glyph_x = if shown_text.is_empty() { f.loc.x + (f.size.w - 14 - 6 - w) / 2 } else { f.loc.x + 10 };
        let text_x = glyph_x + 14 + 6;
        pieces.push((glyph, Rectangle::new((glyph_x, f.loc.y + (f.size.h - 14) / 2).into(), (14, 14).into()), false, alpha));
        pieces.push((label, Rectangle::new((text_x, f.loc.y).into(), (w, f.size.h).into()), false, alpha));
        let caret_x =
            if shown_text.is_empty() { text_x } else { text_x + logical(text.label(&shown_text[..field.display_caret()], body, 4000, scale).width).min(room) };
        let selected = if field.all { w } else { 0 };

        let key = (output, f, count, page, shown.to_bits(), caret_x, selected, dark, glass_opacity.to_bits(), accent, scale.to_bits());
        if self.glass.as_ref().is_none_or(|(k, _)| *k != key) {
            let [ar, ag, ab] = accent.map(|c| f32::from(c) / 255.0);
            let mut glass = Vec::new();
            // Caret (the field always has focus), or the Ctrl+A selection.
            if selected > 0 {
                glass.push(shaders.glass_shape(
                    Rectangle::new((text_x - 1, f.loc.y + 5).into(), (selected + 2, f.size.h - 10).into()),
                    [ar, ag, ab, 0.3 * alpha],
                    2.0,
                    false,
                    dark,
                    scale,
                ));
            } else {
                glass.push(shaders.glass_shape(
                    Rectangle::new((caret_x, f.loc.y + 6).into(), (1, f.size.h - 12).into()),
                    [ar, ag, ab, alpha],
                    0.0,
                    false,
                    dark,
                    scale,
                ));
            }
            glass.push(shaders.glass_shape(f, tint(alpha), f64::from(f.size.h) / 2.0, true, dark, scale));
            if count > 1 {
                for (i, dot) in grid.dots(count).into_iter().enumerate() {
                    let a = if i == page { 0.9 } else { 0.35 };
                    glass.push(shaders.light(dot, [1.0, 1.0, 1.0, a * alpha], false, scale));
                }
            }
            let [r, g, b, a] = DIM;
            glass.push(shaders.glass_shape(Rectangle::from_size(output), [r, g, b, a * alpha], 0.0, false, dark, scale));
            self.glass = Some((key, glass));
        }

        let in_front = top.len();
        top.append(&mut pieces);
        let pieces = top;
        self.labels = keep_buffers(&self.labels, pieces.iter().map(|(p, ..)| p.clone()));
        let mut out = Vec::new();
        for (n, ((rendered, buffer), (_, rect, stretch, a))) in self.labels.iter().zip(&pieces).enumerate() {
            if n == in_front {
                out.extend(shapes(&mut self.front, front.clone(), shaders, dark, scale).iter().cloned().map(SceneElement::Shader));
            }
            let Some(buffer) = buffer else { continue };
            let (size, src) = if *stretch {
                (rect.size, Some(Rectangle::from_size((f64::from(rendered.width), f64::from(rendered.height)).into())))
            } else {
                (Size::from((logical(rendered.width), logical(rendered.height))), None)
            };
            let loc = Point::from((rect.loc.x + (rect.size.w - size.w) / 2, rect.loc.y + (rect.size.h - size.h) / 2)).to_f64().to_physical(scale);
            match MemoryRenderBufferRenderElement::from_buffer(renderer, loc, buffer, Some(*a), src, Some(size), Kind::Unspecified) {
                Ok(element) => out.push(SceneElement::Memory(element)),
                Err(err) => tracing::warn!("launchpad: {err}"),
            }
        }
        if in_front == pieces.len() {
            out.extend(shapes(&mut self.front, front, shaders, dark, scale).iter().cloned().map(SceneElement::Shader));
        }
        out.extend(shapes(&mut self.moving, moving, shaders, dark, scale).iter().cloned().map(SceneElement::Shader));
        out.extend(self.glass.iter().flat_map(|(_, g)| g).cloned().map(SceneElement::Shader));
        out
    }
}

/// The elements for `list`, kept in `cache` (and their damage ids with them) while it doesn't change.
fn shapes<'a>(
    cache: &'a mut Option<(Vec<Shape>, Vec<PixelShaderElement>)>,
    list: Vec<Shape>,
    shaders: &Shaders,
    dark: bool,
    scale: f64,
) -> &'a [PixelShaderElement] {
    if cache.as_ref().is_none_or(|(k, _)| *k != list) {
        let built = list.iter().map(|&(rect, fill, radius, effects)| shaders.glass_shape(rect, fill, radius, effects, dark, scale)).collect();
        *cache = Some((list, built));
    }
    cache.as_ref().map_or(&[], |(_, built)| built)
}

/// A tile's icon in `rect`: the app's, or a folder's glass with its first nine apps; returns its name.
#[expect(clippy::too_many_arguments, reason = "the frame's drawing context")]
fn art<'a>(
    icons: &mut Icons,
    tile: &'a Tile,
    rect: Rect,
    (px, mini_px): (u32, u32),
    alpha: f32,
    tint: [f32; 4],
    pieces: &mut Vec<Piece>,
    moving: &mut Vec<Shape>,
) -> &'a str {
    match tile {
        Tile::App(app) => {
            pieces.push((icon_image(icons, &app.icon, px), rect, true, alpha));
            &app.name
        }
        Tile::Folder(name, apps) => {
            moving.push((rect, tint, f64::from(rect.size.w) * 0.225, true));
            for (cell, app) in mini_grid(rect).into_iter().zip(apps) {
                pieces.push((icon_image(icons, &app.icon, mini_px), cell, true, alpha));
            }
            name
        }
    }
}

fn icon_image(icons: &mut Icons, name: &str, px: u32) -> Arc<Rendered> {
    icons.get(name, px).or_else(|| icons.get(FALLBACK_ICON, px)).unwrap_or_else(|| super::dock::placeholder(px))
}

/// A folder tile's 3 × 3 mini icons, in reading order.
fn mini_grid(tile: Rect) -> Vec<Rect> {
    let pad = tile.size.w / 8;
    let step = (tile.size.w - 2 * pad) / 3;
    let icon = step * 5 / 6;
    let at = |n: i32, o: i32| o + pad + n * step + (step - icon) / 2;
    (0..9).map(|i| Rectangle::new((at(i % 3, tile.loc.x), at(i / 3, tile.loc.y)).into(), (icon, icon).into())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zooms_about_the_centre() {
        let r = Rectangle::new((100, 100).into(), (80, 80).into());
        assert_eq!(zoom(r, (0.0, 0.0).into(), 1.0), r);
        assert_eq!(zoom(r, (0.0, 0.0).into(), 1.1), Rectangle::new((110, 110).into(), (88, 88).into()));
        let c = Rectangle::new((-40, -40).into(), (80, 80).into());
        assert_eq!(zoom(c, (0.0, 0.0).into(), 1.1), Rectangle::new((-44, -44).into(), (88, 88).into()), "a centred rect stays centred");
    }

    #[test]
    fn mini_icons_fit_the_tile() {
        let tile = Rectangle::new((100, 100).into(), (80, 80).into());
        let cells = mini_grid(tile);
        assert!(cells.iter().all(|c| tile.contains_rect(*c)));
        assert!(!cells[0].overlaps(cells[1]) && !cells[0].overlaps(cells[3]));
    }
}
