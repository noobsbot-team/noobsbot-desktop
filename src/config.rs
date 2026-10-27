//! The settings file (E1.8, C11): `~/.config/noobsbot-desktop/config.toml`, parsed with serde + toml and live-reloaded.
//! It never stops the desktop: a missing file means defaults, a broken one is logged and notified with its line and
//! the last good config stays, unknown keys are warned about and skipped.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

use notify::{
    EventKind, RecursiveMode, Watcher,
    event::{AccessKind, AccessMode},
};
use serde::{Deserialize, Serialize};

use crate::rules::{AllSpaces, Layer, Length, RULE_KEYS, Rule, Space};

/// The commented defaults shipped as `data/config.toml`; parsing it yields `Config::default()` (tested).
pub const DEFAULT_TOML: &str = include_str!("../data/config.toml");

/// Reload waits until the file has been quiet this long, so an editor's two-step save is read once, complete.
const DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Config {
    /// File format version; older files are migrated on load (C11, `migrate`).
    pub version: u32,
    /// Opened by Super+Enter and the `terminal` action.
    pub terminal: String,
    /// Software Update asks GitHub for a newer release once a day (E6.13); read at start.
    pub check_updates: bool,
    pub appearance: Appearance,
    pub night_light: NightLight,
    pub accessibility: Accessibility,
    pub keyboard: Keyboard,
    pub touchpad: Touchpad,
    pub mouse: Mouse,
    pub hot_corners: HotCorners,
    pub display: Displays,
    pub windows: Windows,
    pub spaces: Spaces,
    pub clock: Clock,
    pub menu_bar: MenuBar,
    pub dock: Dock,
    pub idle: Idle,
    pub battery: Battery,
    pub sound: Sound,
    pub privacy: Privacy,
    pub weather: Weather,
    pub widgets: Widgets,
    pub calendar: Calendar,
    pub update: Update,
    pub desktop: Desktop,
    pub housekeeping: Housekeeping,
    pub notifications: Notifications,
    /// App name (as it sends notifications) → how they show.
    pub notification_apps: BTreeMap<String, AppRule>,
    pub app_drawer: AppDrawer,
    pub network: Network,
    /// Key combination (`"Super+Return"`) → action (`"close"`, `"exec helium-browser"`, …). Parsed by input.rs.
    pub bindings: BTreeMap<String, String>,
    /// Window rules (E1.9); compile with `rules::compile` after every load.
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub mode: Mode,
    pub accent: Accent,
    /// Multiplier on every glass material's opacity, 0.0 … 1.0.
    pub glass_opacity: f32,
    /// PNG or JPEG for light mode; empty = the built-in opal gradient.
    pub wallpaper: String,
    /// PNG or JPEG for dark mode; empty = the built-in dark opal gradient.
    pub wallpaper_dark: String,
    /// The Control Center's Dark Mode toggle also rewrites the Qt app theme (E2.9).
    pub theme_apps: bool,
    /// Every movement becomes a 150 ms crossfade (D10).
    pub reduce_motion: bool,
    /// How the wallpaper picture meets the screen.
    pub wallpaper_fit: WallpaperFit,
    /// Settings' wallpaper gallery also shows these folders (`:`-separated); the desktop doesn't read it.
    pub gallery_folders: String,
    /// Slideshow: a folder of PNG/JPEG pictures shown in turn (instead of the two files above); empty = off.
    pub wallpaper_folder: String,
    /// Minutes per picture; 0 = a new one only at login.
    pub wallpaper_interval: u32,
    /// Random order instead of by name.
    pub wallpaper_shuffle: bool,
    /// `auto` is dark from this hour until `dark_until` (0 … 23, local time).
    pub dark_from: u32,
    pub dark_until: u32,
    /// Multiplier on the shell's label sizes, 0.8 … 1.5; also GTK `text-scaling-factor` and the Qt font size.
    pub text_scale: f32,
    /// Pointer size in px; 0 = `$XCURSOR_SIZE`, else 24. Apps we start get it as `XCURSOR_SIZE`.
    pub pointer_size: u32,
    /// `auto` is dark from sunset to sunrise at the weather place instead (the hours without one).
    pub auto_sunset: bool,
}

/// Night Light (E6.2, E8.4): the screen warmed on a schedule.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct NightLight {
    /// The schedule is on; the Control Center turns it on or off until the schedule next turns.
    pub enabled: bool,
    /// Kelvin, 1000 (warmest) … 6500 (none).
    pub temperature: u32,
    pub schedule: NightSchedule,
    /// `custom`: from this hour until `until` (0 … 23, local time).
    pub from: u32,
    pub until: u32,
}

impl Default for NightLight {
    fn default() -> Self {
        Self { enabled: false, temperature: 3400, schedule: NightSchedule::Custom, from: 19, until: 7 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NightSchedule {
    #[default]
    Custom,
    /// Sunset to sunrise at the weather place; the hours without one.
    Sunset,
}

/// Accessibility (E8.6).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Accessibility {
    /// Zoom (Super+Alt+8) magnifies the whole screen, or a lens around the pointer.
    pub zoom_style: ZoomStyle,
    /// Zoom also follows the text caret and keyboard focus, not only the pointer.
    pub zoom_follows_focus: bool,
    /// The whole screen through a colour filter; screenshots stay true.
    pub colour_filter: ColourFilter,
    /// Glass near-opaque, with a visible 1 px border.
    pub increase_contrast: bool,
    /// A tapped modifier holds for the next key; tapped twice it stays until tapped again.
    pub sticky_keys: bool,
    /// Pressing Shift five times turns sticky keys on or off.
    pub sticky_keys_shortcut: bool,
    /// A key counts only once held this long, in ms; 0 = off.
    pub slow_keys_ms: u32,
    /// A key pressed again within this many ms of letting it go is ignored; 0 = off.
    pub bounce_keys_ms: u32,
    /// The keypad moves the pointer: 1–9 move, 5 clicks, 0 holds the button, . lets it go, * right-clicks.
    pub mouse_keys: bool,
    /// What an app ringing the bell does.
    pub bell: Bell,
    /// The on-screen keyboard always shows (the `on-screen-keyboard` action shows it for a while).
    pub on_screen_keyboard: bool,
    /// Shaking the pointer briefly makes it bigger.
    pub shake_to_find: bool,
    /// Resting the pointer this long clicks; 0 = off.
    pub dwell_click_ms: u32,
    /// Holding the primary button still this long gives a secondary click instead; 0 = off.
    pub hold_secondary_ms: u32,
    /// Runs Orca while on (Super+Alt+F5).
    pub screen_reader: bool,
}

impl Default for Accessibility {
    fn default() -> Self {
        Self {
            zoom_style: ZoomStyle::Full,
            zoom_follows_focus: true,
            colour_filter: ColourFilter::None,
            increase_contrast: false,
            sticky_keys: false,
            sticky_keys_shortcut: false,
            slow_keys_ms: 0,
            bounce_keys_ms: 0,
            mouse_keys: false,
            bell: Bell::Sound,
            on_screen_keyboard: false,
            shake_to_find: true,
            dwell_click_ms: 0,
            hold_secondary_ms: 0,
            screen_reader: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ZoomStyle {
    #[default]
    Full,
    Lens,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColourFilter {
    #[default]
    None,
    Invert,
    /// Light and dark swap, colours keep their hue.
    InvertHue,
    Greyscale,
}

/// The menu bar (E8.8).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct MenuBar {
    /// Slides away until the pointer touches the top edge or a menu opens; zoomed windows get the full height.
    pub auto_hide: bool,
    /// The current Space in the bar, by the status items; a click opens Mission Control (E5.1).
    pub space_indicator: SpaceIndicator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpaceIndicator {
    #[default]
    Off,
    /// Its number, or a fullscreen app's name.
    Number,
    /// A dot per Space on this display, the current one filled.
    Dots,
}

/// The menu-bar clock (E2.4) and the system time zone (E2.6).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct Clock {
    /// IANA zone (`Asia/Kathmandu`); empty = leave the system's zone alone.
    pub timezone: String,
    #[serde(flatten)]
    pub format: crate::clock::Format,
    /// The calendar's first column (E2.5).
    pub week_start: WeekStart,
    /// ISO week numbers in a column at the calendar's left.
    pub week_numbers: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WeekStart {
    /// The locale's first weekday (LC_TIME).
    #[default]
    Auto,
    Sunday,
    Monday,
    Saturday,
}

impl From<WeekStart> for chrono::Weekday {
    fn from(start: WeekStart) -> Self {
        match start {
            WeekStart::Auto => crate::calendar::locale_week_start(),
            WeekStart::Sunday => chrono::Weekday::Sun,
            WeekStart::Monday => chrono::Weekday::Mon,
            WeekStart::Saturday => chrono::Weekday::Sat,
        }
    }
}

/// Wallpaper fit (Fill Screen, Fit to Screen, Centre, Stretch to Fill Screen, Tile).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WallpaperFit {
    /// Covers the screen, cropping the long side.
    Fill,
    /// The whole picture, bands of the desktop colour beside it.
    Fit,
    /// At its own pixel size, in the middle.
    #[serde(alias = "center")]
    Centre,
    Stretch,
    /// At its own pixel size, repeated from the top-left corner.
    Tile,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Light,
    Dark,
    Auto,
}

/// Part D's accent set; theme.rs maps each to its light and dark colour.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Accent {
    Blue,
    Purple,
    Pink,
    Red,
    Orange,
    Yellow,
    Green,
    Graphite,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Keyboard {
    /// xkb RMLVO values; empty = the system's (localectl / vconsole.conf).
    pub layout: String,
    pub variant: String,
    pub options: String,
    /// Key repeat: held this many ms, then this many times a second; 0 = off.
    pub repeat_delay: u32,
    pub repeat_rate: u32,
    /// Each window keeps the layout it had when it lost focus, and gets it back on focus.
    pub per_window_source: bool,
    /// The input-method engine: "fcitx5", "ibus", "auto" (fcitx5 if installed, else ibus) or "none".
    pub input_method: String,
}

impl Default for Keyboard {
    fn default() -> Self {
        Self {
            layout: String::new(),
            variant: String::new(),
            options: String::new(),
            repeat_delay: 500,
            repeat_rate: 30,
            per_window_source: false,
            input_method: "fcitx5".into(),
        }
    }
}

impl Keyboard {
    /// The repeat delay and rate apps are told (smithay's order: rate, delay).
    pub fn repeat(&self) -> (i32, i32) {
        (self.repeat_rate.min(100) as i32, self.repeat_delay.clamp(100, 3000) as i32)
    }

    /// `input_method` resolved: "fcitx5", "ibus" or "none".
    pub fn engine(&self) -> &'static str {
        crate::ibus::engine(&self.input_method)
    }

    /// The xkb part changed (not just repeat), so the keymap is rebuilt.
    pub fn xkb_changed(&self, old: &Keyboard) -> bool {
        (&self.layout, &self.variant, &self.options) != (&old.layout, &old.variant, &old.options)
    }
}

/// Where `localectl` keeps the system layout; the X11 file is the one `set-x11-keymap` writes.
const SYSTEM_KEYBOARD_FILES: [&str; 2] = ["/etc/X11/xorg.conf.d/00-keyboard.conf", "/etc/vconsole.conf"];

impl Keyboard {
    /// This section with empty fields filled from the system: layout and variant together, options on their own.
    pub fn resolved(&self) -> Keyboard {
        let system = SYSTEM_KEYBOARD_FILES.iter().filter_map(|p| fs::read_to_string(p).ok()).map(|t| parse_system_keyboard(&t)).find(|k| !k.layout.is_empty());
        let system = system.unwrap_or_default();
        let (layout, variant) = if self.layout.is_empty() { (system.layout, system.variant) } else { (self.layout.clone(), self.variant.clone()) };
        let options = if self.options.is_empty() { system.options } else { self.options.clone() };
        Keyboard { layout, variant, options, ..self.clone() }
    }
}

/// Reads XKBLAYOUT/XKBVARIANT/XKBOPTIONS from `vconsole.conf`, or `Option "XkbLayout" "us"` lines from an xorg.conf.
fn parse_system_keyboard(text: &str) -> Keyboard {
    let mut keyboard = Keyboard::default();
    for line in text.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
        let pair = match line.strip_prefix("Option") {
            Some(rest) => {
                let mut quoted = rest.split('"').skip(1).step_by(2);
                quoted.next().zip(quoted.next())
            }
            None => line.split_once('=').map(|(k, v)| (k.trim(), v.trim().trim_matches(['"', '\'']))),
        };
        let Some((key, value)) = pair else { continue };
        match key.to_ascii_lowercase().as_str() {
            "xkblayout" => keyboard.layout = value.into(),
            "xkbvariant" => keyboard.variant = value.into(),
            "xkboptions" => keyboard.options = value.into(),
            _ => {}
        }
    }
    keyboard
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Touchpad {
    /// Off: the trackpad does nothing (the touchpad-toggle key flips it until the next reload).
    pub enabled: bool,
    pub tap_to_click: bool,
    pub natural_scroll: bool,
    /// Fingers of the Space / Mission Control swipes (E5.3): 3, or 4 to leave 3-finger swipes to apps.
    pub swipe_fingers: u8,
    /// Multiplies gesture travel: higher = a shorter swipe or pinch is enough (0.25 … 4).
    pub gesture_sensitivity: f64,
    /// Tracking speed, -1 (slow) … 1 (fast).
    pub speed: f64,
    pub secondary_click: SecondaryClick,
    pub scroll_method: ScrollMethod,
    /// Tap, then drag with the finger down; `drag_lock` keeps dragging through a short lift.
    pub tap_to_drag: bool,
    pub drag_lock: bool,
    /// Three fingers drag (libinput's 3-finger drag); the system swipes then take four.
    pub three_finger_drag: bool,
    pub disable_while_typing: bool,
    /// Off while a mouse is plugged in.
    pub ignore_with_mouse: bool,
    /// Each system gesture on or off: swipe between Spaces, up for Mission Control, down for App Exposé, pinch for
    /// Launchpad, spread for the desktop.
    pub gesture_spaces: bool,
    pub gesture_mission_control: bool,
    pub gesture_app_expose: bool,
    pub gesture_launchpad: bool,
    pub gesture_show_desktop: bool,
}

impl Touchpad {
    /// Fingers of the system swipes: four while three-finger drag holds three (libinput makes those swipes drags).
    pub fn effective_swipe_fingers(&self) -> u8 {
        if self.three_finger_drag { 4 } else { self.swipe_fingers }
    }

    /// Whether this system gesture is turned on.
    pub fn allows(&self, gesture: crate::gestures::Gesture) -> bool {
        use crate::gestures::Gesture::*;
        match gesture {
            Space(_) => self.gesture_spaces,
            MissionControl => self.gesture_mission_control,
            AppExpose => self.gesture_app_expose,
            Launchpad => self.gesture_launchpad,
            ShowDesktop => self.gesture_show_desktop,
        }
    }
}

/// How the trackpad scrolls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScrollMethod {
    /// Two fingers anywhere.
    TwoFingers,
    /// One finger along the right or bottom edge.
    Edge,
    /// The trackpad doesn't scroll.
    None,
}

/// How the trackpad right-clicks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecondaryClick {
    /// Click with two fingers.
    TwoFingers,
    /// Click in the bottom-right corner.
    Corner,
    /// Every physical click is a primary click (a two-finger tap still right-clicks with tap to click).
    Off,
}

/// Mice and trackballs (libinput).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Mouse {
    /// Tracking speed, -1 (slow) … 1 (fast).
    pub speed: f64,
    /// Faster movement goes further (adaptive); off = flat.
    pub acceleration: bool,
    /// Primary button on the right; the trackpad follows.
    pub left_handed: bool,
    pub natural_scroll: bool,
    /// Left and right together act as the middle button.
    pub middle_click_emulation: bool,
    /// Two clicks closer than this (ms) are a double-click: our title bars, desktop icons and edges, and GTK apps.
    pub double_click_ms: u32,
    /// Middle click pastes the last selected text (the primary selection); off: apps never see it.
    pub middle_click_paste: bool,
}

impl Default for Mouse {
    fn default() -> Self {
        Self {
            speed: 0.0,
            acceleration: true,
            left_handed: false,
            natural_scroll: false,
            middle_click_emulation: false,
            double_click_ms: 400,
            middle_click_paste: true,
        }
    }
}

impl Mouse {
    /// The double-click time, kept between 150 ms and 1 s.
    pub fn double_click(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.double_click_ms.clamp(150, 1000).into())
    }
}

/// Hot corners (E5.4): an action name per corner (as in `[bindings]`); "none" = off.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct HotCorners {
    pub top_left: String,
    pub top_right: String,
    pub bottom_left: String,
    pub bottom_right: String,
}

impl Default for HotCorners {
    fn default() -> Self {
        let none = || "none".to_string();
        Self { top_left: none(), top_right: none(), bottom_left: none(), bottom_right: none() }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Displays {
    /// The GPU that draws the desktop (`/dev/dri/card1`); empty: the one the firmware booted with. Read at start.
    pub gpu: String,
    /// Every display's scale (1.0 = 100%, 1.25, 1.5, 2.0 …); 0 = picked from each display's pixel density. Live.
    pub scale: f64,
    /// `[[display.outputs]]`: one display's own settings, by connector name. Live.
    pub outputs: Vec<OutputSettings>,
}

impl Default for Displays {
    fn default() -> Self {
        Self { gpu: String::new(), scale: 1.15, outputs: Vec::new() }
    }
}

/// One display's settings (E6.2); a display without an entry gets the defaults below.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct OutputSettings {
    /// The connector (`eDP-1`, `HDMI-A-1`, `DP-2`; `winit` when nested).
    pub name: String,
    pub enabled: bool,
    /// `"1920x1080"`; empty = the display's preferred resolution.
    pub mode: String,
    /// Hz (`59.95`); 0 = the highest the resolution offers.
    pub refresh: f64,
    /// Overrides `[display] scale`; 0 = that one.
    pub scale: f64,
    /// Degrees clockwise: 0, 90, 180 or 270.
    pub rotation: u16,
    /// Top-left corner in logical px; unset = right of the others.
    pub position: Option<[i32; 2]>,
    /// The menu bar and dock go here.
    pub primary: bool,
    /// Mirror: show this connector's picture instead of extending the desktop; empty = extended.
    pub mirror: String,
    /// Variable refresh rate (adaptive sync), where the display and its connector support it.
    pub vrr: bool,
    /// Percent (0–10) the picture shrinks by, centred with black around it, for TVs that crop the edges.
    pub underscan: u8,
}

/// The keys an `[[display.outputs]]` entry may have (unknown ones are warned about).
const OUTPUT_KEYS: [&str; 11] = ["name", "enabled", "mode", "refresh", "scale", "rotation", "position", "primary", "mirror", "vrr", "underscan"];

impl Default for OutputSettings {
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: true,
            mode: String::new(),
            refresh: 0.0,
            scale: 0.0,
            rotation: 0,
            position: None,
            primary: false,
            mirror: String::new(),
            vrr: false,
            underscan: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Windows {
    /// Held while dragging anywhere in a window to move it.
    pub move_modifier: Modifier,
    pub magnetic_edges: bool,
    pub snapping: bool,
    pub double_click_title: TitleAction,
    /// Spaces (E5.1): 0 = dynamic (start with one, `+` in Mission Control adds more); 2–16 = that many, fixed.
    pub spaces: u8,
    /// Which window gets the keyboard: the one clicked, or (advanced) the one under the pointer.
    pub focus: FocusMode,
    /// The Log Out alert's "Reopen windows when logging back in" box, as last left (E8.8).
    pub reopen_at_login: bool,
    /// The `nudge` and `resize` shortcuts (E1.2): off, their keys go to the app.
    pub keyboard_nudge: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FocusMode {
    Click,
    Hover,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modifier {
    Super,
    Alt,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleAction {
    Zoom,
    #[serde(alias = "minimize")]
    Minimise,
    Nothing,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: VERSION,
            terminal: "noobsbot".into(),
            check_updates: true,
            appearance: Appearance::default(),
            night_light: NightLight::default(),
            accessibility: Accessibility::default(),
            keyboard: Keyboard::default(),
            touchpad: Touchpad::default(),
            mouse: Mouse::default(),
            hot_corners: HotCorners::default(),
            display: Displays::default(),
            windows: Windows::default(),
            clock: Clock::default(),
            menu_bar: MenuBar::default(),
            dock: Dock::default(),
            idle: Idle::default(),
            battery: Battery::default(),
            sound: Sound::default(),
            privacy: Privacy::default(),
            weather: Weather::default(),
            widgets: Widgets::default(),
            calendar: Calendar::default(),
            update: Update::default(),
            desktop: Desktop::default(),
            housekeeping: Housekeeping::default(),
            spaces: Spaces::default(),
            notifications: Notifications::default(),
            notification_apps: BTreeMap::new(),
            app_drawer: AppDrawer::default(),
            network: Network::default(),
            bindings: default_bindings(),
            rules: vec![
                Rule {
                    app_id: Some(crate::rules::QUAKE_ID.into()),
                    title: None,
                    actions: crate::rules::Actions {
                        space: Some(Space::All(AllSpaces::All)),
                        size: Some([Length::Fraction(1.0), Length::Fraction(0.4)]),
                        position: Some([Length::Px(0), Length::Px(0)]),
                        decorations: Some(false),
                        layer: Some(Layer::Overlay),
                        ..Default::default()
                    },
                },
                Rule {
                    app_id: None,
                    title: Some("^Picture in picture$".into()),
                    actions: crate::rules::Actions { float_above: Some(true), ..Default::default() },
                },
            ],
        }
    }
}

/// The Weather widget (E7.2): off until turned on, and then only for the place given here.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Weather {
    pub enabled: bool,
    /// Geocoded once by Open-Meteo; empty = `latitude`/`longitude`.
    pub city: String,
    pub latitude: f64,
    pub longitude: f64,
    pub units: Units,
}

/// Where widgets from other processes show (E8.8), by id, in order.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Widgets {
    /// Notification Center's order; widgets in neither list follow these.
    pub center: Vec<String>,
    /// On the desktop instead, top to bottom.
    pub desktop: Vec<String>,
    /// Shown nowhere unless also in `center` or `desktop`.
    pub hidden: Vec<String>,
    /// The World Clock widget's IANA zones (`Europe/London`), a row each; empty = no widget.
    pub clocks: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Units {
    #[default]
    Celsius,
    Fahrenheit,
}

/// The dock (E3.1); defaults are the owner's Plank settings.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Dock {
    /// Pinned apps, left to right: anything `apps::Apps::find` takes.
    pub items: Vec<String>,
    pub icon_size: u16,
    pub zoom: bool,
    pub zoom_percent: u16,
    pub hide: DockHide,
    /// Plank's hide-delay / unhide-delay, in ms.
    pub hide_delay: u32,
    pub unhide_delay: u32,
    /// No dragging icons in, out or around, and no Keep in Dock.
    pub lock_items: bool,
    /// Only pinned apps, no other running ones but those with a minimised window.
    pub pinned_only: bool,
    /// Running apps and their windows from the current Space only.
    pub current_workspace_only: bool,
    /// The app's name over the hovered icon.
    pub tooltips: bool,
    /// Clicking the focused app's icon (E8.3).
    pub click: DockClick,
    /// Middle-clicking an icon.
    pub middle: DockMiddle,
    /// Scrolling on an icon cycles its app's windows.
    pub scroll: DockScroll,
    /// Cycling by click or scroll goes on past an app's last window into the next app's.
    pub cycle_across_apps: bool,
    /// How a window goes into the dock and comes back (E5.5).
    pub minimise_effect: MinimiseEffect,
    /// Options ▸ Assign To, per dock app (item key): `"all"` (every Space) or `"<display>:<n>"` (its new windows
    /// open on Space n of that display).
    pub assign: BTreeMap<String, String>,
    /// Which displays have a dock (D13).
    pub displays: DockDisplays,
    /// A display's own pinned list, by connector name, in place of `items` (the dock menu's Separate Items on This
    /// Display). Kept while the display is away.
    pub outputs: BTreeMap<String, Vec<String>>,
}

impl Dock {
    /// The pinned list of `display`'s dock: its own, else the shared `items`.
    pub fn items_on(&self, display: Option<&str>) -> &[String] {
        display.and_then(|d| self.outputs.get(d)).unwrap_or(&self.items)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DockDisplays {
    /// One dock per display.
    #[default]
    All,
    /// The main display only.
    Main,
    /// The display the pointer is on: the dock follows it.
    Pointer,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MinimiseEffect {
    /// It shrinks and slides into its dock spot.
    #[default]
    Scale,
    /// It narrows into its dock spot, bottom first.
    Genie,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DockClick {
    /// Minimise its windows.
    #[default]
    Toggle,
    /// Its next window.
    Cycle,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DockMiddle {
    /// A new window.
    #[default]
    New,
    /// Close the app's windows.
    Close,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DockScroll {
    Off,
    /// Down or right: the next window.
    #[default]
    On,
    Reverse,
}

/// Notifications (E4.1, E8.2).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Notifications {
    /// Do Not Disturb: banners stay quiet, Notification Center still collects them.
    pub dnd: bool,
    /// Do Not Disturb turns on at `dnd_from` and off at `dnd_until` (whole hours, local; across midnight when
    /// `dnd_from` is the later one).
    pub dnd_scheduled: bool,
    pub dnd_from: u32,
    pub dnd_until: u32,
    /// Banners while a full-screen window has focus.
    pub full_screen: bool,
    /// When banners and Notification Center show a notification's body.
    pub previews: Previews,
}

impl Default for Notifications {
    fn default() -> Self {
        Self { dnd: false, dnd_scheduled: false, dnd_from: 22, dnd_until: 7, full_screen: false, previews: Previews::Always }
    }
}

impl Notifications {
    /// Does the schedule want Do Not Disturb on at `hour` o'clock?
    pub fn dnd_at(&self, hour: u32) -> bool {
        self.dnd_scheduled && crate::sun::within(self.dnd_from, self.dnd_until, hour)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Previews {
    #[default]
    Always,
    Unlocked,
    Never,
}

impl Previews {
    /// Is a notification's body shown, `locked` or not?
    pub fn show(self, locked: bool) -> bool {
        self == Previews::Always || (self == Previews::Unlocked && !locked)
    }
}

/// One app's notification style.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppStyle {
    /// Goes after a few seconds.
    #[default]
    Banner,
    /// Stays until dismissed.
    Alert,
    /// Notification Center only.
    None,
    /// Dropped.
    Off,
}

/// One app's `[notification_apps]` rule: a style alone (`"alert"`), or a table with the switches as well
/// (`{ style = "alert", sound = false, badge = false, center = false, lock = true }`); missing switches are on, except
/// `lock`, which is off (the lock screen shows only a count unless an app is allowed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(from = "RuleToml")]
pub struct AppRule {
    pub style: AppStyle,
    /// A sound as it arrives.
    pub sound: bool,
    /// The count badge on its dock icon.
    pub badge: bool,
    /// Kept in Notification Center; off, it goes with its banner.
    pub center: bool,
    /// Listed on the lock screen (title, and body with `previews = "always"`), not only counted.
    pub lock: bool,
}

impl Default for AppRule {
    fn default() -> Self {
        Self { style: AppStyle::Banner, sound: true, badge: true, center: true, lock: false }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RuleToml {
    Style(AppStyle),
    Table {
        #[serde(default)]
        style: AppStyle,
        #[serde(default = "yes")]
        sound: bool,
        #[serde(default = "yes")]
        badge: bool,
        #[serde(default = "yes")]
        center: bool,
        #[serde(default)]
        lock: bool,
    },
}

fn yes() -> bool {
    true
}

impl From<RuleToml> for AppRule {
    fn from(rule: RuleToml) -> Self {
        match rule {
            RuleToml::Style(style) => AppRule { style, ..Default::default() },
            RuleToml::Table { style, sound, badge, center, lock } => AppRule { style, sound, badge, center, lock },
        }
    }
}

/// Desktop icons (E7.2): the files in `~/Desktop` on the wallpaper.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Desktop {
    pub icons: bool,
    /// Mounted drives as icons too, above the files.
    pub show_drives: bool,
}

impl Default for Desktop {
    fn default() -> Self {
        Self { icons: true, show_drives: true }
    }
}

/// Daily cleanups (E8.8); 0 days = never.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Housekeeping {
    /// Trash items deleted more than this many days ago are removed for good.
    pub empty_trash_days: u32,
    /// Recent-files entries last used more than this many days ago are dropped.
    pub recent_days: u32,
}

impl Default for Housekeeping {
    fn default() -> Self {
        Self { empty_trash_days: 30, recent_days: 30 }
    }
}

/// Spaces across displays (E8.7).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Spaces {
    /// Each display has its own Spaces; false: one set every display switches together.
    pub per_display: bool,
}

impl Default for Spaces {
    fn default() -> Self {
        Self { per_display: true }
    }
}

/// The app drawer (a classic application menu).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct AppDrawer {
    /// Pressing and releasing Super on its own opens it.
    pub super_tap: bool,
}

impl Default for AppDrawer {
    fn default() -> Self {
        Self { super_tap: true }
    }
}

/// `[network]`: the proxy apps we start are given (`src/proxy.rs`).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Network {
    pub proxy: ProxyMode,
    /// `host:port` of each proxy (manual); empty = none for that kind.
    pub proxy_http: String,
    pub proxy_https: String,
    pub proxy_socks: String,
    /// Hosts and networks reached directly, comma separated.
    pub proxy_bypass: String,
    /// The proxy auto-config (PAC) file's URL (automatic).
    pub proxy_pac: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    #[default]
    None,
    Manual,
    /// From a PAC file: only apps that read the GTK proxy settings follow it.
    Auto,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Bell {
    #[default]
    Sound,
    /// The screen flashes white.
    Flash,
    Both,
    Off,
}

/// Screen-recording answers (E6.11), by program path; the "would like to record this screen" alert fills them in.
/// Calendar events (E7.2): `.ics` files, dirs or `dir/*.ics` patterns, read live.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Calendar {
    pub files: Vec<String>,
    /// CalDAV calendars (`caldav.rs`); each password is in the keyring, never here.
    pub caldav: Vec<CalDav>,
}

impl Default for Calendar {
    fn default() -> Self {
        Self { files: vec!["~/.local/share/noobsbot-desktop/calendars/*.ics".into()], caldav: Vec::new() }
    }
}

/// One CalDAV account: its server's address (its calendars are found) or one calendar's URL, and the login's user.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct CalDav {
    pub url: String,
    pub user: String,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Privacy {
    pub screen_recording: Vec<String>,
    pub screen_recording_denied: Vec<String>,
    /// Programs that may type and click through a virtual keyboard or pointer, and those denied.
    pub input_control: Vec<String>,
    pub input_control_denied: Vec<String>,
}

/// Software Update (E6.13); a changed channel checks again at once.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Update {
    pub channel: crate::services::update::Channel,
    /// A verified download installs at the next login, before the desktop starts, without asking for the password.
    pub install_at_login: bool,
}

/// Plank's hide modes, by Plank's names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DockHide {
    /// Hides while a window of the focused app overlaps it.
    Intelligent,
    /// Hides unless the pointer is on it.
    Auto,
    /// Hides while the focused window is maximised over it (or the focused app's dialog overlaps it).
    DodgeMaximized,
    /// Hides while any window overlaps it.
    WindowDodge,
    /// Hides while the focused window overlaps it.
    DodgeActive,
    /// Never hides, and windows keep clear of it (Plank's "none").
    #[serde(alias = "none")]
    Never,
}

impl Default for Dock {
    fn default() -> Self {
        let items = ["launchpad", "google-chrome", "com.microsoft.VSCode", "helium-browser", "dolphin", "kate", "screenshot", "kitty", "noobsbot"];
        Self {
            items: items.map(String::from).to_vec(),
            icon_size: 84,
            zoom: true,
            zoom_percent: 150,
            hide: DockHide::Intelligent,
            hide_delay: 0,
            unhide_delay: 0,
            lock_items: false,
            pinned_only: false,
            current_workspace_only: false,
            tooltips: true,
            click: DockClick::Toggle,
            middle: DockMiddle::New,
            scroll: DockScroll::On,
            cycle_across_apps: false,
            minimise_effect: MinimiseEffect::Scale,
            assign: BTreeMap::new(),
            displays: DockDisplays::All,
            outputs: BTreeMap::new(),
        }
    }
}

/// Sound effects (E8.2).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Sound {
    /// The alert sound: a freedesktop sound theme name (`bell`, `dialog-warning`), played by the bell.
    pub alert: String,
    /// The alert sound's volume, 0 … 100 percent.
    pub alert_volume: u8,
    /// Screenshots and the volume keys make their sounds.
    pub ui_sounds: bool,
    /// A sound as the session starts (once per login).
    pub startup_sound: bool,
}

impl Default for Sound {
    fn default() -> Self {
        Self { alert: "bell".into(), alert_volume: 100, ui_sounds: true, startup_sound: false }
    }
}

/// Battery (E8.2).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Battery {
    /// The percentage beside the menu bar's battery symbol.
    pub show_percentage: bool,
    /// Unplugging lowers the brightness a step (`idle::battery_dim`); plugging in gives it back.
    pub dim_on_battery: bool,
}

impl Default for Battery {
    fn default() -> Self {
        Self { show_percentage: true, dim_on_battery: true }
    }
}

/// Idle and sleep (E6.5): seconds without input before each step; 0 = never.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Idle {
    pub dim: u32,
    /// The screen turns off: on the power adapter, and on battery.
    pub screen_off: u32,
    pub screen_off_on_battery: u32,
    /// The keyboard backlight goes off (back on at the next input); at the latest when the screen dims.
    pub keyboard_backlight_off: u32,
    pub sleep_on_battery: u32,
    pub sleep_on_ac: u32,
    /// Lock when the screen turns off.
    pub lock: bool,
    /// Seconds after the screen turns off before it locks; input meanwhile wakes it unlocked.
    pub lock_delay: u32,
    /// Shown on the lock screen, under the clock.
    pub lock_message: String,
}

impl Default for Idle {
    fn default() -> Self {
        Self {
            dim: 120,
            screen_off: 300,
            keyboard_backlight_off: 30,
            screen_off_on_battery: 180,
            sleep_on_battery: 900,
            sleep_on_ac: 0,
            lock: true,
            lock_delay: 0,
            lock_message: String::new(),
        }
    }
}

impl Appearance {
    /// `text_scale` within 0.8 … 1.5.
    pub fn text_scale(&self) -> f32 {
        self.text_scale.clamp(0.8, 1.5)
    }
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            mode: Mode::Light,
            accent: Accent::Blue,
            glass_opacity: 1.0,
            wallpaper: String::new(),
            wallpaper_dark: String::new(),
            theme_apps: false,
            reduce_motion: false,
            wallpaper_fit: WallpaperFit::Fill,
            gallery_folders: String::new(),
            wallpaper_folder: String::new(),
            wallpaper_interval: 30,
            wallpaper_shuffle: false,
            dark_from: 19,
            dark_until: 7,
            text_scale: 1.0,
            pointer_size: 0,
            auto_sunset: false,
        }
    }
}

impl Default for Touchpad {
    fn default() -> Self {
        Self {
            enabled: true,
            tap_to_click: true,
            natural_scroll: true,
            swipe_fingers: 3,
            gesture_sensitivity: 1.0,
            speed: 0.0,
            secondary_click: SecondaryClick::TwoFingers,
            scroll_method: ScrollMethod::TwoFingers,
            tap_to_drag: true,
            drag_lock: false,
            three_finger_drag: false,
            disable_while_typing: true,
            ignore_with_mouse: false,
            gesture_spaces: true,
            gesture_mission_control: true,
            gesture_app_expose: true,
            gesture_launchpad: true,
            gesture_show_desktop: true,
        }
    }
}

impl Default for Windows {
    fn default() -> Self {
        Self {
            move_modifier: Modifier::Super,
            magnetic_edges: true,
            snapping: true,
            double_click_title: TitleAction::Zoom,
            spaces: 0,
            focus: FocusMode::Click,
            reopen_at_login: true,
            keyboard_nudge: false,
        }
    }
}

/// The M1 shortcuts (E1.6) and the hardware keys (E2.7).
fn default_bindings() -> BTreeMap<String, String> {
    [
        ("Super+Return", "terminal"),
        ("Super+B", "exec helium-browser"),
        ("Super+E", "exec dolphin"),
        ("Super+Q", "quit"),
        ("Super+W", "close"),
        ("Super+M", "minimise"),
        ("Super+Ctrl+F", "fullscreen"),
        ("Super+H", "hide"),
        ("Super+Alt+H", "hide-others"),
        ("Super+Tab", "switcher"),
        ("Super+Alt+Escape", "force-quit"),
        ("Super+Alt+8", "screen-zoom"),
        ("Super+Alt+equal", "screen-zoom in"),
        ("Super+Alt+minus", "screen-zoom out"),
        ("Super+Alt+F5", "screen-reader"),
        ("Super+Ctrl+Q", "lock"),
        ("Super+N", "notification-center"),
        ("Super+Space", "spotlight"),
        ("Super+A", "app-drawer"),
        ("Super+Ctrl+Space", "input-source"),
        ("Super+Ctrl+Shift+Space", "input-source previous"),
        ("Super+Ctrl+E", "emoji"),
        ("Super+period", "emoji"),
        ("Super+Left", "space left"),
        ("Super+Right", "space right"),
        ("Super+Ctrl+1", "space 1"),
        ("Super+Ctrl+2", "space 2"),
        ("Super+Ctrl+3", "space 3"),
        ("Super+Ctrl+4", "space 4"),
        ("Super+Ctrl+5", "space 5"),
        ("Super+Ctrl+6", "space 6"),
        ("Super+Ctrl+7", "space 7"),
        ("Super+Ctrl+8", "space 8"),
        ("Super+Ctrl+9", "space 9"),
        ("Super+1", "dock-app 1"),
        ("Super+2", "dock-app 2"),
        ("Super+3", "dock-app 3"),
        ("Super+4", "dock-app 4"),
        ("Super+5", "dock-app 5"),
        ("Super+6", "dock-app 6"),
        ("Super+7", "dock-app 7"),
        ("Super+8", "dock-app 8"),
        ("Super+9", "dock-app 9"),
        ("Super+Shift+1", "dock-app-new 1"),
        ("Super+Shift+2", "dock-app-new 2"),
        ("Super+Shift+6", "dock-app-new 6"),
        ("Super+Shift+7", "dock-app-new 7"),
        ("Super+Shift+8", "dock-app-new 8"),
        ("Super+Shift+9", "dock-app-new 9"),
        ("Super+Shift+Left", "move-to-space left"),
        ("Super+Shift+Right", "move-to-space right"),
        ("Super+Ctrl+Left", "tile left"),
        ("Super+Ctrl+Right", "tile right"),
        ("Super+Ctrl+Up", "tile top"),
        ("Super+Ctrl+Down", "tile bottom"),
        ("Super+Alt+Left", "nudge left"),
        ("Super+Alt+Right", "nudge right"),
        ("Super+Alt+Up", "nudge up"),
        ("Super+Alt+Down", "nudge down"),
        ("Super+Alt+Shift+Left", "resize left"),
        ("Super+Alt+Shift+Right", "resize right"),
        ("Super+Alt+Shift+Up", "resize up"),
        ("Super+Alt+Shift+Down", "resize down"),
        ("Super+grave", "cycle-app-windows"),
        ("Super+Up", "mission-control"),
        ("Super+Down", "app-expose"),
        ("Super+F11", "show-desktop"),
        ("Super+Shift+3", "screenshot"),
        ("Super+Shift+4", "screenshot-region"),
        ("Super+Shift+5", "screenshot-toolbar"),
        ("Print", "screenshot-toolbar"),
        ("Shift+Print", "screenshot-region"),
        ("Alt+Print", "screenshot-window"),
        ("Super+Alt+5", "screen-record"),
        ("F12", "quake"),
        ("XF86AudioRaiseVolume", "volume up"),
        ("XF86AudioLowerVolume", "volume down"),
        ("Shift+Alt+XF86AudioRaiseVolume", "volume up-fine"),
        ("Shift+Alt+XF86AudioLowerVolume", "volume down-fine"),
        ("XF86AudioMute", "mute"),
        ("XF86AudioMicMute", "mic-mute"),
        ("XF86MonBrightnessUp", "brightness up"),
        ("XF86MonBrightnessDown", "brightness down"),
        ("XF86KbdBrightnessUp", "keyboard-backlight up"),
        ("XF86KbdBrightnessDown", "keyboard-backlight down"),
        ("XF86KbdLightOnOff", "keyboard-backlight toggle"),
        ("XF86TouchpadToggle", "touchpad toggle"),
        ("XF86TouchpadOn", "touchpad on"),
        ("XF86TouchpadOff", "touchpad off"),
        ("XF86AudioPlay", "media play-pause"),
        ("XF86AudioPause", "media play-pause"),
        ("XF86AudioNext", "media next"),
        ("XF86AudioPrev", "media previous"),
        ("XF86AudioStop", "media stop"),
        ("XF86AudioForward", "media forward"),
        ("XF86AudioRewind", "media rewind"),
        ("XF86Eject", "eject"),
        ("XF86WWW", "default-app x-scheme-handler/https"),
        ("XF86Mail", "default-app x-scheme-handler/mailto"),
        ("XF86Explorer", "default-app inode/directory"),
        ("XF86Calculator", "default-app Calculator"),
        ("XF86Search", "spotlight"),
        ("XF86Display", "exec noobsbot-desktop-settings --page displays"),
        ("XF86PowerOff", "power-button"),
        ("XF86Sleep", "sleep"),
        ("Super+Shift+Q", "log-out"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect()
}

/// An XDG base directory: `$var` if it is absolute, else `~/<fallback>`.
pub fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback))
}

/// `$XDG_CONFIG_HOME/noobsbot-desktop/config.toml`, else `~/.config/noobsbot-desktop/config.toml`.
pub fn path() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config").join("noobsbot-desktop").join("config.toml")
}

/// Parses a config. The error's Display names the line and column. Unknown keys are logged, never an error.
pub fn parse(text: &str) -> Result<Config, toml::de::Error> {
    let mut config: Config = toml::from_str(text)?;
    if let (Ok(user), Ok(known)) = (toml::from_str::<toml::Table>(text), toml::from_str::<toml::Table>(DEFAULT_TOML)) {
        warn_unknown("", &user, &known);
    }
    if config.version > VERSION {
        tracing::warn!("config version {} is newer than {VERSION}; reading the keys this version understands", config.version);
    }
    config.bindings = merge_bindings(default_bindings(), std::mem::take(&mut config.bindings));
    config.appearance.glass_opacity = config.appearance.glass_opacity.clamp(0.0, 1.0);
    config.dock.icon_size = config.dock.icon_size.clamp(crate::shell::dock::MIN_ICON as u16, crate::shell::dock::MAX_ICON as u16);
    config.dock.zoom_percent = config.dock.zoom_percent.clamp(100, 300);
    Ok(config)
}

/// The user's `[bindings]` over the defaults, comparing parsed keys, so `Ctrl+Super+x` is `Super+Ctrl+X`: a user
/// binding replaces the default for its keys, and of two user bindings for the same keys the first (in sorted order)
/// wins. Each conflict is logged; "none" frees a key.
fn merge_bindings(defaults: BTreeMap<String, String>, user: BTreeMap<String, String>) -> BTreeMap<String, String> {
    use crate::shortcuts::parse;
    let mut out = defaults;
    let mut taken: Vec<(crate::shortcuts::Binding, String)> = Vec::new();
    for (combo, action) in user {
        // A combo that doesn't parse can't conflict; `shortcuts::from_config` reports it.
        if let Some(keys) = parse(&combo) {
            if let Some((_, first)) = taken.iter().find(|(k, _)| *k == keys) {
                tracing::warn!("config: shortcut `{combo}` = `{action}` is the same keys as `{first}` = `{}`; keeping `{first}`", out[first]);
                continue;
            }
            out.retain(|default, was| {
                let same = parse(default) == Some(keys);
                if same && *default != combo && action != "none" {
                    tracing::warn!("config: shortcut `{combo}` = `{action}` is the same keys as the default `{default}` = `{was}`; using `{combo}`");
                }
                !same
            });
            taken.push((keys, combo.clone()));
        }
        out.insert(combo, action);
    }
    out.retain(|_, action| action != "none");
    out
}

/// Every key of `user` that `known` (the shipped defaults) doesn't have. `bindings` keys are free-form.
fn warn_unknown(prefix: &str, user: &toml::Table, known: &toml::Table) {
    for (key, value) in user {
        let name = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
        match (known.get(key), value) {
            // Shipped commented out, so `known` has no `outputs`.
            (None, toml::Value::Array(outputs)) if name == "display.outputs" => {
                for (i, output) in outputs.iter().enumerate() {
                    for key in output.as_table().into_iter().flat_map(|t| t.keys()).filter(|k| !OUTPUT_KEYS.contains(&k.as_str())) {
                        tracing::warn!("config: unknown setting `{key}` in display {} ignored", i + 1);
                    }
                }
            }
            (None, _) => tracing::warn!("config: unknown setting `{name}` ignored"),
            (Some(_), toml::Value::Array(rules)) if name == "rules" => {
                for (i, rule) in rules.iter().enumerate() {
                    for key in rule.as_table().into_iter().flat_map(|t| t.keys()).filter(|k| !RULE_KEYS.contains(&k.as_str())) {
                        tracing::warn!("config: unknown setting `{key}` in window rule {} ignored", i + 1);
                    }
                }
            }
            (Some(toml::Value::Table(known)), toml::Value::Table(user))
                if !["bindings", "notification_apps", "dock.assign", "dock.outputs"].contains(&name.as_str()) =>
            {
                warn_unknown(&name, user, known)
            }
            _ => {}
        }
    }
}

/// Reads and parses the file: the error (logged), "line 3: …", when it can't be read or parsed; a missing file is the
/// defaults.
pub fn read(path: &Path) -> Result<Config, String> {
    let result = match fs::read_to_string(path) {
        Ok(text) => parse(&text).map_err(|err| {
            let message = err.message().trim().replace('\n', " ");
            let line = err.span().map(|s| text.as_bytes()[..s.start.min(text.len())].iter().filter(|&&b| b == b'\n').count() + 1);
            line.map_or(message.clone(), |n| format!("line {n}: {message}"))
        }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
        Err(err) => Err(err.to_string()),
    };
    result.inspect_err(|err| tracing::warn!("{}: {err}", path.display()))
}

/// Startup load: the file, or the defaults if it is missing or broken. An older file is migrated (and saved) first.
pub fn load(path: &Path) -> Config {
    if let Some(text) = fs::read_to_string(path).ok().and_then(|text| migrate(&text)) {
        // Through a dotfiles symlink, as `save_edit` does.
        let file = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        match save_atomic(&file, &text) {
            Ok(()) => tracing::info!("{}: settings moved to version {VERSION}", file.display()),
            Err(err) => tracing::warn!("{}: {err}", file.display()),
        }
    }
    read(path).unwrap_or_default()
}

/// The settings file's format version.
const VERSION: u32 = 2;

/// An older file's text brought up to `VERSION`; `None` when it is current or doesn't parse. Version 2: the display
/// scale's default went from "picked from the pixel density" (0) to 115%, so a file still on 0 gets that. Choosing 0
/// again later sticks.
fn migrate(text: &str) -> Option<String> {
    let table: toml::Table = toml::from_str(text).ok()?;
    if table.get("version").and_then(toml::Value::as_integer).unwrap_or(1) >= i64::from(VERSION) {
        return None;
    }
    let scale = table.get("display").and_then(|d| d.get("scale"));
    let text = match scale {
        Some(toml::Value::Float(0.0) | toml::Value::Integer(0)) => set_key(text, "display", "scale", &Displays::default().scale.to_string()),
        _ => text.to_owned(),
    };
    // `version` goes above every table; a file without one gets it first.
    let version = format!("version = {VERSION}\n");
    let mut lines: Vec<&str> = text.split_inclusive('\n').collect();
    let tables = lines.iter().position(|l| l.trim_start().starts_with('[')).unwrap_or(lines.len());
    match lines[..tables].iter().position(|l| l.trim_start().strip_prefix("version").is_some_and(|r| r.trim_start().starts_with('='))) {
        Some(i) => lines[i] = &version,
        None => lines.insert(0, &version),
    }
    Some(lines.concat())
}

/// The broken-settings notification's id, below the network portal's.
pub const BROKEN_NOTICE: u32 = 0xFFF9_0000;

/// "Settings file has an error", with `err` (its line) and Open File.
pub fn broken_notice(err: &str) -> crate::services::notifications::Notification {
    let icon = "dialog-warning";
    crate::services::notifications::Notification {
        id: BROKEN_NOTICE,
        app: "System Settings".into(),
        icon_name: icon.into(),
        icon: crate::services::notifications::load_icon(icon).map(std::sync::Arc::new),
        summary: "Settings file has an error".into(),
        body: format!("config.toml {err}. The last good settings stay until it is fixed."),
        actions: vec![("default".into(), String::new()), ("open".into(), "Open File".into())],
        urgency: 1,
        expire: -1,
        time: chrono::Local::now().timestamp(),
        ..Default::default()
    }
}

/// Writes `contents` to a temp file next to `path`, syncs it and renames it over `path`, so a reader (or a crash)
/// only ever sees the old file or the new one.
pub fn save_atomic(path: &Path, contents: &str) -> io::Result<()> {
    save_atomic_if(path, contents, || true).map(drop)
}

/// `save_atomic`, but the rename only happens if `still()` holds once the new text is on disk (another writer may
/// have replaced the file meanwhile); tells whether it did.
pub fn save_atomic_if(path: &Path, contents: &str, still: impl FnOnce() -> bool) -> io::Result<bool> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "config path has no file name"))?;
    fs::create_dir_all(dir)?;
    // One temp name per save, so two threads saving at once never write into each other's.
    static SAVES: AtomicU64 = AtomicU64::new(0);
    let tmp = dir.join(format!(".{}.{}.{}.tmp", name.to_string_lossy(), std::process::id(), SAVES.fetch_add(1, Ordering::Relaxed)));
    let written = fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(contents.as_bytes())?;
        f.sync_all()
    });
    match written.and_then(|()| if still() { fs::rename(&tmp, path).map(|()| true) } else { Ok(false) }) {
        Ok(true) => {}
        not_renamed => {
            let _ = fs::remove_file(&tmp);
            return not_renamed;
        }
    }
    // The rename itself is durable once the directory is synced; failing that is not worth an error.
    let _ = fs::File::open(dir).and_then(|d| d.sync_all());
    Ok(true)
}

/// `text` with `[table] key = value` (`value` already TOML, one line); every other line and comment is kept.
// ponytail: single-line values only; a multi-line array needs dock.rs's `set_items` scan.
pub fn set_key(text: &str, table: &str, key: &str, value: &str) -> String {
    let mut lines: Vec<String> = text.split_inclusive('\n').map(String::from).collect();
    if let Some(last) = lines.last_mut().filter(|l| !l.ends_with('\n')) {
        last.push('\n');
    }
    let entry = format!("{key} = {value}\n");
    let Some(start) = lines.iter().position(|l| l.trim() == format!("[{table}]")) else {
        let gap = if lines.is_empty() { "" } else { "\n" };
        return format!("{}{gap}[{table}]\n{entry}", lines.concat());
    };
    let end = lines[start + 1..].iter().position(|l| l.trim_start().starts_with('[')).map_or(lines.len(), |i| start + 1 + i);
    let is_key = |l: &str| l.trim_start().strip_prefix(key).is_some_and(|r| r.trim_start().starts_with('='));
    match (start + 1..end).find(|&i| is_key(&lines[i])) {
        Some(i) => {
            // A trailing comment stays.
            let comment = lines[i].find(" #").map(|at| lines[i][at..].to_owned()).unwrap_or_else(|| "\n".into());
            lines[i] = format!("{key} = {value}{comment}");
        }
        // After the table's last setting, before the blank lines and comments that lead into the next table.
        None => {
            let at = (start + 1..end).rev().find(|&i| !lines[i].trim().is_empty() && !lines[i].trim_start().starts_with('#')).map_or(start + 1, |i| i + 1);
            lines.insert(at, entry);
        }
    }
    lines.concat()
}

/// How many `[` a TOML line opens minus the `]` it closes, brackets inside quotes or a comment not counted.
pub fn bracket_depth(line: &str) -> i32 {
    let (mut depth, mut quote, mut escaped) = (0, None, false);
    for c in line.chars() {
        match (quote, c) {
            (Some(_), _) if escaped => escaped = false,
            (Some('"'), '\\') => escaped = true,
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '#') => break,
            (None, '[') => depth += 1,
            (None, ']') => depth -= 1,
            _ => {}
        }
    }
    depth
}

/// `s` as a TOML basic string: `"`, `\` and every control character escaped, so no value can break the file.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if c.is_ascii_control() => out.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `text` with `key = [items]` set in `[section]`, keeping every other line and comment; adds the section or the
/// key when missing, and replaces a multi-line array whole.
pub fn set_array(text: &str, section: &str, key: &str, items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|s| quote(s)).collect();
    let value = format!("{key} = [{}]\n", quoted.join(", "));
    let mut lines: Vec<String> = text.split_inclusive('\n').map(String::from).collect();
    if let Some(last) = lines.last_mut().filter(|l| !l.ends_with('\n')) {
        last.push('\n');
    }
    let header = |l: &str| l.trim_start().starts_with('[');
    let title = format!("[{section}]");
    let Some(at) = lines.iter().position(|l| l.trim_start().starts_with(&title)) else {
        let gap = if lines.is_empty() { "" } else { "\n" };
        return format!("{}{gap}{title}\n{value}", lines.concat());
    };
    let end = lines[at + 1..].iter().position(|l| header(l)).map_or(lines.len(), |i| at + 1 + i);
    let is_key = |l: &str| l.trim_start().strip_prefix(key).is_some_and(|r| r.trim_start().starts_with('='));
    match (at + 1..end).find(|&i| is_key(&lines[i])) {
        Some(start) => {
            // A multi-line array runs until its brackets close.
            let mut depth = 0;
            let mut last = start;
            for (i, line) in lines.iter().enumerate().skip(start) {
                depth += bracket_depth(line);
                last = i;
                if depth <= 0 {
                    break;
                }
            }
            lines.splice(start..=last, [value]);
        }
        None => lines.insert(at + 1, value),
    }
    lines.concat()
}

/// `text` with its `[table]` section (the table and every `[table.…]` and `[[table.…]]` under it) replaced by
/// `section` (TOML of its own, empty to drop it), keeping every other line and comment.
pub fn set_section(text: &str, table: &str, section: &str) -> String {
    let ours = |l: &str| l.trim_start().trim_start_matches('[').strip_prefix(table).is_some_and(|r| r.starts_with(']') || r.starts_with('.'));
    let mut skip = false;
    let mut kept: String = text
        .split_inclusive('\n')
        .filter(|l| {
            if l.trim_start().starts_with('[') {
                skip = ours(l);
            }
            !skip
        })
        .collect();
    if !kept.is_empty() && !kept.ends_with('\n') {
        kept.push('\n');
    }
    let gap = if kept.is_empty() || section.is_empty() { "" } else { "\n" };
    format!("{kept}{gap}{section}")
}

/// Sets one key in the user's file (through a dotfiles symlink, so the live reload agrees) and logs a failure.
pub fn save_key(table: &str, key: &str, value: &str) {
    let (table, key, value) = (table.to_owned(), key.to_owned(), value.to_owned());
    let path = self::path();
    save_later(fs::canonicalize(&path).unwrap_or(path), move |text| set_key(if text.is_empty() { DEFAULT_TOML } else { text }, &table, &key, &value));
}

/// Rewrites the user's file as `edit` makes it (through a dotfiles symlink, so the live reload agrees); logs a
/// failure and tells whether it was written.
pub fn save_edit(edit: impl FnOnce(&str) -> String) -> bool {
    let path = self::path();
    let path = fs::canonicalize(&path).unwrap_or(path);
    read_config(&path)
        .and_then(|text| save_atomic(&path, &edit(if text.is_empty() { DEFAULT_TOML } else { &text })))
        .inspect_err(|err| tracing::warn!("{}: {err}", path.display()))
        .is_ok()
}

/// A file's text before an edit: "" when it is missing. Any other error (unreadable, not UTF-8) is returned, so the
/// write is skipped rather than replacing the whole file with the edit alone.
pub fn read_config(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        read => read,
    }
}

type Edit = Box<dyn FnOnce(&str) -> String + Send>;

/// What the `config-save` thread is sent: an edit, or a flush (write what is queued now, then answer).
enum Save {
    Edit(PathBuf, Edit),
    Flush(mpsc::Sender<()>),
}

static QUEUE: OnceLock<Mutex<mpsc::Sender<Save>>> = OnceLock::new();

/// Rewrites `path` with `edit(current text)` on the `config-save` thread, so a save (a sync and two renames, tens of
/// ms on a slow disk) never stalls a frame. Edits of one file within `DEBOUNCE` of each other are applied in order
/// and written once: Ctrl+scrolling the dock's icon size saves at the end, not per notch. A missing file reads as "";
/// one that can't be read isn't written.
/// Quitting calls `flush`, so the last edits are kept.
pub fn save_later(path: PathBuf, edit: impl FnOnce(&str) -> String + Send + 'static) {
    let queue = QUEUE.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Save>();
        let spawned = std::thread::Builder::new().name("config-save".into()).spawn(move || {
            while let Ok(first) = rx.recv() {
                let (mut edits, mut flushed) = (Vec::new(), None);
                let mut next = Ok(first);
                while let Ok(save) = next {
                    next = match save {
                        Save::Edit(path, edit) => {
                            edits.push((path, edit));
                            rx.recv_timeout(DEBOUNCE)
                        }
                        Save::Flush(done) => {
                            flushed = Some(done);
                            Err(mpsc::RecvTimeoutError::Timeout)
                        }
                    };
                }
                let mut files: Vec<(PathBuf, Vec<Edit>)> = Vec::new();
                for (path, edit) in edits {
                    match files.iter_mut().find(|(p, _)| *p == path) {
                        Some((_, edits)) => edits.push(edit),
                        None => files.push((path, vec![edit])),
                    }
                }
                for (path, edits) in files {
                    let saved = read_config(&path).and_then(|text| save_atomic(&path, &edits.into_iter().fold(text, |text, edit| edit(&text))));
                    if let Err(err) = saved {
                        tracing::warn!("{}: {err}", path.display());
                    }
                }
                if let Some(done) = flushed {
                    let _ = done.send(());
                }
            }
        });
        if let Err(err) = spawned {
            tracing::warn!("saving settings on the main thread: {err}");
        }
        Mutex::new(tx)
    });
    let edit: Edit = Box::new(edit);
    if let Err(mpsc::SendError(Save::Edit(path, edit))) = queue.lock().unwrap_or_else(|e| e.into_inner()).send(Save::Edit(path, edit)) {
        // The thread never started: do it here rather than lose the save.
        if let Err(err) = read_config(&path).and_then(|text| save_atomic(&path, &edit(&text))) {
            tracing::warn!("{}: {err}", path.display());
        }
    }
}

/// Writes the edits `save_later` still holds back and waits for them, 2 s at most (a hung disk must not keep the
/// desktop from quitting). Called on the way out: log out, restart, shut down, the nested window closed, SIGTERM or
/// SIGINT (`stop_on_signals` in main.rs).
pub fn flush() {
    let Some(queue) = QUEUE.get() else { return };
    let (tx, rx) = mpsc::channel();
    if queue.lock().unwrap_or_else(|e| e.into_inner()).send(Save::Flush(tx)).is_ok() {
        let _ = rx.recv_timeout(Duration::from_secs(2));
    }
}

/// Live reload on a worker thread. Watches the file's directory (editors replace the file by renaming), waits for
/// 300 ms of quiet, and calls `send` with each newly parsed config that differs from the last one sent (or fixes a
/// broken file). A file that doesn't parse sends its error instead, once per new error (also at start, when the
/// desktop started with the defaults), so the last good config stays. The thread ends when `send` returns false.
///
/// The compositor bridges it into calloop without an extra thread:
/// ```ignore
/// let (tx, rx) = calloop::channel::channel();
/// config::watch(&path, move |config| tx.send(config).is_ok());
/// handle.insert_source(rx, |event, _, state| if let channel::Event::Msg(Ok(config)) = event { state.apply_config(config) })?;
/// ```
pub fn watch(path: &Path, mut send: impl FnMut(Result<Config, String>) -> bool + Send + 'static) {
    let path = path.to_owned();
    let (Some(dir), Some(name)) = (path.parent().map(Path::to_owned), path.file_name().map(ToOwned::to_owned)) else { return };
    let spawned = std::thread::Builder::new().name("config-watch".into()).spawn(move || {
        let _ = fs::create_dir_all(&dir);
        let (tx, rx) = mpsc::channel();
        let mut watcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(err) => return tracing::warn!("config live reload off: {err}"),
        };
        if let Err(err) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
            return tracing::warn!("config live reload off: {}: {err}", dir.display());
        }
        // Writes, renames and deletes of our file; our own reads (open / close-after-read) would loop forever.
        let changed = |event: notify::Result<notify::Event>| {
            event.is_ok_and(|e| {
                let write = matches!(e.kind, EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_))
                    || matches!(e.kind, EventKind::Access(AccessKind::Close(AccessMode::Write)));
                write && e.paths.iter().any(|p| p.file_name() == Some(&name))
            })
        };
        let mut last: Option<Config> = None;
        let mut broken = read(&path).err();
        if let Some(err) = &broken
            && !send(Err(err.clone()))
        {
            return;
        }
        while let Ok(event) = rx.recv() {
            if !changed(event) {
                continue;
            }
            while rx.recv_timeout(DEBOUNCE).is_ok() {}
            let sent = match read(&path) {
                Ok(config) if broken.take().is_some() || last.as_ref() != Some(&config) => {
                    last = Some(config.clone());
                    send(Ok(config))
                }
                // Not every save while it stays broken the same way.
                Err(err) if broken.as_ref() != Some(&err) => {
                    broken = Some(err.clone());
                    send(Err(err))
                }
                Ok(_) | Err(_) => true,
            };
            if !sent {
                return;
            }
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("config live reload off: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_finger_drag_moves_swipes_to_four_fingers() {
        let mut touchpad = Touchpad::default();
        assert_eq!(touchpad.effective_swipe_fingers(), 3);
        touchpad.swipe_fingers = 4;
        assert_eq!(touchpad.effective_swipe_fingers(), 4);
        touchpad.swipe_fingers = 3;
        touchpad.three_finger_drag = true;
        assert_eq!(touchpad.effective_swipe_fingers(), 4);
    }

    #[test]
    fn notification_app_rules() {
        let config: Config = toml::from_str(
            "[notification_apps]\nMail = \"alert\"\nChat = { style = \"none\", sound = false, lock = true }\n\"org.x.Y\" = { center = false, badge = false }\n",
        )
        .unwrap();
        let rule = |app: &str| config.notification_apps[app];
        assert_eq!(rule("Mail"), AppRule { style: AppStyle::Alert, ..Default::default() });
        assert_eq!(rule("Chat"), AppRule { style: AppStyle::None, sound: false, badge: true, center: true, lock: true });
        assert_eq!(rule("org.x.Y"), AppRule { style: AppStyle::Banner, sound: true, badge: false, center: false, lock: false });
        assert!(toml::from_str::<Config>("[notification_apps]\nX = \"loud\"\n").is_err());
    }

    #[test]
    fn dnd_schedule_crosses_midnight() {
        let at = |dnd_scheduled, dnd_from, dnd_until, hour| Notifications { dnd_scheduled, dnd_from, dnd_until, ..Default::default() }.dnd_at(hour);
        assert!(at(true, 22, 7, 22) && at(true, 22, 7, 23) && at(true, 22, 7, 0) && at(true, 22, 7, 6));
        assert!(!at(true, 22, 7, 7) && !at(true, 22, 7, 12) && !at(true, 22, 7, 21));
        assert!(at(true, 9, 17, 9) && at(true, 9, 17, 16) && !at(true, 9, 17, 17) && !at(true, 9, 17, 8));
        assert!(!at(false, 22, 7, 23), "off without the schedule");
        assert!(!at(true, 8, 8, 8), "an empty window");
    }

    #[test]
    fn previews_show_the_body() {
        assert!(Previews::Always.show(true) && Previews::Always.show(false));
        assert!(Previews::Unlocked.show(false) && !Previews::Unlocked.show(true));
        assert!(!Previews::Never.show(false) && !Previews::Never.show(true));
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("noobsbot-desktop-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn system_keyboard_files() {
        let vconsole = "KEYMAP=de-latin1\n# XKBLAYOUT=fr\nXKBLAYOUT=de\nXKBVARIANT=\"nodeadkeys\"\nXKBOPTIONS='caps:escape'\n";
        let k = parse_system_keyboard(vconsole);
        assert_eq!((k.layout.as_str(), k.variant.as_str(), k.options.as_str()), ("de", "nodeadkeys", "caps:escape"));

        let xorg = "Section \"InputClass\"\n    Identifier \"system-keyboard\"\n    MatchIsKeyboard \"on\"\n    Option \"XkbLayout\" \"us,de\"\n    Option \"XkbOptions\" \"grp:alt_shift_toggle\"\nEndSection\n";
        let k = parse_system_keyboard(xorg);
        assert_eq!((k.layout.as_str(), k.variant.as_str(), k.options.as_str()), ("us,de", "", "grp:alt_shift_toggle"));

        assert_eq!(parse_system_keyboard("KEYMAP=us\n"), Keyboard::default());
    }

    #[test]
    fn config_layout_wins() {
        let k = Keyboard { layout: "de".into(), variant: "neo".into(), options: "caps:escape".into(), ..Default::default() }.resolved();
        assert_eq!((k.layout.as_str(), k.variant.as_str(), k.options.as_str()), ("de", "neo", "caps:escape"));
    }

    #[test]
    fn old_files_get_the_new_default_scale() {
        let old = "# mine\nversion = 1\n\n[display]\nscale = 0.0 # auto\n";
        assert_eq!(migrate(old).unwrap(), "# mine\nversion = 2\n\n[display]\nscale = 1.15 # auto\n");
        assert_eq!(migrate("[display]\nscale = 0\n").unwrap(), "version = 2\n[display]\nscale = 1.15\n");
        // A scale the owner picked stays; so does 0 picked again in a version 2 file.
        assert_eq!(migrate("version = 1\n[display]\nscale = 1.5\n").unwrap(), "version = 2\n[display]\nscale = 1.5\n");
        assert_eq!(migrate("version = 2\n[display]\nscale = 0.0\n"), None);
        assert_eq!(migrate(DEFAULT_TOML), None);
        assert_eq!(migrate("version = "), None);
    }

    #[test]
    fn defaults_round_trip() {
        assert_eq!(parse(DEFAULT_TOML).unwrap(), Config::default());
        assert_eq!(parse("").unwrap(), Config::default());
        assert!(!Config::default().update.install_at_login, "installing without the password is opt-in");
        assert_eq!(parse("[dock]\nstyle = \"glass\"").unwrap(), Config::default(), "the dock's old bar style is ignored");
    }

    #[test]
    fn user_bindings_beat_defaults_by_keys() {
        let map = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<BTreeMap<_, _>>();
        let defaults = map(&[("Super+Ctrl+Q", "lock"), ("Super+B", "exec browser"), ("Super+W", "close")]);
        let user = map(&[("Ctrl+Super+q", "exec a"), ("super+ctrl+Q", "exec b"), ("super+b", "none"), ("Super+W", "minimise")]);
        let merged = merge_bindings(defaults, user);
        // Sorted, "Ctrl+Super+q" comes before "super+ctrl+Q", so it wins; both replace the default.
        assert_eq!(merged, map(&[("Ctrl+Super+q", "exec a"), ("Super+W", "minimise")]));
    }

    #[test]
    fn default_bindings_do_not_conflict() {
        let mut seen = Vec::new();
        for combo in default_bindings().keys() {
            let keys = crate::shortcuts::parse(combo).unwrap_or_else(|| panic!("{combo} parses"));
            assert!(!seen.contains(&keys), "{combo} repeats another default");
            seen.push(keys);
        }
    }

    #[test]
    fn partial_config_and_bindings_merge() {
        let c =
            parse("[windows]\nsnapping = false\ndouble_click_title = \"minimize\"\n[bindings]\n\"Super+B\" = \"none\"\n\"Super+K\" = \"exec kate\"").unwrap();
        assert!(!c.windows.snapping && c.windows.magnetic_edges);
        assert_eq!(c.windows.double_click_title, TitleAction::Minimise);
        assert!(!c.bindings.contains_key("Super+B"));
        assert_eq!(c.bindings["Super+K"], "exec kate");
        assert_eq!(c.bindings["Super+Q"], "quit");
    }

    #[test]
    fn caldav_accounts_parse() {
        let c = parse("[calendar]\ncaldav = [{ url = \"https://dav.example.com/calendars/me/personal/\", user = \"me\" }]\n").unwrap();
        assert_eq!(c.calendar.caldav, [CalDav { url: "https://dav.example.com/calendars/me/personal/".into(), user: "me".into() }]);
        assert_eq!(c.calendar.files, Calendar::default().files, "files keep their default");
    }

    #[test]
    fn dock_lists_per_display() {
        let c = parse("[dock]\nitems = [\"kate\"]\n\n[dock.outputs]\n\"HDMI-A-1\" = [\"kitty\", \"dolphin\"]\n").unwrap();
        assert_eq!(c.dock.items_on(Some("HDMI-A-1")), ["kitty", "dolphin"]);
        assert_eq!(c.dock.items_on(Some("eDP-1")), ["kate"], "no list of its own: the shared one");
        assert_eq!(c.dock.items_on(None), ["kate"]);
        assert!(parse(DEFAULT_TOML).unwrap().dock.outputs.is_empty());
    }

    #[test]
    fn display_outputs() {
        let c = parse(concat!(
            "[display]\nscale = 1.5\n",
            "[[display.outputs]]\nname = \"eDP-1\"\nmode = \"1920x1200\"\nrefresh = 59.95\nscale = 1.25\nrotation = 90\nposition = [0, -200]\nprimary = true\nvrr = true\nunderscan = 5\nshiny = 1\n",
            "[[display.outputs]]\nname = \"DP-1\"\nmirror = \"eDP-1\"\n",
            "[[display.outputs]]\nname = \"HDMI-A-1\"\nenabled = false\n",
        ))
        .unwrap();
        assert_eq!(c.display.scale, 1.5);
        let [laptop, dp, hdmi] = &c.display.outputs[..] else { panic!("{:?}", c.display.outputs) };
        assert_eq!(
            laptop,
            &OutputSettings {
                name: "eDP-1".into(),
                enabled: true,
                mode: "1920x1200".into(),
                refresh: 59.95,
                scale: 1.25,
                rotation: 90,
                position: Some([0, -200]),
                primary: true,
                mirror: String::new(),
                vrr: true,
                underscan: 5
            }
        );
        assert_eq!(hdmi, &OutputSettings { name: "HDMI-A-1".into(), enabled: false, ..Default::default() }, "unset keys: the defaults");
        assert_eq!(dp.mirror, "eDP-1");
    }

    #[test]
    fn double_click_time_is_kept_sane() {
        let ms = |text: &str| parse(text).unwrap().mouse.double_click().as_millis();
        assert_eq!(ms(""), 400);
        assert_eq!(ms("[mouse]\ndouble_click_ms = 650"), 650);
        assert_eq!(ms("[mouse]\ndouble_click_ms = 0"), 150, "0 would make double-clicks impossible");
        assert_eq!(ms("[mouse]\ndouble_click_ms = 99999"), 1000);
    }

    #[test]
    fn gestures_turn_off_one_by_one() {
        use crate::gestures::Gesture;
        let t = parse("[touchpad]\ngesture_spaces = false\ngesture_launchpad = false").unwrap().touchpad;
        assert!(!t.allows(Gesture::Space(1)) && !t.allows(Gesture::Space(-1)) && !t.allows(Gesture::Launchpad));
        assert!(t.allows(Gesture::MissionControl) && t.allows(Gesture::AppExpose) && t.allows(Gesture::ShowDesktop));
    }

    #[test]
    fn scroll_method_parses() {
        let method = |v: &str| parse(&format!("[touchpad]\nscroll_method = \"{v}\"")).unwrap().touchpad.scroll_method;
        assert_eq!(method("edge"), ScrollMethod::Edge);
        assert_eq!(method("none"), ScrollMethod::None);
        assert_eq!(parse("").unwrap().touchpad.scroll_method, ScrollMethod::TwoFingers);
    }

    #[test]
    fn secondary_click_off_parses() {
        assert_eq!(parse("[touchpad]\nsecondary_click = \"off\"").unwrap().touchpad.secondary_click, SecondaryClick::Off);
    }

    #[test]
    fn unknown_keys_tolerated() {
        let c = parse("colour = 1\n[windows]\nsnaping = false\n[future]\nx = 1\n[[rules]]\napp_id = \"x\"\nwobbly = true").unwrap();
        assert!(c.windows.snapping);
        assert_eq!(c.rules.len(), 1);
    }

    #[test]
    fn retired_dock_keys_still_load() {
        let c = parse("[dock]\nitems = [\"firefox\", \"trash\"]\nshow_trash = false\nminimise_into_icon = false").unwrap();
        assert_eq!(c.dock.items, ["firefox", "trash"], "the dock skips the old Trash entry itself");
    }

    #[test]
    fn bad_toml_keeps_previous() {
        let err = parse("version = 1\n[windows]\nsnapping = maybe").unwrap_err().to_string();
        assert!(err.contains("line 3"), "{err}");
        assert!(parse("[appearance]\nmode = \"sepia\"").is_err());

        let dir = temp_dir("bad");
        let path = dir.join("config.toml");
        fs::write(&path, "[windows\nsnapping = true\n").unwrap();
        assert!(read(&path).is_err_and(|e| e.starts_with("line 1: ")));
        fs::write(&path, "[windows]\nsnapping = maybe\n").unwrap();
        assert!(read(&path).is_err_and(|e| e.starts_with("line 2: ")));
        assert_eq!(load(&path), Config::default());
        assert_eq!(load(&dir.join("missing.toml")), Config::default());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn set_one_key() {
        let text = "[appearance]\n# the mode\nmode = \"light\" # or dark\naccent = \"blue\"\n\n# next\n[dock]\nzoom = true";
        assert_eq!(set_key(text, "appearance", "mode", "\"dark\""), text.replace("\"light\"", "\"dark\"") + "\n");
        assert_eq!(
            set_key("[appearance]\naccent = \"red\"\n\n# next\n[dock]\n", "appearance", "mode", "\"dark\""),
            "[appearance]\naccent = \"red\"\nmode = \"dark\"\n\n# next\n[dock]\n"
        );
        assert_eq!(set_key("version = 1\n", "appearance", "mode", "\"dark\""), "version = 1\n\n[appearance]\nmode = \"dark\"\n");
        let config = parse(&set_key(DEFAULT_TOML, "appearance", "mode", "\"dark\"")).unwrap();
        assert_eq!(config.appearance.mode, Mode::Dark);
    }

    #[test]
    fn set_whole_section() {
        let text = "[keyboard]\nlayout = \"us\"\n\n# displays\n[display]\nscale = 2.0\n\n[[display.outputs]]\nname = \"eDP-1\"\nscale = 1.5\n\n[clock]\ntimezone = \"UTC\"\n";
        let previous = "[display]\nscale = 1.0\n";
        assert_eq!(set_section(text, "display", previous), format!("[keyboard]\nlayout = \"us\"\n\n# displays\n[clock]\ntimezone = \"UTC\"\n\n{previous}"));
        assert_eq!(set_section(text, "display", ""), "[keyboard]\nlayout = \"us\"\n\n# displays\n[clock]\ntimezone = \"UTC\"\n");
        assert_eq!(set_section("[displays]\nx = 1", "display", previous), format!("[displays]\nx = 1\n\n{previous}"), "another table");
        assert_eq!(set_section("", "display", previous), previous);
        assert_eq!(parse(&set_section(DEFAULT_TOML, "display", previous)).unwrap().display.scale, 1.0);
    }

    #[test]
    fn atomic_save() {
        let dir = temp_dir("save");
        let path = dir.join("sub").join("config.toml");
        save_atomic(&path, "version = 1\n").unwrap();
        save_atomic(&path, "[windows]\nsnapping = false\n").unwrap();
        assert!(!load(&path).windows.snapping);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1, "no temp file left behind");
        assert!(!save_atomic_if(&path, "version = 1\n", || false).unwrap());
        assert!(!load(&path).windows.snapping, "not renamed when the file changed meanwhile");
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1, "nor its temp file left behind");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saves_from_two_threads_at_once() {
        let dir = temp_dir("threads");
        let path = dir.join("config.toml");
        let texts = ["a = 1\n".repeat(10_000), "b = 2\n".repeat(10_000)];
        std::thread::scope(|s| {
            for text in &texts {
                let path = &path;
                s.spawn(move || (0..20).for_each(|_| save_atomic(path, text).unwrap()));
            }
        });
        assert!(texts.contains(&fs::read_to_string(&path).unwrap()), "one save whole, never a mix");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1, "no temp file left behind");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn later_saves_coalesce_in_order() {
        let dir = temp_dir("later");
        let path = dir.join("config.toml");
        save_later(path.clone(), |text| set_key(text, "dock", "icon_size", "40"));
        save_later(path.clone(), |text| set_key(text, "dock", "icon_size", "48"));
        save_later(dir.join("other.txt"), |_| "other\n".into());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        // Both files: the thread writes them one after the other.
        while !(path.is_file() && dir.join("other.txt").is_file()) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), "[dock]\nicon_size = 48\n", "the second edit saw the first");
        assert_eq!(fs::read_to_string(dir.join("other.txt")).unwrap(), "other\n");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unreadable_file_is_not_saved_over() {
        let dir = temp_dir("unreadable");
        let path = dir.join("config.toml");
        assert_eq!(read_config(&path).unwrap(), "", "missing reads as empty");
        fs::write(&path, b"[dock]\nicon_size = 40 # \xff\n").unwrap();
        assert!(read_config(&path).is_err(), "not UTF-8");
        save_later(path.clone(), |text| set_key(text, "dock", "icon_size", "48"));
        flush();
        assert_eq!(fs::read(&path).unwrap(), b"[dock]\nicon_size = 40 # \xff\n", "left as it was");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn flush_writes_at_once() {
        let dir = temp_dir("flush");
        let path = dir.join("config.toml");
        save_later(path.clone(), |_| "x = 1\n".into());
        flush();
        assert_eq!(fs::read_to_string(&path).unwrap(), "x = 1\n", "written without waiting out the debounce");
        flush();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn live_reload() {
        let dir = temp_dir("watch");
        let path = dir.join("config.toml");
        let (tx, rx) = mpsc::channel();
        watch(&path, move |c| tx.send(c).is_ok());
        std::thread::sleep(Duration::from_millis(200));

        save_atomic(&path, "[windows]\nsnapping = false\n").unwrap();
        assert!(!rx.recv_timeout(Duration::from_secs(3)).unwrap().unwrap().windows.snapping);

        fs::write(&path, "[windows\nsnapping = true").unwrap();
        assert!(rx.recv_timeout(Duration::from_secs(3)).unwrap().is_err_and(|e| e.starts_with("line 1")), "a broken file sends its error");
        fs::write(&path, "[windows\nsnapping = true").unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(800)).is_err(), "the same error only once");

        fs::write(&path, "[windows]\nmagnetic_edges = false\n").unwrap();
        let c = rx.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
        assert!(c.windows.snapping && !c.windows.magnetic_edges);
        drop(rx);
        fs::remove_dir_all(dir).unwrap();
    }
}
