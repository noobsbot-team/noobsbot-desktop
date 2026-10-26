//! The screenshot overlay (E6.8): outside the selection dimmed to black @ 35%, a 1 px white edge and a `hud` pill
//! with its size; in window mode the window under the pointer tinted accent @ 25%. The toolbar is a `hud` strip
//! (radius 12) above the dock: ✕ · Screen / Window / Region (44 × 36 symbol buttons, the chosen one on `fill` @ 3×)
//! · Record · Options ⌄ and the default Capture button; Options opens a D8 menu above it. The floating thumbnail
//! sits 16 px from the bottom-right corner, 200 px wide, with a level-3 shadow.

use std::sync::Arc;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
            },
            gles::{GlesRenderer, element::PixelShaderElement},
        },
    },
    utils::{Logical, Rectangle, Size, Transform},
};

use super::{
    Material, SceneElement, Shaders,
    titlebar::{Label, keep_buffers, label_element},
    ui::{Align, BODY, CALLOUT, RED, State, Ui},
};
use crate::{
    shell::screenshot::{self, Hit, Mode, OPTION_ROWS, Shot, Thumb},
    symbols::{Symbol, Symbols, Weight},
    text::{Rendered, Style, Text, ts},
    wm::Rect,
};

/// What the toolbar can show besides the shot itself: can record, is recording, timer seconds, thumbnail off.
pub type Flags = (bool, bool, u8, bool);
/// Shot, flags, output size, dark, accent, opacity, scale, text scale.
type Key = (Shot, Flags, Size<i32, Logical>, bool, [u8; 3], u32, u64, u32);
type Drawn = (Key, Vec<PixelShaderElement>, Vec<(Arc<Rendered>, Rect)>);
/// The image, the frame it was drawn for (rect, dark, scale), its buffer and its edge + shadow.
type Thumbnail = (Arc<[u8]>, (Rect, bool, u64), MemoryRenderBuffer, Vec<PixelShaderElement>);

#[derive(Default)]
pub struct ScreenshotView {
    cache: Option<Drawn>,
    labels: Vec<Label>,
    /// The thumbnail's texture and frame, rebuilt for each new screenshot.
    thumb: Option<Thumbnail>,
}

/// D7 state of a toolbar part.
fn state(shot: &Shot, hit: Hit, enabled: bool) -> State {
    match (enabled, shot.hover == Some(hit)) {
        (false, _) => State::Disabled,
        (true, true) => State::Hover,
        (true, false) => State::Normal,
    }
}

impl ScreenshotView {
    /// Front to back: toolbar text, toolbar and menu glass, then the selection and the dimming.
    #[expect(clippy::too_many_arguments, reason = "the frame's drawing context")]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        symbols: &mut Symbols,
        shot: &Shot,
        flags: Flags,
        output: Size<i32, Logical>,
        dark: bool,
        accent: [u8; 3],
        glass_opacity: f32,
        scale: f64,
    ) -> Vec<SceneElement> {
        let key = (shot.clone(), flags, output, dark, accent, glass_opacity.to_bits(), scale.to_bits(), crate::text::factor().to_bits());
        if self.cache.as_ref().is_none_or(|(k, ..)| *k != key) {
            let mut ui = Ui { shaders, text, symbols, dark, accent, glass_opacity, scale, glass: Vec::new(), pieces: Vec::new() };
            draw_selection(&mut ui, shot, output);
            if shot.toolbar {
                draw_toolbar(&mut ui, shot, flags, output);
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

    /// The floating thumbnail in `rect` at `alpha` (dragged, it is see-through): the image, then its edge and shadow.
    #[expect(clippy::too_many_arguments, reason = "the frame's drawing context")]
    pub fn thumbnail(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        thumb: &Thumb,
        rect: Rect,
        alpha: f32,
        dark: bool,
        scale: f64,
    ) -> Vec<SceneElement> {
        let frame = (rect, dark, scale.to_bits());
        if self.thumb.as_ref().is_none_or(|(rgba, f, ..)| !Arc::ptr_eq(rgba, &thumb.rgba) || *f != frame) {
            let (w, h) = thumb.size;
            let buffer = MemoryRenderBuffer::from_slice(&thumb.rgba, Fourcc::Abgr8888, (w as i32, h as i32), 1, Transform::Normal, None);
            let edge = if dark { [1.0, 1.0, 1.0, 0.25] } else { [0.0, 0.0, 0.0, 0.15] };
            let mut glass = vec![shaders.ring(rect, 0.0, 1, edge, scale)];
            glass.extend(shaders.shadow(rect, 0.0, 3, dark, scale, false));
            self.thumb = Some((thumb.rgba.clone(), frame, buffer, glass));
        }
        let Some((_, _, buffer, glass)) = &self.thumb else { return Vec::new() };
        let loc = rect.loc.to_f64().to_physical(scale);
        let mut out: Vec<SceneElement> = glass[..1].iter().cloned().map(SceneElement::Shader).collect();
        // The whole picture scaled into `rect` (see `label_element`).
        let src = Rectangle::from_size((f64::from(thumb.size.0), f64::from(thumb.size.1)).into());
        match MemoryRenderBufferRenderElement::from_buffer(renderer, loc, buffer, Some(alpha), Some(src), Some(rect.size), Kind::Unspecified) {
            Ok(image) => out.push(SceneElement::Memory(image)),
            Err(err) => tracing::warn!("screenshot thumbnail: {err}"),
        }
        out.extend(glass[1..].iter().cloned().map(SceneElement::Shader));
        out
    }

    /// Nothing shown: the next thumbnail starts fresh.
    pub fn hide_thumbnail(&mut self) {
        self.thumb = None;
    }
}

/// The selection (dragged, or kept by the toolbar) or the window under the pointer, over the dimmed screen.
fn draw_selection(ui: &mut Ui, shot: &Shot, output: Size<i32, Logical>) {
    let dim = [0.0, 0.0, 0.0, 0.35];
    let selection = shot.drag.map(|(a, b)| screenshot::region(a, b)).or(shot.region).filter(|_| shot.mode == Mode::Region);
    match (shot.mode, selection) {
        (Mode::Region, Some(sel)) => {
            let (x0, y0, x1, y1) = (sel.loc.x, sel.loc.y, sel.loc.x + sel.size.w, sel.loc.y + sel.size.h);
            // Above, below, left, right of the selection.
            for (x, y, w, h) in [(0, 0, output.w, y0), (0, y1, output.w, output.h - y1), (0, y0, x0, sel.size.h), (x1, y0, output.w - x1, sel.size.h)] {
                if w > 0 && h > 0 {
                    ui.shape(Rectangle::new((x, y).into(), (w, h).into()), dim, 0.0);
                }
            }
            ui.glass.push(ui.shaders.ring(sel, 0.0, 1, [1.0, 1.0, 1.0, 0.9], ui.scale));
            size_label(ui, sel, output);
        }
        // The toolbar's region mode before a drag: the whole screen dimmed.
        (Mode::Region, None) if shot.toolbar => ui.shape(Rectangle::from_size(output), dim, 0.0),
        (Mode::Window, _) => {
            if let Some(window) = shot.window {
                let [r, g, b] = ui.accent.map(|c| f32::from(c) / 255.0);
                ui.shape(window, [r, g, b, 0.25], super::WINDOW_RADIUS);
            }
        }
        _ => {}
    }
}

/// "640 × 400" in a `hud` pill under the selection's bottom-right corner (inside it when there's no room below).
fn size_label(ui: &mut Ui, sel: Rect, output: Size<i32, Logical>) {
    let words = format!("{} × {}", sel.size.w, sel.size.h);
    let style = Style { color: ui.label(), tabular: true, ..CALLOUT };
    let piece = ui.text.label(&words, style, 200, ui.scale);
    let w = (f64::from(piece.width) / ui.scale).round() as i32 + 16;
    let h = ts(22);
    let below = sel.loc.y + sel.size.h + 8;
    let y = if below + h <= output.h { below } else { sel.loc.y + sel.size.h - h - 8 };
    let x = (sel.loc.x + sel.size.w - w).clamp(0, (output.w - w).max(0));
    let pill = Rectangle::new((x, y).into(), (w, h).into());
    ui.material(pill, Material::Hud, Some(f64::from(h) / 2.0));
    ui.pieces.push((piece, pill));
}

fn draw_toolbar(ui: &mut Ui, shot: &Shot, (can_record, recording, delay, no_thumbnail): Flags, output: Size<i32, Logical>) {
    let bar = screenshot::toolbar(output);
    ui.material(bar.rect, Material::Hud, Some(screenshot::RADIUS));
    let line = if ui.dark { [1.0, 1.0, 1.0, 0.10] } else { [0.0, 0.0, 0.0, 0.10] };
    for divider in bar.dividers {
        ui.shape(divider, line, 0.0);
    }
    // ✕: a `fill` circle.
    let close = bar.close;
    let close_state = state(shot, Hit::Close, true);
    ui.shape(close, ui.fill(2.0), f64::from(close.size.w) / 2.0);
    ui.states(close, f64::from(close.size.w) / 2.0, close_state);
    let color = ui.secondary();
    ui.symbol(Symbol::X, Weight::Regular, 14, close, color);

    let modes = [(Mode::Screen, Symbol::Monitor), (Mode::Window, Symbol::AppWindow), (Mode::Region, Symbol::Selection)];
    for ((mode, symbol), rect) in modes.into_iter().zip(bar.modes) {
        let on = shot.mode == mode;
        if on {
            ui.shape(rect, ui.fill(3.0), 6.0);
        }
        ui.states(rect, 6.0, state(shot, Hit::Mode(mode), true));
        let color = ui.label();
        ui.symbol(symbol, Weight::Regular, 22, rect, color);
    }

    let record = state(shot, Hit::Record, can_record);
    ui.states(bar.record, 6.0, record);
    let (symbol, color) = if recording { (Symbol::Stop, RED) } else { (Symbol::Record, ui.label()) };
    let color = if record == State::Disabled { ui.tertiary() } else { color };
    ui.symbol(symbol, if recording { Weight::Fill } else { Weight::Regular }, 22, bar.record, color);

    // Options ⌄: text and a caret, no fill until hovered (or open).
    let options = bar.options;
    ui.states(options, 6.0, if shot.options { State::Pressed } else { state(shot, Hit::Options, true) });
    let words = Rectangle::new((options.loc.x + 10, options.loc.y).into(), (options.size.w - 30, options.size.h).into());
    let label = ui.label();
    ui.text("Options", Style { color: label, ..BODY }, words, Align::Left);
    let caret = Rectangle::new((options.loc.x + options.size.w - 20, options.loc.y + (options.size.h - 10) / 2).into(), (10, 10).into());
    ui.symbol(Symbol::CaretDown, Weight::Regular, 10, caret, label);

    let ready = match shot.mode {
        Mode::Screen => true,
        Mode::Window => shot.window.is_some(),
        Mode::Region => shot.region.is_some(),
    };
    ui.button(bar.capture, "Capture", true, state(shot, Hit::Capture, ready));

    if shot.options {
        let (menu, _) = screenshot::options_menu(&bar, output);
        let hovered = match shot.hover {
            Some(Hit::Option(i)) => Some(i),
            _ => None,
        };
        let ticked = |i: usize| match i {
            0 => delay == 0,
            1 => delay == 5,
            2 => delay == 10,
            _ => !no_thumbnail,
        };
        let items: Vec<_> = OPTION_ROWS.iter().enumerate().map(|(i, r)| r.map(|label| (label, if ticked(i) { "✓" } else { "" }, true, false))).collect();
        ui.menu(menu.loc, menu.size.w, &items, hovered);
    }
}
