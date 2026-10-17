//! The menu bar (E2.2): a 24 px `bar` glass strip across the top. Left: the logo and the active app's name; right:
//! Wi-Fi, Bluetooth, battery, volume, Control Center and the clock (E2.4). Every piece is kept between frames, so a
//! frame where only the minute changed damages only the clock.

use std::{collections::HashMap, sync::Arc};

use smithay::{
    backend::renderer::gles::{GlesRenderer, element::PixelShaderElement},
    utils::{Rectangle, Size},
};

use super::{
    Material, SceneElement, Shaders,
    titlebar::{Label, label_element},
};
use crate::{
    services::{
        bluetooth::Bluetooth,
        network::{Link, Network},
        power::Battery,
        sound::Sound,
    },
    symbols::{Symbol, Symbols, Weight},
    text::{self, Rendered, Style, Text},
    wm::Rect,
};

/// D: the bar's height in logical px; windows stay below it.
pub const HEIGHT: i32 = 24;
const EDGE: i32 = 14;
/// Between right-side items (E2.2).
const GAP: i32 = 12;
const LOGO_GAP: i32 = 18;
/// Between a battery's percentage and its symbol.
const TIGHT: i32 = 4;
const SYMBOL: u16 = 16;

/// What the bar shows; `State::menu_bar` builds it from the service states.
#[derive(Debug, Default, PartialEq)]
pub struct Content {
    /// The focused app's id (`brave-browser`), else its process name; `None`: no window focused.
    pub app: Option<String>,
    pub status: Vec<Item>,
    pub clock: String,
}

#[derive(Debug, PartialEq)]
pub struct Item {
    pub symbol: Symbol,
    /// Shown left of the symbol (the battery percentage).
    pub text: Option<String>,
}

pub fn wifi(network: &Network) -> Symbol {
    match &network.link {
        Link::Wifi { bars, .. } => [Symbol::WifiLow, Symbol::WifiMedium, Symbol::WifiHigh][usize::from((*bars).clamp(1, 3)) - 1],
        Link::Ethernet => Symbol::Ethernet,
        Link::Other(_) => Symbol::Network,
        Link::None if network.wifi_enabled => Symbol::WifiNone,
        Link::None => Symbol::WifiSlash,
    }
}

pub fn bluetooth(bluetooth: &Bluetooth) -> Symbol {
    match (bluetooth.powered, bluetooth.devices.is_empty()) {
        (false, _) => Symbol::BluetoothSlash,
        (true, false) => Symbol::BluetoothConnected,
        (true, true) => Symbol::Bluetooth,
    }
}

pub fn battery(battery: &Battery) -> Item {
    let symbol = match battery.percent {
        _ if battery.charging => Symbol::BatteryCharging,
        88.. => Symbol::BatteryFull,
        63.. => Symbol::BatteryHigh,
        38.. => Symbol::BatteryMedium,
        13.. => Symbol::BatteryLow,
        _ => Symbol::BatteryEmpty,
    };
    Item { symbol, text: Some(format!("{}%", battery.percent)) }
}

pub fn volume(sound: &Sound) -> Symbol {
    match sound.output.volume {
        _ if sound.output.muted => Symbol::SpeakerMuted,
        0 => Symbol::SpeakerMuted,
        1..34 => Symbol::SpeakerNone,
        34..67 => Symbol::SpeakerLow,
        _ => Symbol::SpeakerHigh,
    }
}

/// The name an app goes by: its `.desktop` entry's `Name`, else its id, capitalised (`org.kde.dolphin` → `Dolphin`).
// ponytail: looks up `<id>.desktop` only; StartupWMClass matching and localised names come with the app index (M3).
fn app_name(id: &str) -> String {
    let home = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{}/.local/share", std::env::var("HOME").unwrap_or_default()));
    let system = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    let entry = std::iter::once(home.as_str())
        .chain(system.split(':'))
        .find_map(|dir| std::fs::read_to_string(format!("{dir}/applications/{id}.desktop")).ok());
    let name = entry.as_deref().and_then(|text| {
        let main = text.split_once("[Desktop Entry]")?.1;
        let main = main.split("\n[").next().unwrap_or(main);
        main.lines().find_map(|line| line.strip_prefix("Name=")).map(str::trim).map(str::to_owned)
    });
    name.unwrap_or_else(|| {
        let short = id.rsplit('.').next().unwrap_or(id);
        let mut chars = short.chars();
        chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
    })
}

/// Width, dark, glass opacity and scale: what the strip and its shadow depend on.
type GlassKey = (i32, bool, u32, u64);

/// The bar's pieces, kept between frames.
#[derive(Default)]
pub struct MenuBar {
    glass: Option<(GlassKey, Vec<PixelShaderElement>)>,
    labels: Vec<Label>,
    names: HashMap<String, String>,
}

impl MenuBar {
    /// Front to back: symbols and labels, the strip, its shadow. `width` is the output's, logical.
    #[allow(clippy::too_many_arguments)]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        symbols: &mut Symbols,
        content: &Content,
        width: i32,
        dark: bool,
        glass_opacity: f32,
        scale: f64,
    ) -> Vec<SceneElement> {
        let key = (width, dark, glass_opacity.to_bits(), scale.to_bits());
        if self.glass.as_ref().is_none_or(|(k, _)| *k != key) {
            let rect = Rectangle::new((0, 0).into(), (width, HEIGHT).into());
            let spec = Material::Bar.spec(dark);
            let mut glass = vec![shaders.glass(rect, Material::Bar, dark, glass_opacity, scale)];
            glass.extend(shaders.shadow(rect, spec.radius, spec.elevation, dark, scale, false));
            self.glass = Some((key, glass));
        }

        // label (D2): 85% black or white.
        let color = if dark { [255, 255, 255, 217] } else { [0, 0, 0, 217] };
        let body = Style { size: 13, line_height: 16, weight: 400, color, tabular: true };
        let symbol = |symbols: &mut Symbols, s: Symbol| symbols.get(s, Weight::Regular, SYMBOL, scale, color);
        // (piece, gap after it); left pieces then right ones, each list in reading order.
        let mut left = vec![(symbols.get(Symbol::Logo, Weight::Regular, SYMBOL, scale, color), LOGO_GAP)];
        if let Some(app) = &content.app {
            let name = self.names.entry(app.clone()).or_insert_with(|| app_name(app));
            left.push((text.label(name, Style { weight: 600, tabular: false, ..body }, 300, scale), 0));
        }
        let mut right = Vec::new();
        for item in &content.status {
            if let Some(t) = &item.text {
                right.push((text.label(t, body, 100, scale), TIGHT));
            }
            right.push((symbol(symbols, item.symbol), GAP));
        }
        right.push((symbol(symbols, Symbol::Sliders), GAP));
        right.push((text.label(&content.clock, body, 300, scale), 0));

        let logical = |r: &Rendered| (f64::from(r.width) / scale).round() as i32;
        let mut rects = Vec::new();
        let mut x = EDGE;
        for (piece, gap) in &left {
            rects.push(Rectangle::new((x, 0).into(), (logical(piece), HEIGHT).into()));
            x += logical(piece) + gap;
        }
        let total: i32 = right.iter().map(|(piece, gap)| logical(piece) + gap).sum();
        let mut x = width - EDGE - total;
        for (piece, gap) in &right {
            rects.push(Rectangle::new((x, 0).into(), (logical(piece), HEIGHT).into()));
            x += logical(piece) + gap;
        }

        // Unchanged pieces keep their buffer, so they keep their damage id.
        let old = std::mem::take(&mut self.labels);
        self.labels = left
            .into_iter()
            .chain(right)
            .map(|(piece, _)| match old.iter().find(|(r, _)| Arc::ptr_eq(r, &piece)) {
                Some(label) => label.clone(),
                None => {
                    let buffer = text::to_buffer(&piece);
                    (piece, buffer)
                }
            })
            .collect();
        let mut out: Vec<SceneElement> =
            self.labels.iter().zip(rects).filter_map(|(label, rect): (&Label, Rect)| label_element(renderer, label, rect, scale)).collect();
        if let Some((_, glass)) = &self.glass {
            out.extend(glass.iter().cloned().map(SceneElement::Shader));
        }
        out
    }
}

/// The part of the output windows may use: everything below the bar.
pub fn below(area: Rect) -> Rect {
    let cut = (HEIGHT - area.loc.y).clamp(0, area.size.h);
    Rectangle::new((area.loc.x, area.loc.y + cut).into(), Size::from((area.size.w, area.size.h - cut)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::sound::Device;

    #[test]
    fn symbols_follow_state() {
        let wifi_of = |link, wifi_enabled| wifi(&Network { wifi_enabled, link });
        assert_eq!(wifi_of(Link::Wifi { ssid: "x".into(), bars: 3 }, true), Symbol::WifiHigh);
        assert_eq!(wifi_of(Link::Wifi { ssid: "x".into(), bars: 1 }, true), Symbol::WifiLow);
        assert_eq!(wifi_of(Link::None, true), Symbol::WifiNone);
        assert_eq!(wifi_of(Link::None, false), Symbol::WifiSlash);
        assert_eq!(wifi_of(Link::Ethernet, false), Symbol::Ethernet);

        let battery_of = |percent, charging| battery(&Battery { percent, charging, minutes_left: None });
        assert_eq!(battery_of(85, false), Item { symbol: Symbol::BatteryHigh, text: Some("85%".into()) });
        assert_eq!(battery_of(100, false).symbol, Symbol::BatteryFull);
        assert_eq!(battery_of(5, false).symbol, Symbol::BatteryEmpty);
        assert_eq!(battery_of(40, true).symbol, Symbol::BatteryCharging);

        let sound = |volume, muted| Sound { output: Device { name: String::new(), volume, muted }, input: None, outputs: Vec::new() };
        assert_eq!(volume(&sound(89, false)), Symbol::SpeakerHigh);
        assert_eq!(volume(&sound(50, false)), Symbol::SpeakerLow);
        assert_eq!(volume(&sound(10, false)), Symbol::SpeakerNone);
        assert_eq!(volume(&sound(0, false)), Symbol::SpeakerMuted);
        assert_eq!(volume(&sound(89, true)), Symbol::SpeakerMuted);
    }

    #[test]
    fn app_names() {
        assert_eq!(app_name("org.example.no-such-app"), "No-such-app");
    }

    #[test]
    fn windows_stay_below_the_bar() {
        let area = Rectangle::new((0, 0).into(), (1280, 800).into());
        assert_eq!(below(area), Rectangle::new((0, 24).into(), (1280, 776).into()));
        // A layer bar already reserved more: unchanged.
        let lower = Rectangle::new((0, 30).into(), (1280, 770).into());
        assert_eq!(below(lower), lower);
    }
}
