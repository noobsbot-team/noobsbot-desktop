//! Control Center (E2.3): a `panel` glass column under the bar at the top right with `card` modules: Wi-Fi and
//! Bluetooth toggles, Do Not Disturb, Dark Mode, Display and Sound sliders, Now Playing, battery and power mode.

use smithay::{
    backend::renderer::gles::{GlesRenderer, element::PixelShaderElement},
    utils::Rectangle,
};

use super::{
    Material, SceneElement, Shaders,
    titlebar::{Label, keep_buffers, label_element},
};
use crate::{
    shell::control::{self, LIST_ROW, Layout, Module, Status, TOGGLE, media_buttons, profile_name, slider_fill, tile_toggle, toggle_in, track_in},
    symbols::{Symbol, Symbols, Weight},
    text::{Rendered, Style, Text},
    wm::Rect,
};

/// What the glass depends on: the layout's rect, the status, dark, accent, opacity, scale.
type Key = (Rect, Status, bool, [u8; 3], u32, u64);

#[derive(Default)]
pub struct ControlCenter {
    glass: Option<(Key, Vec<PixelShaderElement>)>,
    labels: Vec<Label>,
}

fn volume_symbol((volume, muted): (u8, bool)) -> Symbol {
    match volume {
        _ if muted => Symbol::SpeakerMuted,
        0 => Symbol::SpeakerMuted,
        1..34 => Symbol::SpeakerNone,
        34..67 => Symbol::SpeakerLow,
        _ => Symbol::SpeakerHigh,
    }
}

fn battery_symbol((percent, charging): (u8, bool)) -> Symbol {
    match percent {
        _ if charging => Symbol::BatteryCharging,
        88.. => Symbol::BatteryFull,
        63.. => Symbol::BatteryHigh,
        38.. => Symbol::BatteryMedium,
        13.. => Symbol::BatteryLow,
        _ => Symbol::BatteryEmpty,
    }
}

/// From `x` to `right` at `y`, `h` tall; at least 1 px wide.
fn row_rect(x: i32, y: i32, right: i32, h: i32) -> Rect {
    Rectangle::new((x, y).into(), ((right - x).max(1), h).into())
}

/// The round icon behind a Bluetooth device: accent when connected.
const DEVICE: i32 = 22;

/// A Bluetooth device's symbol, from its name.
fn device_symbol(name: &str) -> Symbol {
    let name = name.to_lowercase();
    if ["airpods", "buds", "tws", "headphone", "headset", "wh-", "wf-"].iter().any(|k| name.contains(k)) {
        Symbol::Headphones
    } else if name.contains("keyboard") {
        Symbol::Keyboard
    } else {
        Symbol::Bluetooth
    }
}

/// A sound output's symbol, from its name.
fn output_symbol(name: &str) -> Symbol {
    let name = name.to_lowercase();
    if name.contains("headphone") || name.contains("headset") {
        Symbol::Headphones
    } else if name.contains("hdmi") || name.contains("displayport") {
        Symbol::Monitor
    } else {
        Symbol::Speaker
    }
}

/// A `size` square centred in `rect`.
fn centred(rect: Rect, size: i32) -> Rect {
    Rectangle::new((rect.loc.x + (rect.size.w - size) / 2, rect.loc.y + (rect.size.h - size) / 2).into(), (size, size).into())
}

impl ControlCenter {
    /// Front to back: text and glyphs, toggles and sliders, the cards, the panel, its shadow.
    #[allow(clippy::too_many_arguments)]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        symbols: &mut Symbols,
        status: &Status,
        output_width: i32,
        dark: bool,
        accent: [u8; 3],
        glass_opacity: f32,
        scale: f64,
    ) -> Vec<SceneElement> {
        let layout = control::layout(output_width, status);
        let key = (layout.rect, status.clone(), dark, accent, glass_opacity.to_bits(), scale.to_bits());
        if self.glass.as_ref().is_none_or(|(k, _)| *k != key) {
            self.glass = Some((key, glass(shaders, &layout, status, dark, accent, glass_opacity, scale)));
        }

        // label, label-secondary (Part D).
        let ink = |alpha: u8| if dark { [255, 255, 255, alpha] } else { [0, 0, 0, alpha] };
        let (label, secondary) = (ink(217), ink(if dark { 140 } else { 128 }));
        let white = [255, 255, 255, 255];
        // On the slider fill: white in light mode (the fill is dark), dark in dark mode.
        let on_fill = if dark { [0, 0, 0, 191] } else { white };
        let title = Style { size: 13, line_height: 16, weight: 600, color: label, tabular: false };
        let body = Style { weight: 400, ..title };
        let small = Style { size: 11, line_height: 14, weight: 400, color: secondary, tabular: false };

        // (piece, rect, left-aligned); a left-aligned piece's rect shrinks to its width.
        let mut pieces: Vec<(std::sync::Arc<Rendered>, Rect, bool)> = Vec::new();
        let mut put = |piece, rect, left| pieces.push((piece, rect, left));
        let row = row_rect;

        let unavailable = (false, "Unavailable".to_owned());
        for (i, (symbol, name, (on, sub))) in
            [(Symbol::WifiHigh, "Wi-Fi", status.wifi.clone()), (Symbol::Bluetooth, "Bluetooth", status.bluetooth.clone())].into_iter().enumerate().map(|(i, (s, n, v))| (i, (s, n, v.unwrap_or_else(|| unavailable.clone()))))
        {
            let toggle = toggle_in(layout.network, i as i32);
            put(symbols.get(symbol, Weight::Fill, 16, scale, if on { white } else { label }), centred(toggle, 16), false);
            let x = toggle.loc.x + TOGGLE + 10;
            let right = layout.network.loc.x + layout.network.size.w - 10;
            put(text.label(name, title, (right - x) as u32, scale), row(x, toggle.loc.y - 1, right, 16), true);
            put(text.label(&sub, small, (right - x) as u32, scale), row(x, toggle.loc.y + 15, right, 14), true);
        }
        for (card, symbol, on, lines) in [(layout.dnd, Symbol::Moon, status.dnd, &["Do Not", "Disturb"][..]), (layout.dark, Symbol::CircleHalf, status.dark, &["Dark Mode"][..])] {
            let toggle = tile_toggle(card);
            put(symbols.get(symbol, Weight::Fill, 16, scale, if on { white } else { label }), centred(toggle, 16), false);
            let x = toggle.loc.x + TOGGLE + 10;
            let right = card.loc.x + card.size.w - 8;
            let top = card.loc.y + card.size.h / 2 - 8 * lines.len() as i32;
            for (j, line) in lines.iter().enumerate() {
                put(text.label(line, title, (right - x) as u32, scale), row(x, top + 16 * j as i32, right, 16), true);
            }
        }
        for (card, name, symbol) in [
            (layout.display, "Display", Symbol::Sun),
            (layout.sound, "Sound", status.volume.map_or(Symbol::SpeakerHigh, volume_symbol)),
        ] {
            let Some(card) = card else { continue };
            put(text.label(name, title, 200, scale), row(card.loc.x + 12, card.loc.y + 8, card.loc.x + card.size.w - 12, 16), true);
            let track = track_in(card);
            put(symbols.get(symbol, Weight::Fill, 16, scale, on_fill), Rectangle::new((track.loc.x + 6, track.loc.y + 6).into(), (16, 16).into()), false);
        }
        if let Some(card) = layout.sound {
            let caret = if status.expanded == Some(Module::Sound) { Symbol::CaretDown } else { Symbol::CaretRight };
            put(symbols.get(caret, Weight::Regular, 12, scale, secondary), Rectangle::new((card.loc.x + card.size.w - 24, card.loc.y + 10).into(), (12, 12).into()), false);
        }
        if let Some((_, rows)) = &layout.devices {
            if status.devices.is_empty() {
                put(text.label("No Devices", Style { color: secondary, ..body }, 200, scale), row_rect(rows[0].loc.x + 6, rows[0].loc.y, rows[0].loc.x + rows[0].size.w, LIST_ROW), true);
            }
            for (row, (name, battery, connected)) in rows.iter().zip(&status.devices) {
                let circle = Rectangle::new((row.loc.x + 3, row.loc.y + (LIST_ROW - DEVICE) / 2).into(), (DEVICE, DEVICE).into());
                put(symbols.get(device_symbol(name), Weight::Fill, 12, scale, if *connected { white } else { label }), centred(circle, 12), false);
                let x = circle.loc.x + DEVICE + 8;
                let right = row.loc.x + row.size.w - 44;
                put(text.label(name, body, (right - x) as u32, scale), row_rect(x, row.loc.y, right, LIST_ROW), true);
                if let Some(percent) = battery {
                    let piece = text.label(&format!("{percent}%"), Style { color: secondary, ..body }, 40, scale);
                    let w = (f64::from(piece.width) / scale).round() as i32;
                    let end = row.loc.x + row.size.w - 6;
                    put(piece, row_rect(end - w, row.loc.y, end, LIST_ROW), false);
                }
            }
        }
        for (row, (_, name, current)) in layout.outputs.iter().zip(&status.outputs) {
            let icon = Rectangle::new((row.loc.x + 6, row.loc.y + (LIST_ROW - 16) / 2).into(), (16, 16).into());
            put(symbols.get(output_symbol(name), Weight::Regular, 16, scale, label), icon, false);
            let x = icon.loc.x + 16 + 8;
            let right = row.loc.x + row.size.w - 30;
            put(text.label(name, body, (right - x) as u32, scale), row_rect(x, row.loc.y, right, LIST_ROW), true);
            if *current {
                put(symbols.get(Symbol::Check, Weight::Regular, 14, scale, label), Rectangle::new((row.loc.x + row.size.w - 22, row.loc.y + (LIST_ROW - 14) / 2).into(), (14, 14).into()), false);
            }
        }

        let media = layout.media;
        put(symbols.get(Symbol::MusicNote, Weight::Fill, 20, scale, secondary), Rectangle::new((media.loc.x + 12, media.loc.y + 18).into(), (20, 20).into()), false);
        let x = media.loc.x + 42;
        match &status.media {
            Some((song, artist, playing)) => {
                let buttons = media_buttons(media);
                let right = buttons[0].loc.x - 8;
                // No artist (a browser tab): the title alone, centred.
                let y = if artist.is_empty() { media.loc.y + 20 } else { media.loc.y + 11 };
                put(text.label(song, title, (right - x) as u32, scale), row(x, y, right, 16), true);
                put(text.label(artist, small, (right - x) as u32, scale), row(x, media.loc.y + 29, right, 14), true);
                let middle = if *playing { Symbol::Pause } else { Symbol::Play };
                for (button, symbol) in buttons.into_iter().zip([Symbol::SkipBack, middle, Symbol::SkipForward]) {
                    put(symbols.get(symbol, Weight::Fill, 18, scale, label), centred(button, 18), false);
                }
            }
            None => put(text.label("Not Playing", Style { color: secondary, ..title }, 200, scale), row(x, media.loc.y + 20, media.loc.x + media.size.w - 12, 16), true),
        }

        if let Some(card) = layout.power {
            let mut x = card.loc.x + 12;
            if let Some(battery) = status.battery {
                put(symbols.get(battery_symbol(battery), Weight::Regular, 20, scale, label), Rectangle::new((x, card.loc.y + 12).into(), (20, 20).into()), false);
                put(text.label(&format!("{}%", battery.0), body, 60, scale), row(x + 28, card.loc.y + 14, x + 88, 16), true);
                x += 88;
            }
            if let Some(profile) = &status.profile {
                let words = format!("Power: {}", profile_name(profile));
                let right = card.loc.x + card.size.w - 12;
                let piece = text.label(&words, Style { color: secondary, ..body }, (right - x) as u32, scale);
                let w = (f64::from(piece.width) / scale).round() as i32;
                put(piece, row(right - w, card.loc.y + 14, right, 16), false);
            }
        }

        self.labels = keep_buffers(&self.labels, pieces.iter().map(|(p, _, _)| p.clone()));
        let mut out: Vec<SceneElement> = self
            .labels
            .iter()
            .zip(&pieces)
            .filter_map(|(label, (piece, rect, left))| {
                let rect = if *left { Rectangle::new(rect.loc, (((f64::from(piece.width) / scale).round() as i32).min(rect.size.w), rect.size.h).into()) } else { *rect };
                label_element(renderer, label, rect, scale)
            })
            .collect();
        if let Some((_, glass)) = &self.glass {
            out.extend(glass.iter().cloned().map(SceneElement::Shader));
        }
        out
    }
}

/// Toggles, slider tracks and fills, cards, the panel and its shadow, front to back.
fn glass(shaders: &Shaders, layout: &Layout, status: &Status, dark: bool, accent: [u8; 3], glass_opacity: f32, scale: f64) -> Vec<PixelShaderElement> {
    let [r, g, b] = accent.map(|c| f32::from(c) / 255.0);
    let on = [r, g, b, 1.0];
    // D8 `fill` at 2× (as toggle tracks use it), so off toggles and slider tracks show on card glass.
    let off = if dark { [1.0, 1.0, 1.0, 0.16] } else { [0.0, 0.0, 0.0, 0.10] };
    let slider = if dark { [1.0, 1.0, 1.0, 0.9] } else { [0.0, 0.0, 0.0, 0.85 * 0.9] };
    let round = f64::from(TOGGLE) / 2.0;
    let mut out = Vec::new();
    let toggles = [
        (toggle_in(layout.network, 0), status.wifi.as_ref().is_some_and(|w| w.0)),
        (toggle_in(layout.network, 1), status.bluetooth.as_ref().is_some_and(|b| b.0)),
        (tile_toggle(layout.dnd), status.dnd),
        (tile_toggle(layout.dark), status.dark),
    ];
    for (rect, state) in toggles {
        out.push(shaders.light(rect, if state { on } else { off }, false, scale));
    }
    if let Some((_, rows)) = &layout.devices {
        for (row, (_, _, connected)) in rows.iter().zip(&status.devices) {
            let circle = Rectangle::new((row.loc.x + 3, row.loc.y + (LIST_ROW - DEVICE) / 2).into(), (DEVICE, DEVICE).into());
            out.push(shaders.light(circle, if *connected { on } else { off }, false, scale));
        }
    }
    for (card, value) in [(layout.display, status.brightness), (layout.sound, status.volume.map(|(v, muted)| if muted { 0 } else { v }))] {
        let (Some(card), Some(value)) = (card, value) else { continue };
        let track = track_in(card);
        out.push(shaders.glass_shape(Rectangle::new(track.loc, (slider_fill(track, value), TOGGLE).into()), slider, round, false, dark, scale));
        out.push(shaders.glass_shape(track, off, round, false, dark, scale));
    }
    let card = Material::Card.spec(dark);
    let [r, g, b] = card.tint.map(|c| f32::from(c) / 255.0);
    for rect in &layout.cards {
        out.push(shaders.glass_shape(*rect, [r, g, b, card.opacity * glass_opacity], 12.0, true, dark, scale));
    }
    let panel = Material::Panel.spec(dark);
    let [r, g, b] = panel.tint.map(|c| f32::from(c) / 255.0);
    out.push(shaders.glass_shape(layout.rect, [r, g, b, panel.opacity * glass_opacity], 16.0, true, dark, scale));
    out.extend(shaders.shadow(layout.rect, 16.0, panel.elevation, dark, scale, false));
    out
}
