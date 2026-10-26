//! The menu bar (E2.2): a 24 px `bar` glass strip (taller with larger text) across the top. Left: the logo and the
//! active app's name; right: tray icons, Wi-Fi, Bluetooth, battery, volume, Control Center and the clock (E2.4).
//! Every piece is kept between frames, so a frame where only the minute changed damages only the clock.

use std::{collections::HashMap, sync::Arc};

use smithay::{
    backend::renderer::gles::{GlesRenderer, element::PixelShaderElement},
    utils::{Rectangle, Size},
};

use super::{
    Material, SceneElement, Shaders,
    titlebar::{Label, keep_buffers, label_element},
};
use crate::{
    apps::Icons,
    services::{
        bluetooth::Bluetooth,
        network::{Link, Network},
        power::Battery,
        sound::Sound,
        tray::Image,
    },
    symbols::{Symbol, Symbols, Weight},
    text::{Rendered, Style, Text, ts},
    wm::Rect,
};

/// D: the bar's height in logical px at text size 1; windows stay below it.
const HEIGHT: i32 = 24;
const EDGE: i32 = 14;
/// Between right-side items (E2.2); this, `TIGHT`, `APP_WIDTH` and `MIN_APP` grow with the text size.
const GAP: i32 = 12;
const LOGO_GAP: i32 = 18;
/// Between a battery's percentage and its symbol.
const TIGHT: i32 = 4;
const SYMBOL: u16 = 16;
/// The app's name is ellipsised past this, and dropped when less than `MIN_APP` is free.
const APP_WIDTH: i32 = 300;
const MIN_APP: i32 = 40;

/// The bar's height in logical px, grown with the text size.
pub fn height() -> i32 {
    ts(HEIGHT)
}

/// The width left for the app's name between the logo and the right group on a `width`-wide bar.
pub fn app_room(width: i32, logo: i32, right: i32) -> i32 {
    width - 2 * EDGE - logo - LOGO_GAP - ts(GAP) - right
}

/// How many tray icons, from the left, go into the "»" item's menu so the app's name keeps `MIN_APP`: `fixed` is
/// the rest of the right group, `tray` the icons' widths, `chevron` the "»"'s.
pub fn tray_overflow(width: i32, logo: i32, fixed: i32, tray: &[i32], chevron: i32) -> usize {
    (0..=tray.len())
        .find(|&k| {
            let more = if k > 0 { chevron + ts(GAP) } else { 0 };
            app_room(width, logo, fixed + more + tray[k..].iter().map(|w| w + ts(GAP)).sum::<i32>()) >= ts(MIN_APP)
        })
        .unwrap_or(tray.len())
}

/// What the bar shows; `State::menu_bar` builds it from the service states.
#[derive(Debug, Default, PartialEq)]
pub struct Content {
    /// The focused app's id (`helium`), else its process name; `None`: no window focused.
    pub app: Option<String>,
    /// Each tray item's icons, left to right.
    pub tray: Vec<Arc<Vec<Image>>>,
    pub status: Vec<Item>,
    /// The input item's text, the active layout's code ("US", E6.12); `None`: one layout, no input method.
    pub input: Option<String>,
    pub clock: String,
}

/// What a click on the bar hit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BarItem {
    Logo,
    App,
    /// `Content::tray[i]`.
    Tray(usize),
    /// "»": the tray icons that didn't fit.
    Overflow,
    /// `Content::status[i]`.
    Status(usize),
    /// The keyboard layout / input method menu.
    Input,
    ControlCenter,
    Clock,
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
    match (bluetooth.powered, bluetooth.connected().next().is_some()) {
        (false, _) => Symbol::BluetoothSlash,
        (true, true) => Symbol::BluetoothConnected,
        (true, false) => Symbol::Bluetooth,
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
pub fn app_name(id: &str) -> String {
    let home = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{}/.local/share", std::env::var("HOME").unwrap_or_default()));
    let system = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    let entry =
        std::iter::once(home.as_str()).chain(system.split(':')).find_map(|dir| std::fs::read_to_string(format!("{dir}/applications/{id}.desktop")).ok());
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

/// Width, height, dark, glass opacity and scale: what the strip and its shadow depend on.
type GlassKey = (i32, i32, bool, u32, u64);

/// The bar's pieces, kept between frames.
#[derive(Default)]
pub struct MenuBar {
    glass: Option<(GlassKey, Vec<PixelShaderElement>)>,
    labels: Vec<Label>,
    names: HashMap<String, String>,
    /// Each item's area from the last frame; empty while the bar is hidden.
    hits: Vec<(BarItem, Rect)>,
    /// The pressed pill behind the item whose popup is open, and what it was made for.
    pill: Option<((Rect, bool, u64), PixelShaderElement)>,
    /// The first this many tray icons are in the "»" menu instead of on the bar.
    overflow: usize,
}

impl MenuBar {
    pub fn visible(&self) -> bool {
        !self.hits.is_empty()
    }

    /// The item under `location` (output-relative).
    pub fn hit(&self, location: smithay::utils::Point<f64, smithay::utils::Logical>) -> Option<BarItem> {
        self.hits.iter().find(|(_, rect)| rect.to_f64().contains(location)).map(|(item, _)| *item)
    }

    /// The pressed pill behind `item`: rounded, a little inside the bar's height. Menus line up with it.
    pub fn pill(&self, item: BarItem) -> Option<Rect> {
        let (_, area) = self.hits.iter().find(|(i, _)| *i == item)?;
        Some(Rectangle::new((area.loc.x + ts(GAP) / 2 - 6, 2).into(), (area.size.w - ts(GAP) + 12, height() - 4).into()))
    }

    /// How many tray icons, from the first, didn't fit and are in the "»" menu.
    pub fn overflow(&self) -> usize {
        self.overflow
    }

    /// The display name of app `id`, looked up once.
    pub fn name(&mut self, id: &str) -> String {
        self.names.entry(id.to_owned()).or_insert_with(|| app_name(id)).clone()
    }

    /// A fullscreen window covers the bar: it takes no clicks.
    pub fn hide(&mut self) {
        self.hits.clear();
    }

    /// Front to back: symbols and labels, the pressed pill behind `active`, the strip, its shadow. `width` is the
    /// output's, logical.
    #[allow(clippy::too_many_arguments)]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        symbols: &mut Symbols,
        icons: &mut Icons,
        content: &Content,
        active: Option<BarItem>,
        width: i32,
        dark: bool,
        glass_opacity: f32,
        scale: f64,
    ) -> Vec<SceneElement> {
        let key = (width, height(), dark, glass_opacity.to_bits(), scale.to_bits());
        if self.glass.as_ref().is_none_or(|(k, _)| *k != key) {
            let rect = Rectangle::new((0, 0).into(), (width, height()).into());
            let spec = Material::Bar.spec(dark);
            let mut glass = vec![shaders.glass(rect, Material::Bar, dark, glass_opacity, scale)];
            glass.extend(shaders.shadow(rect, spec.radius, spec.elevation, dark, scale, false));
            self.glass = Some((key, glass));
        }

        // label (D2): 85% black or white.
        let color = if dark { [255, 255, 255, 217] } else { [0, 0, 0, 217] };
        let body = Style { size: 13, line_height: 16, weight: 400, color, tabular: true };
        let symbol = |symbols: &mut Symbols, s: Symbol| symbols.get(s, Weight::Regular, SYMBOL, scale, color);
        // (piece, gap after it, item); left pieces then right ones, each list in reading order. The right group is
        // laid out first: the app's name takes what is left of the bar, or goes when that is too little.
        let mut right = Vec::new();
        let px = (f64::from(SYMBOL) * scale).round() as u32;
        let tray: Vec<_> = content.tray.iter().map(|images| icons.tray(images, px)).collect();
        if let Some(code) = &content.input {
            right.push((text.label(code, Style { weight: 600, ..body }, 100, scale), ts(GAP), BarItem::Input));
        }
        for (i, item) in content.status.iter().enumerate() {
            if let Some(t) = &item.text {
                right.push((text.label(t, body, 100, scale), ts(TIGHT), BarItem::Status(i)));
            }
            right.push((symbol(symbols, item.symbol), ts(GAP), BarItem::Status(i)));
        }
        right.push((symbol(symbols, Symbol::Sliders), ts(GAP), BarItem::ControlCenter));
        right.push((text.label(&content.clock, body, 300, scale), 0, BarItem::Clock));

        let logical = |r: &Rendered| (f64::from(r.width) / scale).round() as i32;
        let logo = symbols.get(Symbol::Logo, Weight::Regular, SYMBOL, scale, color);
        // Tray icons that don't fit go into "»", at the left of the tray.
        let fixed: i32 = right.iter().map(|(piece, gap, _)| logical(piece) + gap).sum();
        let chevron = text.label("»", body, 100, scale);
        let widths: Vec<i32> = tray.iter().map(|t| logical(t)).collect();
        self.overflow = tray_overflow(width, logical(&logo), fixed, &widths, logical(&chevron));
        let shown = tray.into_iter().enumerate().skip(self.overflow).map(|(i, icon)| (icon, ts(GAP), BarItem::Tray(i)));
        let more = (self.overflow > 0).then_some((chevron, ts(GAP), BarItem::Overflow));
        right = more.into_iter().chain(shown).chain(right).collect();
        let total: i32 = right.iter().map(|(piece, gap, _)| logical(piece) + gap).sum();
        let room = app_room(width, logical(&logo), total);
        let mut left = vec![(logo, LOGO_GAP, BarItem::Logo)];
        if let Some(app) = content.app.as_ref().filter(|_| room >= ts(MIN_APP)) {
            let name = self.names.entry(app.clone()).or_insert_with(|| app_name(app));
            left.push((text.label(name, Style { weight: 600, tabular: false, ..body }, room.min(ts(APP_WIDTH)) as u32, scale), 0, BarItem::App));
        }
        let mut rects = Vec::new();
        let mut x = EDGE;
        for (piece, gap, _) in &left {
            rects.push(Rectangle::new((x, 0).into(), (logical(piece), height()).into()));
            x += logical(piece) + gap;
        }
        let mut x = width - EDGE - total;
        for (piece, gap, _) in &right {
            rects.push(Rectangle::new((x, 0).into(), (logical(piece), height()).into()));
            x += logical(piece) + gap;
        }

        // An item's click area spans its pieces plus half a gap each side.
        let items: Vec<BarItem> = left.iter().chain(&right).map(|(_, _, item)| *item).collect();
        self.hits.clear();
        for (item, rect) in items.iter().zip(&rects) {
            let rect = Rectangle::new((rect.loc.x - ts(GAP) / 2, 0).into(), (rect.size.w + ts(GAP), height()).into());
            match self.hits.last_mut() {
                Some((last, area)) if last == item => *area = area.merge(rect),
                _ => self.hits.push((*item, rect)),
            }
        }

        self.labels = keep_buffers(&self.labels, left.into_iter().chain(right).map(|(piece, _, _)| piece));
        let mut out: Vec<SceneElement> =
            self.labels.iter().zip(rects).filter_map(|(label, rect): (&Label, Rect)| label_element(renderer, label, rect, scale)).collect();
        match active.and_then(|item| self.pill(item)) {
            Some(rect) => {
                let key = (rect, dark, scale.to_bits());
                if self.pill.as_ref().is_none_or(|(k, _)| *k != key) {
                    let fill = if dark { [1.0, 1.0, 1.0, 0.15] } else { [0.0, 0.0, 0.0, 0.10] };
                    self.pill = Some((key, shaders.glass_shape(rect, fill, 4.0, false, dark, scale)));
                }
                out.extend(self.pill.as_ref().map(|(_, e)| SceneElement::Shader(e.clone())));
            }
            None => self.pill = None,
        }
        if let Some((_, glass)) = &self.glass {
            out.extend(glass.iter().cloned().map(SceneElement::Shader));
        }
        out
    }
}

/// `at` (global) relative to the display at `geo`, when it is on that display's bar.
pub fn on_bar(geo: Rect, at: smithay::utils::Point<f64, smithay::utils::Logical>) -> Option<smithay::utils::Point<f64, smithay::utils::Logical>> {
    let local = at - geo.loc.to_f64();
    (geo.to_f64().contains(at) && local.y < f64::from(height())).then_some(local)
}

/// Whether an auto-hiding bar shows with the pointer `y` px below its display's top: at the very edge, or anywhere
/// on the bar once `shown`.
pub fn reveals(shown: bool, y: f64) -> bool {
    (0.0..if shown { f64::from(height()) } else { 1.0 }).contains(&y)
}

/// The bar's elements moved `hidden` (0 to 1) of the way up out of sight, shadow included.
pub fn slid(renderer: &mut GlesRenderer, strip: Vec<SceneElement>, hidden: f64, scale: f64) -> Vec<SceneElement> {
    if hidden <= 0.0 {
        return strip;
    }
    super::motion::transformed(renderer, strip, scale, |mut rect| {
        rect.loc.y -= hidden * (rect.loc.y + rect.size.h);
        crate::anim::Look::at(rect)
    })
}

/// The part of the output windows may use: everything below the bar.
pub fn below(area: Rect) -> Rect {
    let cut = (height() - area.loc.y).clamp(0, area.size.h);
    Rectangle::new((area.loc.x, area.loc.y + cut).into(), Size::from((area.size.w, area.size.h - cut)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::sound::Device;

    #[test]
    fn symbols_follow_state() {
        let wifi_of = |link, wifi_enabled| wifi(&Network { wifi_enabled, link, networks: Vec::new() });
        assert_eq!(wifi_of(Link::Wifi { ssid: "x".into(), bars: 3 }, true), Symbol::WifiHigh);
        assert_eq!(wifi_of(Link::Wifi { ssid: "x".into(), bars: 1 }, true), Symbol::WifiLow);
        assert_eq!(wifi_of(Link::None, true), Symbol::WifiNone);
        assert_eq!(wifi_of(Link::None, false), Symbol::WifiSlash);
        assert_eq!(wifi_of(Link::Ethernet, false), Symbol::Ethernet);

        let battery_of = |percent, charging| battery(&Battery { percent, charging, on_battery: !charging, minutes_left: None });
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
    fn a_second_displays_bar_hits_in_its_own_coordinates() {
        // The second display sits right of a 1280 px first one; its clock is at its right edge.
        let second = Rectangle::new((1280, 0).into(), (1920, 1080).into());
        let bar = MenuBar {
            hits: vec![
                (BarItem::Logo, Rectangle::new((0, 0).into(), (40, height()).into())),
                (BarItem::Clock, Rectangle::new((1800, 0).into(), (120, height()).into())),
            ],
            ..Default::default()
        };
        let hit = |x: f64, y: f64| on_bar(second, (x, y).into()).and_then(|at| bar.hit(at));
        assert_eq!(hit(1280.0 + 1900.0, 10.0), Some(BarItem::Clock));
        assert_eq!(hit(1290.0, 5.0), Some(BarItem::Logo));
        assert_eq!(on_bar(second, (1290.0, 5.0).into()), Some((10.0, 5.0).into()));
        // Below its bar, or on the first display: not this bar.
        assert_eq!(on_bar(second, (1290.0, f64::from(height()) + 1.0).into()), None);
        assert_eq!(on_bar(second, (20.0, 5.0).into()), None);
    }

    #[test]
    fn app_names() {
        assert_eq!(app_name("org.example.no-such-app"), "No-such-app");
    }

    #[test]
    fn the_app_name_gives_way_to_the_right_group() {
        // 1280 wide with a usual right group: the full 300 px name fits.
        assert!(app_room(1280, 16, 400) >= APP_WIDTH);
        // 1024 wide with tray icons, a battery and a long clock: ellipsised to what is left, still clear of the group.
        let room = app_room(1024, 16, 700);
        assert!((MIN_APP..APP_WIDTH).contains(&room));
        assert_eq!(EDGE + 16 + LOGO_GAP + room + GAP, 1024 - EDGE - 700);
        // Portrait: the right group leaves nothing worth showing, so the name goes.
        assert!(app_room(600, 16, 540) < MIN_APP);
    }

    #[test]
    fn auto_hide_reveals_at_the_top_edge() {
        assert!(reveals(false, 0.0));
        assert!(!reveals(false, 5.0), "hidden: only the very edge brings it");
        assert!(reveals(true, 20.0), "shown: it stays while the pointer is on it");
        assert!(!reveals(true, 30.0));
        assert!(!reveals(true, -1.0), "above the display: another one's bottom");
    }

    #[test]
    fn tray_icons_overflow_from_the_left() {
        let tray = [16, 16, 16, 16];
        // Room for all: none hidden.
        assert_eq!(tray_overflow(1280, 16, 400, &tray, 10), 0);
        // Tight: just enough go that the name keeps its minimum beside the "»".
        let fixed = 1024 - 2 * EDGE - 16 - LOGO_GAP - GAP - MIN_APP - (10 + GAP) - 2 * (16 + GAP);
        assert_eq!(tray_overflow(1024, 16, fixed, &tray, 10), 2);
        // Nothing fits even with every icon gone: all of them are in the menu.
        assert_eq!(tray_overflow(600, 16, 700, &tray, 10), 4);
        assert_eq!(tray_overflow(600, 16, 700, &[], 10), 0);
    }

    #[test]
    fn the_bar_grows_with_the_text_size() {
        let area = Rectangle::new((0, 0).into(), (1280, 800).into());
        assert_eq!(height(), 24);
        crate::text::with_scale(1.5, || {
            // A 13 px label shapes into a 24 px line at 1.5: the 36 px bar holds it with room to spare.
            assert_eq!(height(), 36);
            assert_eq!(below(area), Rectangle::new((0, 36).into(), (1280, 764).into()));
            assert!(reveals(true, 30.0), "the whole taller bar keeps it shown");
            // Items space out and the app's name keeps a wider minimum.
            assert_eq!(app_room(1280, 16, 400), 1280 - 2 * EDGE - 16 - LOGO_GAP - 18 - 400);
            let tray = [24; 4];
            let fixed = 1024 - 2 * EDGE - 24 - LOGO_GAP - 18 - 60 - (15 + 18) - 2 * (24 + 18);
            assert_eq!(tray_overflow(1024, 24, fixed, &tray, 15), 2);
        });
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
