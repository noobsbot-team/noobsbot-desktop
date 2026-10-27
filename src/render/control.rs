//! Control Center (E2.3): a `panel` glass column under the bar at the top right with `card` modules: Wi-Fi and
//! Bluetooth toggles, Do Not Disturb, Dark Mode, Keep Awake (with the apps blocking sleep), Display and Sound sliders,
//! Now Playing, battery and power mode. Opened from a menu bar item, one module of it with its links (`Status::bar`).

use smithay::{
    backend::renderer::gles::{GlesRenderer, element::PixelShaderElement},
    utils::{Logical, Rectangle, Size},
};

use super::{
    Material, SceneElement, Shaders,
    titlebar::{Label, keep_buffers, label_element},
    ui,
};
use crate::{
    apps::Icons,
    shell::control::{
        self, Control, Layout, Module, Status, TOGGLE, app_track, label_top, media_buttons, night_toggle, profile_name, slider_fill, tile_toggle, track_in,
    },
    symbols::{Symbol, Symbols, Weight},
    text::{Rendered, Style, Text, ts},
    wm::Rect,
};

/// What the glass depends on: the layout's rect, the status, dark, accent, opacity, scale, which app icons were found
/// (the rest get a letter tile in the glass).
type Key = (Rect, Status, bool, [u8; 3], u32, u64, Vec<bool>);

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

/// The round icon of a list row: accent when connected.
const DEVICE: i32 = 22;

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
        icons: &mut Icons,
        status: &Status,
        output: Size<i32, Logical>,
        dark: bool,
        accent: [u8; 3],
        glass_opacity: f32,
        scale: f64,
    ) -> Vec<SceneElement> {
        let layout = control::layout(output, status);
        // Each app row's icon, left of its name: the theme's, else a letter tile.
        let app_icons: Vec<_> = layout
            .apps
            .iter()
            .zip(&status.apps)
            .map(|(row, app)| {
                let rect = Rectangle::new((row.loc.x + 4, row.loc.y + (row.size.h - 20) / 2).into(), (20, 20).into());
                (rect, super::switcher::app_icon(shaders, text, icons, &app.name, app.icon.as_deref(), rect, dark, scale))
            })
            .collect();
        let found = app_icons.iter().map(|(_, (tile, _))| tile.is_none()).collect();
        let key = (layout.rect, status.clone(), dark, accent, glass_opacity.to_bits(), scale.to_bits(), found);
        if self.glass.as_ref().is_none_or(|(k, _)| *k != key) {
            let mut elements: Vec<_> = app_icons.iter().filter_map(|(_, (tile, _))| tile.clone()).collect();
            elements.extend(glass(shaders, &layout, status, dark, accent, glass_opacity, scale));
            self.glass = Some((key, elements));
        }

        let (label, secondary) = (ui::label(dark), ui::secondary(dark));
        let white = [255, 255, 255, 255];
        // On the slider fill: white in light mode (the fill is dark), dark in dark mode.
        let on_fill = if dark { [0, 0, 0, 191] } else { white };
        let title = Style { color: label, ..ui::HEADLINE };
        let body = Style { color: label, ..ui::BODY };
        let small = Style { color: secondary, ..ui::SUBHEADLINE };

        // (piece, rect, left-aligned); a left-aligned piece's rect shrinks to its width.
        let mut pieces: Vec<(std::sync::Arc<Rendered>, Rect, bool)> = Vec::new();
        let mut put = |piece, rect, left| pieces.push((piece, rect, left));
        let row = row_rect;

        let full = status.bar.is_none();
        let unavailable = (false, "Unavailable".to_owned());
        let toggles = [(Control::Wifi, Symbol::WifiHigh, "Wi-Fi", &status.wifi), (Control::Bluetooth, Symbol::Bluetooth, "Bluetooth", &status.bluetooth)];
        for (toggle, symbol, name, (on, sub)) in
            toggles.into_iter().filter_map(|(c, s, n, v)| Some((layout.rect_of(c)?, s, n, v.clone().unwrap_or_else(|| unavailable.clone()))))
        {
            put(symbols.get(symbol, Weight::Fill, 16, scale, if on { white } else { label }), centred(toggle, 16), false);
            let x = toggle.loc.x + TOGGLE + 10;
            let right = layout.network.loc.x + layout.network.size.w - 10;
            let top = label_top(toggle);
            put(text.label(name, title, (right - x) as u32, scale), row(x, top, right, ts(16)), true);
            put(text.label(&sub, small, (right - x) as u32, scale), row(x, top + ts(16), right, ts(14)), true);
        }
        let tiles = [(layout.dnd, Symbol::Moon, status.dnd, &["Do Not", "Disturb"][..]), (layout.dark, Symbol::CircleHalf, status.dark, &["Dark Mode"][..])];
        for (card, symbol, on, lines) in tiles.into_iter().filter(|_| full) {
            let toggle = tile_toggle(card);
            put(symbols.get(symbol, Weight::Fill, 16, scale, if on { white } else { label }), centred(toggle, 16), false);
            let x = toggle.loc.x + TOGGLE + 10;
            let right = card.loc.x + card.size.w - 8;
            let top = card.loc.y + card.size.h / 2 - ts(8) * lines.len() as i32;
            for (j, line) in lines.iter().enumerate() {
                put(text.label(line, title, (right - x) as u32, scale), row(x, top + ts(16) * j as i32, right, ts(16)), true);
            }
        }
        for (card, name, symbol) in
            [(layout.display, "Display", Symbol::Sun), (layout.sound, "Sound", status.volume.map_or(Symbol::SpeakerHigh, volume_symbol))]
        {
            let Some(card) = card else { continue };
            put(text.label(name, title, 200, scale), row(card.loc.x + 12, card.loc.y + ts(8), card.loc.x + card.size.w - 12, ts(16)), true);
            let track = track_in(card);
            put(symbols.get(symbol, Weight::Fill, 16, scale, on_fill), Rectangle::new((track.loc.x + 6, track.loc.y + 6).into(), (16, 16).into()), false);
        }
        if let Some(card) = layout.display {
            let on = if status.night { white } else { label };
            put(symbols.get(Symbol::Sun, Weight::Fill, 12, scale, on), centred(night_toggle(card), 12), false);
        }
        if let Some(card) = layout.sound.filter(|_| full) {
            let caret = if status.expanded == Some(Module::Sound) { Symbol::CaretDown } else { Symbol::CaretRight };
            put(
                symbols.get(caret, Weight::Regular, 12, scale, secondary),
                Rectangle::new((card.loc.x + card.size.w - 24, card.loc.y + ts(8) + (ts(16) - 12) / 2).into(), (12, 12).into()),
                false,
            );
        }
        if full {
            let card = layout.awake;
            let toggle = layout.rect_of(control::Control::KeepAwake).unwrap_or_default();
            put(symbols.get(Symbol::Monitor, Weight::Fill, 16, scale, if status.awake { white } else { label }), centred(toggle, 16), false);
            let x = toggle.loc.x + TOGGLE + 10;
            let right = card.loc.x + card.size.w - 32;
            let top = label_top(toggle);
            put(text.label("Keep Awake", title, (right - x) as u32, scale), row(x, top, right, ts(16)), true);
            let mut sub = (if status.awake { "On" } else { "Off" }).to_owned();
            match status.blockers.len() {
                0 => {}
                1 => sub.push_str(" · 1 app blocking sleep"),
                n => sub.push_str(&format!(" · {n} apps blocking sleep")),
            }
            put(text.label(&sub, small, (right - x) as u32, scale), row(x, top + ts(16), right, ts(14)), true);
            let caret = if status.expanded == Some(Module::Awake) { Symbol::CaretDown } else { Symbol::CaretRight };
            put(
                symbols.get(caret, Weight::Regular, 12, scale, secondary),
                Rectangle::new((card.loc.x + card.size.w - 24, toggle.loc.y + 8).into(), (12, 12).into()),
                false,
            );
            if status.blockers.is_empty()
                && let Some(first) = layout.blockers.first()
            {
                put(
                    text.label("No Apps Blocking Sleep", Style { color: secondary, ..body }, 240, scale),
                    row_rect(first.loc.x + 6, first.loc.y, first.loc.x + first.size.w, first.size.h),
                    true,
                );
            }
            for (rect, (who, why)) in layout.blockers.iter().zip(&status.blockers) {
                let (x, end) = (rect.loc.x + 6, rect.loc.x + rect.size.w - 6);
                let piece = text.label(who, body, ((end - x) / 2) as u32, scale);
                let w = (f64::from(piece.width) / scale).round() as i32;
                put(piece, row_rect(x, rect.loc.y, end, rect.size.h), true);
                if !why.is_empty() {
                    let x = x + w + 8;
                    put(text.label(why, Style { color: secondary, ..body }, (end - x).max(1) as u32, scale), row_rect(x, rect.loc.y, end, rect.size.h), true);
                }
            }
        }
        if let Some((_, rows)) = &layout.list {
            if status.rows.is_empty()
                && let Some(first) = rows.first()
            {
                let empty = if status.expanded == Some(Module::Wifi) { "No Networks" } else { "No Devices" };
                put(
                    text.label(empty, Style { color: secondary, ..body }, 200, scale),
                    row_rect(first.loc.x + 6, first.loc.y, first.loc.x + first.size.w, first.size.h),
                    true,
                );
            }
            for (row, item) in rows.iter().zip(status.rows.iter().skip(layout.scroll)) {
                let circle = Rectangle::new((row.loc.x + 3, row.loc.y + (row.size.h - DEVICE) / 2).into(), (DEVICE, DEVICE).into());
                put(symbols.get(item.symbol, Weight::Fill, 12, scale, if item.on { white } else { label }), centred(circle, 12), false);
                let x = circle.loc.x + DEVICE + 8;
                let right = row.loc.x + row.size.w - ts(44);
                put(text.label(&item.name, body, (right - x) as u32, scale), row_rect(x, row.loc.y, right, row.size.h), true);
                let end = row.loc.x + row.size.w - 6;
                if let Some(detail) = &item.detail {
                    let piece = text.label(detail, Style { color: secondary, ..body }, ts(40) as u32, scale);
                    let w = (f64::from(piece.width) / scale).round() as i32;
                    put(piece, row_rect(end - w, row.loc.y, end, row.size.h), false);
                }
                if item.lock {
                    put(
                        symbols.get(Symbol::Lock, Weight::Regular, 14, scale, secondary),
                        Rectangle::new((end - 14, row.loc.y + (row.size.h - 14) / 2).into(), (14, 14).into()),
                        false,
                    );
                }
            }
        }
        for ((row, app), (icon, (_, piece))) in layout.apps.iter().zip(&status.apps).zip(app_icons) {
            put(piece, icon, false);
            let x = icon.loc.x + 20 + 8;
            let right = app_track(*row).loc.x - 8;
            put(text.label(&app.name, body, (right - x) as u32, scale), row_rect(x, row.loc.y, right, row.size.h), true);
        }
        for (row, (_, name, current)) in layout.outputs.iter().zip(&status.outputs) {
            let icon = Rectangle::new((row.loc.x + 6, row.loc.y + (row.size.h - 16) / 2).into(), (16, 16).into());
            put(symbols.get(output_symbol(name), Weight::Regular, 16, scale, label), icon, false);
            let x = icon.loc.x + 16 + 8;
            let right = row.loc.x + row.size.w - 30;
            put(text.label(name, body, (right - x) as u32, scale), row_rect(x, row.loc.y, right, row.size.h), true);
            if *current {
                put(
                    symbols.get(Symbol::Check, Weight::Regular, 14, scale, label),
                    Rectangle::new((row.loc.x + row.size.w - 22, row.loc.y + (row.size.h - 14) / 2).into(), (14, 14).into()),
                    false,
                );
            }
        }

        let media = layout.media;
        if full {
            put(
                symbols.get(Symbol::MusicNote, Weight::Fill, 20, scale, secondary),
                Rectangle::new((media.loc.x + 12, media.loc.y + (media.size.h - 20) / 2).into(), (20, 20).into()),
                false,
            );
            let x = media.loc.x + 42;
            match &status.media {
                Some((song, artist, playing)) => {
                    let buttons = media_buttons(media);
                    let right = buttons[0].loc.x - 8;
                    // No artist (a browser tab): the title alone, centred.
                    let y = if artist.is_empty() { media.loc.y + ts(20) } else { media.loc.y + ts(11) };
                    put(text.label(song, title, (right - x) as u32, scale), row(x, y, right, ts(16)), true);
                    put(text.label(artist, small, (right - x) as u32, scale), row(x, media.loc.y + ts(29), right, ts(14)), true);
                    let middle = if *playing { Symbol::Pause } else { Symbol::Play };
                    for (button, symbol) in buttons.into_iter().zip([Symbol::SkipBack, middle, Symbol::SkipForward]) {
                        put(symbols.get(symbol, Weight::Fill, 18, scale, label), centred(button, 18), false);
                    }
                }
                None => put(
                    text.label("Not Playing", Style { color: secondary, ..title }, 200, scale),
                    row(x, media.loc.y + ts(20), media.loc.x + media.size.w - 12, ts(16)),
                    true,
                ),
            }
        }

        let links = control::links(status);
        for (row, (name, _)) in layout.links.iter().zip(&links) {
            put(text.label(name, body, (row.size.w - 12) as u32, scale), row_rect(row.loc.x + 6, row.loc.y, row.loc.x + row.size.w - 6, row.size.h), true);
        }

        if let Some(card) = layout.power {
            let mut x = card.loc.x + 12;
            if let Some(battery) = status.battery {
                put(
                    symbols.get(battery_symbol(battery), Weight::Regular, 20, scale, label),
                    Rectangle::new((x, card.loc.y + (card.size.h - 20) / 2).into(), (20, 20).into()),
                    false,
                );
                put(text.label(&format!("{}%", battery.0), body, 60, scale), row(x + 28, card.loc.y + ts(14), x + 88, ts(16)), true);
                x += 88;
            }
            if let Some(profile) = &status.profile {
                let words = format!("Power: {}", profile_name(profile));
                // The ▾ that opens the profiles.
                put(
                    symbols.get(Symbol::CaretDown, Weight::Regular, 12, scale, secondary),
                    Rectangle::new((card.loc.x + card.size.w - 24, card.loc.y + ts(14) + (ts(16) - 12) / 2).into(), (12, 12).into()),
                    false,
                );
                let right = card.loc.x + card.size.w - 30;
                let piece = text.label(&words, Style { color: secondary, ..body }, (right - x) as u32, scale);
                let w = (f64::from(piece.width) / scale).round() as i32;
                put(piece, row(right - w, card.loc.y + ts(14), right, ts(16)), false);
            }
        }

        for (row, name) in layout.profiles.iter().zip(&status.profiles) {
            let right = row.loc.x + row.size.w - 30;
            put(text.label(profile_name(name), body, (right - row.loc.x - 6) as u32, scale), row_rect(row.loc.x + 6, row.loc.y, right, row.size.h), true);
            if status.profile.as_ref() == Some(name) {
                put(
                    symbols.get(Symbol::Check, Weight::Regular, 14, scale, label),
                    Rectangle::new((row.loc.x + row.size.w - 22, row.loc.y + (row.size.h - 14) / 2).into(), (14, 14).into()),
                    false,
                );
            }
        }

        self.labels = keep_buffers(&self.labels, pieces.iter().map(|(p, _, _)| p.clone()));
        let mut out: Vec<SceneElement> = self
            .labels
            .iter()
            .zip(&pieces)
            .filter_map(|(label, (piece, rect, left))| {
                let rect = if *left {
                    Rectangle::new(rect.loc, (((f64::from(piece.width) / scale).round() as i32).min(rect.size.w), rect.size.h).into())
                } else {
                    *rect
                };
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
        (layout.rect_of(Control::Wifi), status.wifi.as_ref().is_some_and(|w| w.0)),
        (layout.rect_of(Control::Bluetooth), status.bluetooth.as_ref().is_some_and(|b| b.0)),
        (layout.rect_of(Control::Dnd).map(tile_toggle), status.dnd),
        (layout.rect_of(Control::Dark).map(tile_toggle), status.dark),
        (layout.rect_of(Control::KeepAwake), status.awake),
        (layout.display.map(night_toggle), status.night),
    ];
    for (rect, state) in toggles.into_iter().filter_map(|(r, on)| Some((r?, on))) {
        out.push(shaders.light(rect, if state { on } else { off }, false, scale));
    }
    if let Some((_, rows)) = &layout.list {
        for (row, item) in rows.iter().zip(status.rows.iter().skip(layout.scroll)) {
            let circle = Rectangle::new((row.loc.x + 3, row.loc.y + (row.size.h - DEVICE) / 2).into(), (DEVICE, DEVICE).into());
            out.push(shaders.light(circle, if item.on { on } else { off }, false, scale));
        }
    }
    let main = [(layout.display, status.brightness), (layout.sound, status.volume.map(|(v, muted)| if muted { 0 } else { v }))]
        .into_iter()
        .filter_map(|(card, value)| Some((track_in(card?), value?)));
    let apps = layout.apps.iter().zip(&status.apps).map(|(row, app)| (app_track(*row), app.volume));
    for (track, value) in main.chain(apps) {
        out.push(shaders.glass_shape(Rectangle::new(track.loc, (slider_fill(track, value), TOGGLE).into()), slider, round, false, dark, scale));
        out.push(shaders.glass_shape(track, off, round, false, dark, scale));
    }
    let card = Material::Card.spec(dark);
    let [r, g, b] = card.tint.map(|c| f32::from(c) / 255.0);
    for rect in &layout.cards {
        out.push(shaders.glass_shape(*rect, [r, g, b, card.opacity * glass_opacity], card.radius, true, dark, scale));
    }
    let panel = Material::Panel.spec(dark);
    let [r, g, b] = panel.tint.map(|c| f32::from(c) / 255.0);
    out.push(shaders.glass_shape(layout.rect, [r, g, b, panel.opacity * glass_opacity], panel.radius, true, dark, scale));
    out.extend(shaders.shadow(layout.rect, panel.radius, panel.elevation, dark, scale, false));
    out
}
