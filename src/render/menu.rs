//! Dropdown menus (D8): the logo menu and the app menu (E2.2). `popover` glass, radius 8, padding 5, 22 px rows
//! (grown with the text size); the hovered row is an accent pill with white text, disabled rows are at 40%.

use smithay::{
    backend::renderer::gles::{GlesRenderer, element::PixelShaderElement},
    utils::{Logical, Point, Rectangle, Size},
};

use super::{
    Material, SceneElement, Shaders,
    titlebar::{Label, keep_buffers, label_element},
};
use crate::{
    shell::{self, Row, menu::*},
    text::{Style, Text, ts},
    wm::Rect,
};

const RADIUS: f64 = 8.0;
/// Between a label and its shortcut.
const SHORTCUT_GAP: i32 = 24;

/// What the card, separators and shadow depend on: rect, separator rows, dark, opacity, scale.
type Key = (Rect, Vec<Rect>, bool, u32, u64);
/// The hovered row, accent and scale.
type HoverKey = (Rect, [u8; 3], u64);

#[derive(Default)]
pub struct MenuPopover {
    glass: Option<(Key, Vec<PixelShaderElement>)>,
    hover: Option<(HoverKey, PixelShaderElement)>,
    labels: Vec<Label>,
    /// The menu and its rows as last drawn, output-relative.
    layout: Option<(Rect, Vec<Rect>)>,
    /// Rows scrolled off the top of a menu taller than the screen, the wheel's remainder under a row, and the
    /// (anchor, row count) they belong to: another menu starts at the top again.
    scroll: (usize, f64, Option<(shell::Anchor, usize)>),
}

impl MenuPopover {
    pub fn rect(&self) -> Option<Rect> {
        self.layout.as_ref().map(|(rect, _)| *rect)
    }

    /// Row `i`'s rect as last drawn.
    pub fn cell(&self, i: usize) -> Option<Rect> {
        self.layout.as_ref()?.1.get(i).copied()
    }

    /// The row (separators count) under `location`.
    pub fn row_at(&self, location: Point<f64, Logical>) -> Option<usize> {
        self.layout.as_ref()?.1.iter().position(|r| r.to_f64().contains(location))
    }

    /// The wheel over the menu: `dy` logical px. `true`: the rows shown changed.
    pub fn scroll_by(&mut self, dy: f64) -> bool {
        let Some((_, cells)) = &self.layout else { return false };
        // Hidden rows have no height: those at the top are the scroll, those at the bottom what is left to scroll.
        let above = cells.iter().take_while(|c| c.size.h == 0).count();
        let below = cells.iter().rev().take_while(|c| c.size.h == 0).count();
        self.scroll.1 += dy;
        let rows = (self.scroll.1 / f64::from(ts(ROW))).trunc();
        self.scroll.1 -= rows * f64::from(ts(ROW));
        let scroll = (above as f64 + rows).clamp(0.0, (above + below) as f64) as usize;
        self.scroll.0 = scroll;
        scroll != above
    }

    /// Front to back: labels, the hover pill, the card with its separators, its shadow. `hover` is the pointer,
    /// output-relative; row `open` (its submenu is open) stays lit while the pointer is elsewhere.
    #[allow(clippy::too_many_arguments)]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        rows: &[Option<Row>],
        anchor: shell::Anchor,
        output: Size<i32, Logical>,
        hover: Point<f64, Logical>,
        open: Option<usize>,
        dark: bool,
        accent: [u8; 3],
        glass_opacity: f32,
        scale: f64,
    ) -> Vec<SceneElement> {
        // label, label-secondary (Part D); disabled = label at 40%.
        let ink = |alpha: u8| if dark { [255, 255, 255, alpha] } else { [0, 0, 0, alpha] };
        let (label, secondary, disabled) = (ink(217), ink(if dark { 140 } else { 128 }), ink(87));
        let body = Style { size: 13, line_height: 16, weight: 400, color: label, tabular: false };
        let logical = |w: u32| (f64::from(w) / scale).round() as i32;

        // Sized to the widest label plus its shortcut.
        let mut width = ts(MIN_WIDTH);
        let inset = ts(SIDE);
        // A submenu row shows a chevron where a shortcut would be.
        let side = |row: &Row| if row.sub.is_empty() { row.shortcut } else { "›" };
        let live = |row: &Row| row.action.is_some() || !row.sub.is_empty();
        for row in rows.iter().flatten() {
            let shortcut = if side(row).is_empty() { 0 } else { ts(SHORTCUT_GAP) + logical(text.label(side(row), body, 200, scale).width) };
            width = width.max(logical(text.label(&row.label, body, 400, scale).width) + shortcut + 2 * (inset + PAD));
        }
        if self.scroll.2 != Some((anchor, rows.len())) {
            self.scroll = (0, 0.0, Some((anchor, rows.len())));
        }
        let (rect, cells) = shell::menu_layout(anchor, output, width, rows, self.scroll.0);
        let hovered = cells.iter().position(|c| c.to_f64().contains(hover)).filter(|&i| rows[i].as_ref().is_some_and(live)).or(open);

        let separators: Vec<Rect> = rows
            .iter()
            .zip(&cells)
            .filter(|(row, c)| row.is_none() && c.size.h > 0)
            .map(|(_, c)| Rectangle::new((c.loc.x + inset - PAD, c.loc.y + SEPARATOR / 2).into(), (c.size.w - 2 * (inset - PAD), 1).into()))
            .collect();
        let key = (rect, separators.clone(), dark, glass_opacity.to_bits(), scale.to_bits());
        if self.glass.as_ref().is_none_or(|(k, _)| *k != key) {
            let spec = Material::Popover.spec(dark);
            let [r, g, b] = spec.tint.map(|c| f32::from(c) / 255.0);
            let line = if dark { [1.0, 1.0, 1.0, 0.10] } else { [0.0, 0.0, 0.0, 0.10] };
            let mut glass: Vec<_> = separators.iter().map(|s| shaders.glass_shape(*s, line, 0.0, false, dark, scale)).collect();
            glass.push(shaders.glass_shape(rect, [r, g, b, spec.opacity * glass_opacity], RADIUS, true, dark, scale));
            glass.extend(shaders.shadow(rect, RADIUS, spec.elevation, dark, scale, false));
            self.glass = Some((key, glass));
        }
        match hovered.map(|i| cells[i]) {
            Some(cell) => {
                let key = (cell, accent, scale.to_bits());
                if self.hover.as_ref().is_none_or(|(k, _)| *k != key) {
                    let [r, g, b] = accent.map(|c| f32::from(c) / 255.0);
                    self.hover = Some((key, shaders.glass_shape(cell, [r, g, b, 1.0], 4.0, false, dark, scale)));
                }
            }
            None => self.hover = None,
        }

        let white = [255, 255, 255, 255];
        let mut pieces = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            let Some(row) = row else { continue };
            let (main, second) = match (Some(i) == hovered, live(row)) {
                (true, _) => (white, white),
                (false, true) => (label, secondary),
                (false, false) => (disabled, disabled),
            };
            pieces.push(text.label(&row.label, Style { color: main, ..body }, 400, scale));
            pieces.push(text.label(side(row), Style { color: second, ..body }, 200, scale));
        }
        self.labels = keep_buffers(&self.labels, pieces);

        // Labels left-aligned after the side padding, shortcuts right-aligned before it.
        let places = rows.iter().zip(&cells).filter(|(row, _)| row.is_some()).flat_map(|(_, c)| [(c, false), (c, true)]);
        let mut out: Vec<SceneElement> = self
            .labels
            .iter()
            .zip(places)
            .filter_map(|(piece, (cell, right))| {
                if cell.size.h == 0 {
                    return None;
                }
                let w = logical(piece.0.width);
                let x = if right { cell.loc.x + cell.size.w - (inset - PAD) - w } else { cell.loc.x + inset - PAD };
                label_element(renderer, piece, Rectangle::new((x, cell.loc.y).into(), (w, cell.size.h).into()), scale)
            })
            .collect();
        out.extend(self.hover.as_ref().map(|(_, e)| SceneElement::Shader(e.clone())));
        if let Some((_, glass)) = &self.glass {
            out.extend(glass.iter().cloned().map(SceneElement::Shader));
        }
        self.layout = Some((rect, cells));
        out
    }
}
