//! The settings file (E1.8, C11): `~/.config/noobsbot-desktop/config.toml`, parsed with serde + toml and live-reloaded.
//! It never stops the desktop: a missing file means defaults, a broken one is logged with its line and column and the
//! last good config stays, unknown keys are warned about and skipped.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock, mpsc},
    time::Duration,
};

use notify::{
    EventKind, RecursiveMode, Watcher,
    event::{AccessKind, AccessMode},
};
use serde::Deserialize;

use crate::rules::{AllSpaces, Layer, Length, RULE_KEYS, Rule, Space};

/// The commented defaults shipped as `data/config.toml`; parsing it yields `Config::default()` (tested).
pub const DEFAULT_TOML: &str = include_str!("../data/config.toml");

/// Reload waits until the file has been quiet this long, so an editor's two-step save is read once, complete.
const DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Config {
    /// File format version; older files are migrated on load (C11).
    pub version: u32,
    /// Opened by Super+Enter and the `terminal` action.
    pub terminal: String,
    /// Software Update asks GitHub for a newer release once a day (E6.13); read at start.
    pub check_updates: bool,
    pub appearance: Appearance,
    pub keyboard: Keyboard,
    pub touchpad: Touchpad,
    pub mouse: Mouse,
    pub hot_corners: HotCorners,
    pub display: Displays,
    pub windows: Windows,
    pub clock: Clock,
    pub dock: Dock,
    pub idle: Idle,
    pub privacy: Privacy,
    pub weather: Weather,
    pub calendar: Calendar,
    pub update: Update,
    pub desktop: Desktop,
    pub notifications: Notifications,
    /// App name (as it sends notifications) → how they show.
    pub notification_apps: BTreeMap<String, AppStyle>,
    pub app_drawer: AppDrawer,
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
    /// `auto` is dark from this hour until `dark_until` (0 … 23, local time).
    pub dark_from: u32,
    pub dark_until: u32,
    /// Multiplier on the shell's label sizes, 0.8 … 1.5; also GTK `text-scaling-factor` and the Qt font size.
    pub text_scale: f32,
    /// Pointer size in px; 0 = `$XCURSOR_SIZE`, else 24. Apps we start get it as `XCURSOR_SIZE`.
    pub pointer_size: u32,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WeekStart {
    #[default]
    Sunday,
    Monday,
    Saturday,
}

impl From<WeekStart> for chrono::Weekday {
    fn from(start: WeekStart) -> Self {
        match start {
            WeekStart::Sunday => chrono::Weekday::Sun,
            WeekStart::Monday => chrono::Weekday::Mon,
            WeekStart::Saturday => chrono::Weekday::Sat,
        }
    }
}

/// Wallpaper fit (macOS: Fill Screen, Fit to Screen, Centre, Stretch to Fill Screen).
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
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Light,
    Dark,
    Auto,
}

/// The macOS accent set (Part D); theme.rs maps each to its light and dark colour.
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
    /// Super acts as Cmd (E1.6).
    pub mac_keys: bool,
    /// Key repeat: held this many ms, then this many times a second; 0 = off.
    pub repeat_delay: u32,
    pub repeat_rate: u32,
}

impl Default for Keyboard {
    fn default() -> Self {
        Self { layout: String::new(), variant: String::new(), options: String::new(), mac_keys: false, repeat_delay: 500, repeat_rate: 30 }
    }
}

impl Keyboard {
    /// The repeat delay and rate apps are told (smithay's order: rate, delay).
    pub fn repeat(&self) -> (i32, i32) {
        (self.repeat_rate.min(100) as i32, self.repeat_delay.clamp(100, 3000) as i32)
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
    pub tap_to_click: bool,
    pub natural_scroll: bool,
    /// Fingers of the Space / Mission Control swipes (E5.3): 3, or 4 to leave 3-finger swipes to apps.
    pub swipe_fingers: u8,
    /// Multiplies gesture travel: higher = a shorter swipe or pinch is enough (0.25 … 4).
    pub gesture_sensitivity: f64,
    /// Tracking speed, -1 (slow) … 1 (fast).
    pub speed: f64,
    pub secondary_click: SecondaryClick,
    /// Tap, then drag with the finger down; `drag_lock` keeps dragging through a short lift.
    pub tap_to_drag: bool,
    pub drag_lock: bool,
    pub disable_while_typing: bool,
    /// Off while a mouse is plugged in.
    pub ignore_with_mouse: bool,
}

/// How the trackpad right-clicks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecondaryClick {
    /// Click with two fingers (macOS).
    TwoFingers,
    /// Click in the bottom-right corner.
    Corner,
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
}

impl Default for Mouse {
    fn default() -> Self {
        Self { speed: 0.0, acceleration: true, left_handed: false, natural_scroll: false, middle_click_emulation: false }
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

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct Displays {
    /// The GPU that draws the desktop (`/dev/dri/card1`); empty: the one the firmware booted with. Read at start.
    pub gpu: String,
    /// Every display's scale (1.0 = 100%, 1.25, 1.5, 2.0 …); 0 = picked from each display's pixel density. Live.
    pub scale: f64,
    /// `[[display.outputs]]`: one display's own settings, by connector name. Live.
    pub outputs: Vec<OutputSettings>,
}

/// One display's settings (E6.2); a display without an entry gets the defaults below.
#[derive(Debug, Clone, PartialEq, Deserialize)]
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
}

/// The keys an `[[display.outputs]]` entry may have (unknown ones are warned about).
const OUTPUT_KEYS: [&str; 8] = ["name", "enabled", "mode", "refresh", "scale", "rotation", "position", "primary"];

impl Default for OutputSettings {
    fn default() -> Self {
        Self { name: String::new(), enabled: true, mode: String::new(), refresh: 0.0, scale: 0.0, rotation: 0, position: None, primary: false }
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
            version: 1,
            terminal: "noobsbot".into(),
            check_updates: true,
            appearance: Appearance::default(),
            keyboard: Keyboard::default(),
            touchpad: Touchpad::default(),
            mouse: Mouse::default(),
            hot_corners: HotCorners::default(),
            display: Displays::default(),
            windows: Windows::default(),
            clock: Clock::default(),
            dock: Dock::default(),
            idle: Idle::default(),
            privacy: Privacy::default(),
            weather: Weather::default(),
            calendar: Calendar::default(),
            update: Update::default(),
            desktop: Desktop::default(),
            notifications: Notifications::default(),
            notification_apps: BTreeMap::new(),
            app_drawer: AppDrawer::default(),
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
    /// Only pinned apps, no other running ones.
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
    /// Minimised windows go into their app's icon instead of tiles after the separator.
    pub minimise_into_icon: bool,
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

/// Notifications (E4.1).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Notifications {
    /// Do Not Disturb: banners stay quiet, Notification Center still collects them.
    pub dnd: bool,
}

/// One app's notification style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppStyle {
    /// Goes after a few seconds.
    Banner,
    /// Stays until dismissed.
    Alert,
    /// Notification Center only.
    None,
    /// Dropped.
    Off,
}

/// Desktop icons (E7.2): the files in `~/Desktop` on the wallpaper.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Desktop {
    pub icons: bool,
}

impl Default for Desktop {
    fn default() -> Self {
        Self { icons: true }
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

/// Screen-recording answers (E6.11), by program path; the "would like to record this screen" alert fills them in.
/// Calendar events (E7.2): `.ics` files, dirs or `dir/*.ics` patterns, read live.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Calendar {
    pub files: Vec<String>,
}

impl Default for Calendar {
    fn default() -> Self {
        Self { files: vec!["~/.local/share/noobsbot-desktop/calendars/*.ics".into()] }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Privacy {
    pub screen_recording: Vec<String>,
    pub screen_recording_denied: Vec<String>,
}

/// Software Update (E6.13); read at start.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Update {
    pub channel: crate::services::update::Channel,
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
            minimise_into_icon: false,
        }
    }
}

/// Idle and sleep (E6.5): seconds without input before each step; 0 = never.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Idle {
    pub dim: u32,
    pub screen_off: u32,
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
        Self { dim: 120, screen_off: 300, sleep_on_battery: 900, sleep_on_ac: 0, lock: true, lock_delay: 0, lock_message: String::new() }
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
            dark_from: 19,
            dark_until: 7,
            text_scale: 1.0,
            pointer_size: 0,
        }
    }
}

impl Default for Touchpad {
    fn default() -> Self {
        Self {
            tap_to_click: true,
            natural_scroll: true,
            swipe_fingers: 3,
            gesture_sensitivity: 1.0,
            speed: 0.0,
            secondary_click: SecondaryClick::TwoFingers,
            tap_to_drag: true,
            drag_lock: false,
            disable_while_typing: true,
            ignore_with_mouse: false,
        }
    }
}

impl Default for Windows {
    fn default() -> Self {
        Self { move_modifier: Modifier::Super, magnetic_edges: true, snapping: true, double_click_title: TitleAction::Zoom, spaces: 0 }
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
        ("Super+Ctrl+Q", "lock"),
        ("Super+N", "notification-center"),
        ("Super+Space", "spotlight"),
        ("Super+A", "app-drawer"),
        ("Super+Ctrl+Space", "input-source"),
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
        ("Super+Up", "mission-control"),
        ("Super+Down", "app-expose"),
        ("Super+F11", "show-desktop"),
        ("Super+Shift+3", "screenshot"),
        ("Super+Shift+4", "screenshot-region"),
        ("Super+Shift+5", "screenshot-toolbar"),
        ("Print", "screenshot-toolbar"),
        ("Shift+Print", "screenshot-region"),
        ("F12", "quake"),
        ("XF86AudioRaiseVolume", "volume up"),
        ("XF86AudioLowerVolume", "volume down"),
        ("XF86AudioMute", "mute"),
        ("XF86AudioMicMute", "mic-mute"),
        ("XF86MonBrightnessUp", "brightness up"),
        ("XF86MonBrightnessDown", "brightness down"),
        ("XF86AudioPlay", "media play-pause"),
        ("XF86AudioPause", "media play-pause"),
        ("XF86AudioNext", "media next"),
        ("XF86AudioPrev", "media previous"),
        ("XF86PowerOff", "shut-down"),
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
    if config.version != 1 {
        // ponytail: only version 1 exists; a migration step per version goes here when 2 appears.
        tracing::warn!("config version {} is not 1; reading the keys this version understands", config.version);
    }
    // A user [bindings] table is merged over the defaults; "none" frees a key.
    let mut bindings = default_bindings();
    bindings.extend(std::mem::take(&mut config.bindings));
    bindings.retain(|_, action| action != "none");
    config.bindings = bindings;
    config.appearance.glass_opacity = config.appearance.glass_opacity.clamp(0.0, 1.0);
    config.dock.icon_size = config.dock.icon_size.clamp(crate::shell::dock::MIN_ICON as u16, crate::shell::dock::MAX_ICON as u16);
    config.dock.zoom_percent = config.dock.zoom_percent.clamp(100, 300);
    Ok(config)
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
            (Some(toml::Value::Table(known)), toml::Value::Table(user)) if name != "bindings" => warn_unknown(&name, user, known),
            _ => {}
        }
    }
}

/// Reads and parses the file: `None` (logged) when it can't be read or parsed; a missing file is the defaults.
fn read(path: &Path) -> Option<Config> {
    match fs::read_to_string(path) {
        Ok(text) => parse(&text).map_err(|err| tracing::warn!("{}: {err}", path.display())).ok(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Some(Config::default()),
        Err(err) => {
            tracing::warn!("{}: {err}", path.display());
            None
        }
    }
}

/// Startup load: the file, or the defaults if it is missing or broken.
pub fn load(path: &Path) -> Config {
    read(path).unwrap_or_default()
}

/// Writes `contents` to a temp file next to `path`, syncs it and renames it over `path`, so a reader (or a crash)
/// only ever sees the old file or the new one.
pub fn save_atomic(path: &Path, contents: &str) -> io::Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "config path has no file name"))?;
    fs::create_dir_all(dir)?;
    // ponytail: one temp name per process; two threads of one process saving at once would share it.
    let tmp = dir.join(format!(".{}.{}.tmp", name.to_string_lossy(), std::process::id()));
    let written = fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(contents.as_bytes())?;
        f.sync_all()
    });
    if let Err(err) = written.and_then(|()| fs::rename(&tmp, path)) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    // The rename itself is durable once the directory is synced; failing that is not worth an error.
    let _ = fs::File::open(dir).and_then(|d| d.sync_all());
    Ok(())
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

/// `text` with `key = [items]` set in `[section]`, keeping every other line and comment; adds the section or the
/// key when missing, and replaces a multi-line array whole.
pub fn set_array(text: &str, section: &str, key: &str, items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|s| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))).collect();
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
    let text = fs::read_to_string(&path).unwrap_or_else(|_| DEFAULT_TOML.to_owned());
    save_atomic(&path, &edit(&text)).inspect_err(|err| tracing::warn!("{}: {err}", path.display())).is_ok()
}

type Edit = Box<dyn FnOnce(&str) -> String + Send>;

/// Rewrites `path` with `edit(current text)` on the `config-save` thread, so a save (a sync and two renames, tens of
/// ms on a slow disk) never stalls a frame. Edits of one file within `DEBOUNCE` of each other are applied in order
/// and written once: Ctrl+scrolling the dock's icon size saves at the end, not per notch. A missing file reads as "".
// ponytail: an edit queued in the last 300 ms before exit is lost; flush on quit if that ever matters.
pub fn save_later(path: PathBuf, edit: impl FnOnce(&str) -> String + Send + 'static) {
    static QUEUE: OnceLock<Mutex<mpsc::Sender<(PathBuf, Edit)>>> = OnceLock::new();
    let queue = QUEUE.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<(PathBuf, Edit)>();
        let spawned = std::thread::Builder::new().name("config-save".into()).spawn(move || {
            while let Ok(first) = rx.recv() {
                let mut edits = vec![first];
                while let Ok(next) = rx.recv_timeout(DEBOUNCE) {
                    edits.push(next);
                }
                let mut files: Vec<(PathBuf, Vec<Edit>)> = Vec::new();
                for (path, edit) in edits {
                    match files.iter_mut().find(|(p, _)| *p == path) {
                        Some((_, edits)) => edits.push(edit),
                        None => files.push((path, vec![edit])),
                    }
                }
                for (path, edits) in files {
                    let mut text = fs::read_to_string(&path).unwrap_or_default();
                    for edit in edits {
                        text = edit(&text);
                    }
                    if let Err(err) = save_atomic(&path, &text) {
                        tracing::warn!("{}: {err}", path.display());
                    }
                }
            }
        });
        if let Err(err) = spawned {
            tracing::warn!("saving settings on the main thread: {err}");
        }
        Mutex::new(tx)
    });
    let edit: Edit = Box::new(edit);
    if let Err(mpsc::SendError((path, edit))) = queue.lock().unwrap_or_else(|e| e.into_inner()).send((path, edit)) {
        // The thread never started: do it here rather than lose the save.
        let text = fs::read_to_string(&path).unwrap_or_default();
        if let Err(err) = save_atomic(&path, &edit(&text)) {
            tracing::warn!("{}: {err}", path.display());
        }
    }
}

/// Live reload on a worker thread. Watches the file's directory (editors replace the file by renaming), waits for
/// 300 ms of quiet, and calls `send` with each newly parsed config that differs from the last one sent. A file that
/// doesn't parse sends nothing, so the last good config stays. The thread ends when `send` returns false.
///
/// The compositor bridges it into calloop without an extra thread:
/// ```ignore
/// let (tx, rx) = calloop::channel::channel();
/// config::watch(&path, move |config| tx.send(config).is_ok());
/// handle.insert_source(rx, |event, _, state| if let channel::Event::Msg(config) = event { state.apply_config(config) })?;
/// ```
pub fn watch(path: &Path, mut send: impl FnMut(Config) -> bool + Send + 'static) {
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
        while let Ok(event) = rx.recv() {
            if !changed(event) {
                continue;
            }
            while rx.recv_timeout(DEBOUNCE).is_ok() {}
            let Some(config) = read(&path) else { continue };
            if last.as_ref() != Some(&config) {
                last = Some(config.clone());
                if !send(config) {
                    return;
                }
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
        let k = Keyboard { layout: "de".into(), variant: "neo".into(), options: "caps:escape".into(), mac_keys: true, ..Default::default() }.resolved();
        assert_eq!((k.layout.as_str(), k.variant.as_str(), k.options.as_str(), k.mac_keys), ("de", "neo", "caps:escape", true));
    }

    #[test]
    fn defaults_round_trip() {
        assert_eq!(parse(DEFAULT_TOML).unwrap(), Config::default());
        assert_eq!(parse("").unwrap(), Config::default());
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
    fn display_outputs() {
        let c = parse(concat!(
            "[display]\nscale = 1.5\n",
            "[[display.outputs]]\nname = \"eDP-1\"\nmode = \"1920x1200\"\nrefresh = 59.95\nscale = 1.25\nrotation = 90\nposition = [0, -200]\nprimary = true\nshiny = 1\n",
            "[[display.outputs]]\nname = \"HDMI-A-1\"\nenabled = false\n",
        ))
        .unwrap();
        assert_eq!(c.display.scale, 1.5);
        let [laptop, hdmi] = &c.display.outputs[..] else { panic!("{:?}", c.display.outputs) };
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
                primary: true
            }
        );
        assert_eq!(hdmi, &OutputSettings { name: "HDMI-A-1".into(), enabled: false, ..Default::default() }, "unset keys: the defaults");
    }

    #[test]
    fn unknown_keys_tolerated() {
        let c = parse("colour = 1\n[windows]\nsnaping = false\n[future]\nx = 1\n[[rules]]\napp_id = \"x\"\nwobbly = true").unwrap();
        assert!(c.windows.snapping);
        assert_eq!(c.rules.len(), 1);
    }

    #[test]
    fn bad_toml_keeps_previous() {
        let err = parse("version = 1\n[windows]\nsnapping = maybe").unwrap_err().to_string();
        assert!(err.contains("line 3"), "{err}");
        assert!(parse("[appearance]\nmode = \"sepia\"").is_err());

        let dir = temp_dir("bad");
        let path = dir.join("config.toml");
        fs::write(&path, "[windows\n").unwrap();
        assert_eq!(read(&path), None);
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
    fn live_reload() {
        let dir = temp_dir("watch");
        let path = dir.join("config.toml");
        let (tx, rx) = mpsc::channel();
        watch(&path, move |c| tx.send(c).is_ok());
        std::thread::sleep(Duration::from_millis(200));

        save_atomic(&path, "[windows]\nsnapping = false\n").unwrap();
        assert!(!rx.recv_timeout(Duration::from_secs(3)).unwrap().windows.snapping);

        fs::write(&path, "[windows\nsnapping = true").unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(800)).is_err(), "a broken file sends nothing");

        fs::write(&path, "[windows]\nmagnetic_edges = false\n").unwrap();
        let c = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(c.windows.snapping && !c.windows.magnetic_edges);
        drop(rx);
        fs::remove_dir_all(dir).unwrap();
    }
}
