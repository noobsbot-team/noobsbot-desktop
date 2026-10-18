//! The emoji picker (E7.2): `popover` glass, radius 12, elevation 3; the search field with its magnifier, the Recent
//! row, a Caption header (the category left, the selected emoji's name right) over a grid of 24 px colour emoji in
//! 36 px cells, the selected cell an accent pill at 30%; the category tabs along the bottom, the current one on `fill`.

use smithay::{backend::renderer::gles::GlesRenderer, utils::Rectangle};

use super::{
    Material, SceneElement, Shaders,
    titlebar::{Label, keep_buffers, label_element},
    ui::{Align, BODY, CAPTION, State, Ui},
};
use crate::{
    shell::emoji::{Layout, Picker, TAB_ICONS, layout::COLS},
    symbols::{Symbol, Symbols, Weight},
    text::{Rendered, Style, Text},
    wm::Rect,
};

/// The picker, layout, dark, accent, opacity, scale.
type Key = (Picker, Layout, bool, [u8; 3], u32, u64);
/// Glass front to back, text pieces (centred in their rects).
type Drawn = (Key, Vec<smithay::backend::renderer::gles::element::PixelShaderElement>, Vec<(std::sync::Arc<Rendered>, Rect)>);

const EMOJI: Style = Style { size: 24, line_height: 30, weight: 400, color: [255; 4], tabular: false };

#[derive(Default)]
pub struct EmojiPicker {
    cache: Option<Drawn>,
    labels: Vec<Label>,
}

impl EmojiPicker {
    /// Front to back: text and emoji, glass. Rebuilt only when the picker, theme or size changes.
    #[expect(clippy::too_many_arguments, reason = "the frame's drawing context")]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        symbols: &mut Symbols,
        picker: &Picker,
        dark: bool,
        accent: [u8; 3],
        glass_opacity: f32,
        scale: f64,
    ) -> Vec<SceneElement> {
        let l = picker.layout();
        let key = (picker.clone(), l.clone(), dark, accent, glass_opacity.to_bits(), scale.to_bits());
        if self.cache.as_ref().is_none_or(|(k, ..)| *k != key) {
            let mut ui = Ui { shaders, text, symbols, dark, accent, glass_opacity, scale, glass: Vec::new(), pieces: Vec::new() };
            ui.material(l.rect, Material::Popover, Some(crate::shell::emoji::layout::RADIUS));
            let color = ui.secondary();
            // The field's fill and ring span the magnifier; its text starts after it.
            ui.shape(l.field, ui.fill(2.0), 6.0);
            let after = l.magnifier.loc.x + l.magnifier.size.w;
            ui.field_text(
                Rectangle::new((after, l.field.loc.y).into(), (l.field.loc.x + l.field.size.w - after, l.field.size.h).into()),
                &picker.field,
                "Search",
                State::Focused,
                None,
            );
            ui.states(l.field, 6.0, State::Focused);
            ui.symbol(Symbol::MagnifyingGlass, Weight::Regular, 14, l.magnifier, color);
            let [r, g, b] = accent.map(|c| f32::from(c) / 255.0);
            let (recent, grid) = picker.items();
            let cell = |ui: &mut Ui, rect: Rect, emoji: &str, on: bool| {
                if on {
                    ui.shape(Rectangle::new((rect.loc.x + 2, rect.loc.y + 2).into(), (rect.size.w - 4, rect.size.h - 4).into()), [r, g, b, 0.3], 8.0);
                }
                ui.text(emoji, EMOJI, rect, Align::Centre);
            };
            let header = Style { color, ..CAPTION };
            if let Some((title, row)) = &l.recent {
                ui.text("Recent", header, *title, Align::Left);
                for (i, (rect, emoji)) in row.iter().zip(recent).enumerate() {
                    cell(&mut ui, *rect, emoji, i == picker.selected);
                }
            }
            let (title, name) = picker.header();
            let name_w = ui.text(name, header, l.header, Align::Right);
            ui.text(title, header, Rectangle::new(l.header.loc, ((l.header.size.w - name_w - 12).max(1), l.header.size.h).into()), Align::Left);
            let first = picker.scroll * COLS;
            for (i, (rect, (emoji, _))) in l.grid.iter().zip(grid.iter().skip(first)).enumerate() {
                cell(&mut ui, *rect, emoji, recent.len() + first + i == picker.selected);
            }
            if grid.is_empty() {
                ui.text(
                    "No Emoji Found",
                    Style { color: ui.tertiary(), ..BODY },
                    l.grid.first().map_or(l.header, |c| Rectangle::new(c.loc, (l.header.size.w, 3 * c.size.h).into())),
                    Align::Centre,
                );
            }
            // The tabs, the current one on `fill`; the others faded.
            let searching = !picker.field.text.trim().is_empty();
            for (i, (rect, icon)) in l.tabs.iter().zip(TAB_ICONS).enumerate() {
                let on = i == picker.tab && !searching;
                if on {
                    ui.shape(Rectangle::new((rect.loc.x + 3, rect.loc.y + 2).into(), (rect.size.w - 6, rect.size.h - 4).into()), ui.fill(2.0), 7.0);
                }
                let style = Style { size: 17, line_height: 22, color: [255, 255, 255, if on { 255 } else { 150 }], ..EMOJI };
                ui.text(icon, style, *rect, Align::Centre);
            }
            let (mut glass, pieces) = (ui.glass, ui.pieces);
            glass.reverse();
            self.cache = Some((key, glass, pieces));
        }
        let Some((_, glass, pieces)) = &self.cache else { return Vec::new() };
        self.labels = keep_buffers(&self.labels, pieces.iter().map(|(p, _)| p.clone()));
        let mut out: Vec<SceneElement> = self.labels.iter().zip(pieces).filter_map(|(label, (_, r))| label_element(renderer, label, *r, scale)).collect();
        out.extend(glass.iter().cloned().map(SceneElement::Shader));
        out
    }
}
