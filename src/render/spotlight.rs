//! Spotlight (E4.3): `panel` glass, radius 14, elevation 5; the magnifier and the 52 px field (Title 1), then 40 px
//! rows with a 32 px icon, the name in Body and the kind in Callout `label-secondary` on the right; the selected row
//! is an accent pill with white text.

use std::sync::Arc;

use smithay::{
    backend::renderer::{
        element::{Kind, memory::MemoryRenderBufferRenderElement},
        gles::{GlesRenderer, element::PixelShaderElement},
    },
    utils::Rectangle,
};

use super::{
    Material, SceneElement, Shaders,
    titlebar::{Label, keep_buffers, label_element},
    ui::{Align, BODY, CALLOUT, State, Ui},
};
use crate::{
    apps::Icons,
    shell::spotlight::{Hit, Layout, layout},
    symbols::{Symbol, Symbols, Weight},
    text::{Rendered, Style, Text},
    ui::field::TextField,
    wm::Rect,
};

/// Drawn when an app's icon isn't in any theme (as in the dock).
const FALLBACK_ICON: &str = "application-x-executable";

/// Hits, selection, field, layout, dark, accent, opacity, scale.
type Key = (Vec<Hit>, usize, TextField, Layout, bool, [u8; 3], u32, u64);
/// Glass front to back, text pieces (centred in their rects), app and file icons (stretched to theirs).
type Drawn = (Key, Vec<PixelShaderElement>, Vec<(Arc<Rendered>, Rect)>, Vec<(Arc<Rendered>, Rect)>);

#[derive(Default)]
pub struct SpotlightPanel {
    cache: Option<Drawn>,
    labels: Vec<Label>,
}

impl SpotlightPanel {
    /// Front to back: text and symbols, icons, glass. Rebuilt only when the query, selection, theme or size changes.
    #[expect(clippy::too_many_arguments, reason = "the frame's drawing context")]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        symbols: &mut Symbols,
        icons: &mut Icons,
        hits: &[Hit],
        selected: usize,
        field: &TextField,
        l: &Layout,
        dark: bool,
        accent: [u8; 3],
        glass_opacity: f32,
        scale: f64,
    ) -> Vec<SceneElement> {
        let key = (hits.to_vec(), selected, field.clone(), l.clone(), dark, accent, glass_opacity.to_bits(), scale.to_bits());
        if self.cache.as_ref().is_none_or(|(k, ..)| *k != key) {
            let mut ui = Ui { shaders, text, symbols, dark, accent, glass_opacity, scale, glass: Vec::new(), pieces: Vec::new() };
            let spec = Material::Panel.spec(dark);
            ui.glass.extend(shaders.shadow(l.rect, layout::RADIUS, 5, dark, scale, false));
            let [r, g, b] = spec.tint.map(|c| f32::from(c) / 255.0);
            ui.glass.push(shaders.glass_shape(l.rect, [r, g, b, spec.opacity * glass_opacity], layout::RADIUS, true, dark, scale));
            let color = ui.secondary();
            ui.symbol(Symbol::MagnifyingGlass, Weight::Regular, layout::MAGNIFIER as u16, l.magnifier, color);
            ui.field(l.field, field, "Spotlight Search", State::Focused);

            let px = (f64::from(layout::ICON) * scale).ceil() as u32;
            let mut app_icons = Vec::new();
            for (i, (hit, row)) in hits.iter().zip(&l.rows).enumerate() {
                let on = i == selected;
                if on {
                    let [r, g, b] = accent.map(|c| f32::from(c) / 255.0);
                    ui.shape(*row, [r, g, b, 1.0], 6.0);
                }
                let icon = Rectangle::new((row.loc.x + 8, row.loc.y + (row.size.h - layout::ICON) / 2).into(), (layout::ICON, layout::ICON).into());
                let (main, side) = if on { ([255; 4], [255, 255, 255, 204]) } else { (ui.label(), ui.secondary()) };
                match hit {
                    Hit::App(app) => app_icons
                        .push((icons.get(&app.icon, px).or_else(|| icons.get(FALLBACK_ICON, px)).unwrap_or_else(|| super::dock::placeholder(px)), icon)),
                    Hit::Answer(_) => ui.symbol(Symbol::Calculator, Weight::Regular, 24, icon, main),
                    Hit::File(file) => app_icons.push((
                        icons
                            .get(&file.icon, px)
                            .or_else(|| icons.get(crate::shell::desktop::FALLBACK_ICON, px))
                            .unwrap_or_else(|| super::dock::placeholder(px)),
                        icon,
                    )),
                    // The page's symbol on its colour, as in the Settings sidebar.
                    Hit::Setting(setting) => {
                        let [_, r, g, b] = setting.color.to_be_bytes().map(|c| f32::from(c) / 255.0);
                        ui.shape(icon, [r, g, b, 1.0], 8.0);
                        ui.symbol(setting.symbol, Weight::Fill, 20, icon, [255; 4]);
                    }
                }
                let x = icon.loc.x + layout::ICON + 8;
                let inner = Rectangle::new((x, row.loc.y).into(), (row.loc.x + row.size.w - 12 - x, row.size.h).into());
                let kind_w = ui.text(hit.kind(), Style { color: side, ..CALLOUT }, inner, Align::Right);
                let title = Rectangle::new(inner.loc, (inner.size.w - kind_w - 16, inner.size.h).into());
                ui.text(&hit.title(), Style { color: main, ..BODY }, title, Align::Left);
            }
            let (mut glass, pieces) = (ui.glass, ui.pieces);
            glass.reverse();
            self.cache = Some((key, glass, pieces, app_icons));
        }
        let Some((_, glass, pieces, app_icons)) = &self.cache else { return Vec::new() };
        self.labels = keep_buffers(&self.labels, pieces.iter().chain(app_icons).map(|(p, _)| p.clone()));
        let (texts, images) = self.labels.split_at(pieces.len());
        let mut out: Vec<SceneElement> = texts.iter().zip(pieces).filter_map(|(label, (_, r))| label_element(renderer, label, *r, scale)).collect();
        // Icons fill their 32 px square whatever size the theme's file is (PNGs come at their own).
        for ((rendered, buffer), (_, rect)) in images.iter().zip(app_icons) {
            let Some(buffer) = buffer else { continue };
            let src = Rectangle::from_size((f64::from(rendered.width), f64::from(rendered.height)).into());
            let loc = rect.loc.to_f64().to_physical(scale);
            match MemoryRenderBufferRenderElement::from_buffer(renderer, loc, buffer, None, Some(src), Some(rect.size), Kind::Unspecified) {
                Ok(element) => out.push(SceneElement::Memory(element)),
                Err(err) => tracing::warn!("spotlight icon: {err}"),
            }
        }
        out.extend(glass.iter().cloned().map(SceneElement::Shader));
        out
    }
}
