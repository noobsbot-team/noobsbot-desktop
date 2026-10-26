//! The app drawer: `panel` glass, radius 14, elevation 4. The search field across the top (its D7 ring while it has
//! the keys), the Noobsbot mark and the round session buttons in the left strip, the category sidebar (16 px Phosphor
//! symbols, the current one an accent pill, or `fill` while the list has the keys), a hairline, then the list: a
//! Headline title with the count, and 52 px rows of a 36 px icon, the name in Body and the description in Callout
//! `label-secondary`; the selected row is an accent pill with white text. Fades and scales in and out like popovers.

use std::{sync::Arc, time::Instant};

use smithay::{
    backend::renderer::{
        element::{Kind, memory::MemoryRenderBufferRenderElement},
        gles::{GlesRenderer, element::PixelShaderElement},
    },
    utils::Rectangle,
};

use super::{
    Material, SceneElement, Shaders,
    motion::Fading,
    titlebar::{Label, keep_buffers, label_element},
    ui::{Align, BODY, CALLOUT, HEADLINE, State, Ui},
};
use crate::{
    anim::Entrance,
    apps::Icons,
    shell::drawer::{Frame, SESSION, Target, layout},
    symbols::{Symbol, Symbols, Weight},
    text::{Rendered, Style, Text, ts},
    wm::Rect,
};

/// Drawn when an app's icon isn't in any theme (as in the dock).
const FALLBACK_ICON: &str = "application-x-executable";
const WHITE: [u8; 4] = [255; 4];

/// The frame, dark, accent, opacity, scale.
type Key = (Frame, bool, [u8; 3], u32, u64);
/// Glass front to back, text pieces (centred in their rects), app icons (stretched to theirs).
type Drawn = (Key, Vec<PixelShaderElement>, Vec<(Arc<Rendered>, Rect)>, Vec<(Arc<Rendered>, Rect)>);

#[derive(Default)]
pub struct DrawerView {
    cache: Option<Drawn>,
    labels: Vec<Label>,
    entrance: Option<Entrance>,
    fade: Fading,
    /// The display it was last drawn on, by name: its fade-out plays there.
    pub output: Option<String>,
}

impl DrawerView {
    /// Front to back: text and symbols, icons, glass. Rebuilt only when something in the frame or the theme changes.
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
        let key = (frame.clone(), dark, accent, glass_opacity.to_bits(), scale.to_bits());
        if self.cache.as_ref().is_none_or(|(k, ..)| *k != key) {
            let mut ui = Ui { shaders, text, symbols, dark, accent, glass_opacity, scale, glass: Vec::new(), pieces: Vec::new() };
            let app_icons = draw(&mut ui, icons, frame);
            let (mut glass, pieces) = (ui.glass, ui.pieces);
            glass.reverse();
            self.cache = Some((key, glass, pieces, app_icons));
        }
        let Some((_, glass, pieces, app_icons)) = &self.cache else { return Vec::new() };
        self.labels = keep_buffers(&self.labels, pieces.iter().chain(app_icons).map(|(p, _)| p.clone()));
        let (texts, images) = self.labels.split_at(pieces.len());
        let mut out: Vec<SceneElement> = texts.iter().zip(pieces).filter_map(|(label, (_, r))| label_element(renderer, label, *r, scale)).collect();
        for ((rendered, buffer), (_, rect)) in images.iter().zip(app_icons) {
            let Some(buffer) = buffer else { continue };
            let src = Rectangle::from_size((f64::from(rendered.width), f64::from(rendered.height)).into());
            let loc = rect.loc.to_f64().to_physical(scale);
            match MemoryRenderBufferRenderElement::from_buffer(renderer, loc, buffer, None, Some(src), Some(rect.size), Kind::Unspecified) {
                Ok(element) => out.push(SceneElement::Memory(element)),
                Err(err) => tracing::warn!("app drawer icon: {err}"),
            }
        }
        out.extend(glass.iter().cloned().map(SceneElement::Shader));
        out
    }

    /// The drawer's entrance while it opens (`bar[from..]` is the drawer), and its fade once it is gone.
    #[expect(clippy::too_many_arguments, reason = "the piece and the frame it is in")]
    pub fn motion(
        &mut self,
        renderer: &mut GlesRenderer,
        bar: &mut Vec<SceneElement>,
        from: usize,
        open: bool,
        reduce: bool,
        scale: f64,
        now: Instant,
        until: &mut Option<Instant>,
    ) {
        if !open {
            self.entrance = None;
        } else if self.entrance.is_none() {
            let entrance = Entrance { start: now, reduce, slide: false };
            crate::anim::extend(until, entrance.end());
            self.entrance = Some(entrance);
        }
        super::motion::appear(self.entrance, renderer, bar, from, scale, now);
        self.fade.track(renderer, bar, from, false, true, reduce, scale, now, until);
    }
}

fn inset(rect: Rect, dx: i32, dy: i32) -> Rect {
    Rectangle::new((rect.loc.x + dx, rect.loc.y + dy).into(), ((rect.size.w - 2 * dx).max(1), (rect.size.h - 2 * dy).max(1)).into())
}

/// Lays everything out into `ui`; returns the app icons.
fn draw(ui: &mut Ui, icons: &mut Icons, f: &Frame) -> Vec<(Arc<Rendered>, Rect)> {
    let l = &f.layout;
    let accent = ui.accent.map(|c| f32::from(c) / 255.0);
    let [ar, ag, ab] = accent;
    ui.material(l.rect, Material::Panel, Some(layout::RADIUS));
    let line = if ui.dark { [1.0, 1.0, 1.0, 0.10] } else { [0.0, 0.0, 0.0, 0.10] };

    // The strip: the mark on top, the session buttons at the bottom.
    let mark = ui.secondary();
    ui.symbol(Symbol::Logo, Weight::Regular, 22, inset(l.logo, 5, 5), mark);
    for (i, (rect, (symbol, tip))) in l.session.iter().zip(SESSION).enumerate() {
        let hot = f.hover == Some(Target::Session(i));
        ui.glass.push(ui.shaders.light(*rect, ui.fill(if hot { 3.0 } else { 1.5 }), false, ui.scale));
        let color = if hot { ui.label() } else { ui.secondary() };
        ui.symbol(symbol, Weight::Regular, 16, inset(*rect, 8, 8), color);
        if hot {
            ui.tooltip((rect.loc.x + rect.size.w + 8, rect.loc.y + 4).into(), tip);
        }
    }

    // The field: its fill and ring span the magnifier; the text starts after it.
    let focused = if f.sidebar { State::Normal } else { State::Focused };
    ui.shape(l.field, ui.fill(2.0), 8.0);
    let after = l.magnifier.loc.x + l.magnifier.size.w + 2;
    let text = Rectangle::new((after, l.field.loc.y).into(), (l.field.loc.x + l.field.size.w - after, l.field.size.h).into());
    ui.field_text(text, &f.field, "Search apps", focused, None);
    ui.states(l.field, 8.0, focused);
    let secondary = ui.secondary();
    ui.symbol(Symbol::MagnifyingGlass, Weight::Regular, 16, l.magnifier, secondary);

    // The sidebar.
    for (rect, &category) in l.categories.iter().zip(&f.categories) {
        let on = f.current == Some(category);
        let hot = f.hover == Some(Target::Category(f.categories.iter().position(|c| *c == category).unwrap_or(0)));
        let color = match (on, f.sidebar) {
            (true, true) => {
                ui.shape(*rect, [ar, ag, ab, 1.0], 7.0);
                WHITE
            }
            (true, false) => {
                ui.shape(*rect, ui.fill(2.0), 7.0);
                ui.label()
            }
            _ => {
                if hot {
                    ui.states(*rect, 7.0, State::Hover);
                }
                ui.label()
            }
        };
        let icon = Rectangle::new((rect.loc.x + 9, rect.loc.y + (rect.size.h - 16) / 2).into(), (16, 16).into());
        let symbol_color = if on && f.sidebar { WHITE } else { [ui.accent[0], ui.accent[1], ui.accent[2], 255] };
        ui.symbol(category.symbol(), if on { Weight::Fill } else { Weight::Regular }, 16, icon, symbol_color);
        let x = icon.loc.x + 16 + 9;
        ui.text(
            category.name(),
            Style { color, ..BODY },
            Rectangle::new((x, rect.loc.y).into(), (rect.loc.x + rect.size.w - 8 - x, rect.size.h).into()),
            Align::Left,
        );
    }
    ui.shape(l.divider, line, 0.0);

    // The list: its title and count, then the rows.
    let title = f.current.map_or("Results", |c| c.name());
    let header = inset(l.header, 8, 0);
    let count = if f.count == 1 { "1 app".to_owned() } else { format!("{} apps", f.count) };
    let count_w = if f.count > 0 { ui.text(&count, Style { color: secondary, ..CALLOUT }, header, Align::Right) } else { 0 };
    let label = ui.label();
    ui.text(title, Style { color: label, ..HEADLINE }, Rectangle::new(header.loc, ((header.size.w - count_w - 12).max(1), header.size.h).into()), Align::Left);
    let px = (f64::from(layout::ICON) * ui.scale).ceil() as u32;
    let mut app_icons = Vec::new();
    for (i, ((app, favorite), row)) in f.rows.iter().zip(&l.rows).enumerate() {
        let on = f.selected == Some(i);
        if on {
            ui.shape(*row, [ar, ag, ab, 1.0], 9.0);
        }
        let icon = Rectangle::new((row.loc.x + 10, row.loc.y + (row.size.h - layout::ICON) / 2).into(), (layout::ICON, layout::ICON).into());
        app_icons.push((icons.get(&app.icon, px).or_else(|| icons.get(FALLBACK_ICON, px)).unwrap_or_else(|| super::dock::placeholder(px)), icon));
        let (main, side) = if on { (WHITE, [255, 255, 255, 214]) } else { (ui.label(), ui.secondary()) };
        let x = icon.loc.x + layout::ICON + 12;
        let star = if *favorite && f.current != Some(crate::shell::drawer::Category::Favorites) { 20 } else { 0 };
        let w = row.loc.x + row.size.w - 12 - star - x;
        if app.comment.is_empty() {
            ui.text(&app.name, Style { color: main, ..BODY }, Rectangle::new((x, row.loc.y).into(), (w, row.size.h).into()), Align::Left);
        } else {
            let (name_h, comment_h) = (ts(16), ts(15));
            let top = row.loc.y + (row.size.h - name_h - 2 - comment_h) / 2;
            ui.text(&app.name, Style { color: main, ..BODY }, Rectangle::new((x, top).into(), (w, name_h).into()), Align::Left);
            ui.text(&app.comment, Style { color: side, ..CALLOUT }, Rectangle::new((x, top + name_h + 2).into(), (w, comment_h).into()), Align::Left);
        }
        if star > 0 {
            let at = Rectangle::new((row.loc.x + row.size.w - 12 - 12, row.loc.y + (row.size.h - 12) / 2).into(), (12, 12).into());
            let color = if on { WHITE } else { ui.tertiary() };
            ui.symbol(Symbol::Star, Weight::Fill, 12, at, color);
        }
    }
    if f.rows.is_empty() {
        let area = l.list();
        let tertiary = ui.tertiary();
        ui.text(
            f.empty,
            Style { color: tertiary, ..BODY },
            Rectangle::new((area.loc.x, area.loc.y + area.size.h / 2 - ts(20)).into(), (area.size.w, ts(20)).into()),
            Align::Centre,
        );
    }
    // A thin scroll thumb when the list is longer than the rows.
    let shown = l.rows.len();
    if f.count > shown && shown > 0 {
        let track = l.rows[0].loc.y;
        let height = l.rows[shown - 1].loc.y + l.rows[shown - 1].size.h - track;
        let thumb = (height * shown as i32 / f.count as i32).max(24);
        let y = track + (height - thumb) * f.first as i32 / (f.count - shown) as i32;
        let x = l.rect.loc.x + l.rect.size.w - 7;
        ui.shape(Rectangle::new((x, y).into(), (4, thumb).into()), ui.fill(4.0), 2.0);
    }
    app_icons
}
