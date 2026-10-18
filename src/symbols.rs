//! Phosphor symbols (Part D: menu bar 16 px, Control Center 18–20, OSD 24), bundled in the binary and rasterised by
//! resvg on first use. Phosphor draws in `currentColor`, so one coverage mask is tinted to any colour.

use std::{collections::HashMap, sync::Arc};

use resvg::{tiny_skia, usvg};

use crate::text::Rendered;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Weight {
    Regular,
    /// The "on" state: an enabled toggle, the playing control.
    Fill,
}

macro_rules! symbols {
    ($($variant:ident = $file:literal,)*) => {
        /// One per file in `assets/phosphor/{regular,fill}/`.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[cfg_attr(not(test), expect(dead_code, reason = "most are for the Control Center and OSD (E2.3, E2.7)"))]
        pub enum Symbol {
            /// The Noobsbot mark (`assets/logo.svg`), same weight in both styles.
            Logo,
            $($variant,)*
        }

        impl Symbol {
            #[cfg(test)]
            const ALL: &[Symbol] = &[Symbol::Logo, $(Symbol::$variant,)*];

            fn svg(self, weight: Weight) -> &'static str {
                match (self, weight) {
                    (Symbol::Logo, _) => include_str!("../assets/logo.svg"),
                    $(
                        (Symbol::$variant, Weight::Regular) => include_str!(concat!("../assets/phosphor/regular/", $file, ".svg")),
                        (Symbol::$variant, Weight::Fill) => include_str!(concat!("../assets/phosphor/fill/", $file, ".svg")),
                    )*
                }
            }
        }
    };
}

symbols! {
    WifiHigh = "wifi-high",
    WifiMedium = "wifi-medium",
    WifiLow = "wifi-low",
    WifiNone = "wifi-none",
    WifiSlash = "wifi-slash",
    Bluetooth = "bluetooth",
    BluetoothSlash = "bluetooth-slash",
    BluetoothConnected = "bluetooth-connected",
    BatteryFull = "battery-full",
    BatteryHigh = "battery-high",
    BatteryMedium = "battery-medium",
    BatteryLow = "battery-low",
    BatteryEmpty = "battery-empty",
    BatteryCharging = "battery-charging",
    SpeakerHigh = "speaker-high",
    SpeakerLow = "speaker-low",
    SpeakerNone = "speaker-none",
    SpeakerMuted = "speaker-x",
    Speaker = "speaker-simple-high",
    Microphone = "microphone",
    MicrophoneSlash = "microphone-slash",
    Headphones = "headphones",
    Monitor = "monitor",
    Sun = "sun",
    Moon = "moon",
    CircleHalf = "circle-half",
    Toggle = "toggle-right",
    Sliders = "sliders-horizontal",
    Keyboard = "keyboard",
    Lock = "lock-simple",
    Check = "check",
    CaretRight = "caret-right",
    CaretLeft = "caret-left",
    CaretDown = "caret-down",
    Play = "play",
    Pause = "pause",
    SkipBack = "skip-back",
    SkipForward = "skip-forward",
    MusicNote = "music-note",
    Leaf = "leaf",
    Gauge = "gauge",
    Lightning = "lightning",
    Ethernet = "plugs-connected",
    Network = "network",
    Bell = "bell",
    BellSlash = "bell-slash",
    X = "x",
}

type Key = (Symbol, Weight, u16, u64, [u8; 4]);

/// Rasterised symbols by symbol, weight, logical size, scale and colour.
// ponytail: never evicts; the key space is a few dozen symbols in a handful of sizes and colours. Add the text cache's
// LRU if sizes or themes multiply.
#[derive(Default)]
pub struct Symbols {
    cache: HashMap<Key, Arc<Rendered>>,
}

impl Symbols {
    /// A `size`×`size` logical px symbol in straight RGBA `color`; draw it with `text::to_buffer`.
    pub fn get(&mut self, symbol: Symbol, weight: Weight, size: u16, scale: f64, color: [u8; 4]) -> Arc<Rendered> {
        let key = (symbol, weight, size, scale.to_bits(), color);
        self.cache.entry(key).or_insert_with(|| Arc::new(rasterise(symbol.svg(weight), size, scale, color))).clone()
    }
}

fn rasterise(svg: &str, size: u16, scale: f64, color: [u8; 4]) -> Rendered {
    let px = (f64::from(size) * scale).round().max(1.0) as u32;
    let empty = Rendered { pixels: Vec::new(), width: 0, height: 0, baseline: 0, truncated: false };
    let Ok(tree) = usvg::Tree::from_str(svg, &usvg::Options::default()) else { return empty };
    let Some(mut pixmap) = tiny_skia::Pixmap::new(px, px) else { return empty };
    let fit = px as f32 / tree.size().width();
    resvg::render(&tree, tiny_skia::Transform::from_scale(fit, fit), &mut pixmap.as_mut());
    let [r, g, b, a] = color.map(u32::from);
    let mut pixels = pixmap.take();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let coverage = u32::from(pixel[3]) * a / 255;
        pixel.copy_from_slice(&[r, g, b, 255].map(|v| (v * coverage / 255) as u8));
    }
    Rendered { pixels, width: px, height: px, baseline: px, truncated: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_symbol_renders_tinted() {
        let mut symbols = Symbols::default();
        for &symbol in Symbol::ALL {
            for weight in [Weight::Regular, Weight::Fill] {
                let r = symbols.get(symbol, weight, 16, 2.0, [255, 0, 0, 128]);
                assert_eq!((r.width, r.height), (32, 32));
                let ink: Vec<_> = r.pixels.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).collect();
                assert!(!ink.is_empty(), "{symbol:?} {weight:?} is blank");
                // Premultiplied red at half alpha: R equals A, never above 128; no green or blue.
                assert!(ink.iter().all(|p| p[0] == p[3] && p[3] <= 128 && p[1] == 0 && p[2] == 0), "{symbol:?} {weight:?}");
            }
        }
        assert!(Arc::ptr_eq(
            &symbols.get(Symbol::Sun, Weight::Fill, 16, 2.0, [255, 0, 0, 128]),
            &symbols.get(Symbol::Sun, Weight::Fill, 16, 2.0, [255, 0, 0, 128])
        ));
    }
}
