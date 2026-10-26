//! The pages: every config setting once, in a table (`SETTINGS`) that both the pages and Search read, plus the
//! system pages' own rows. Grouped card rows, label left and control right (E7.1).

use std::{collections::HashMap, sync::LazyLock};

use iced::{
    Alignment, Color, Element, Length,
    font::Weight,
    widget::{button, canvas, column, container, pick_list, row, slider, space, svg, text, text_input, toggler},
};

use crate::{
    App, Message,
    arrange::Arrangement,
    display_info::Display,
    font,
    style::{Tokens, rgb},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Wifi,
    Bluetooth,
    Sound,
    Displays,
    Appearance,
    Accessibility,
    Dock,
    Keyboard,
    Mouse,
    Trackpad,
    DateTime,
    Spaces,
    Notifications,
    Battery,
    LockScreen,
    General,
    About,
}

impl Page {
    pub const ALL: [Page; 17] = [
        Page::Wifi,
        Page::Bluetooth,
        Page::Sound,
        Page::Displays,
        Page::Appearance,
        Page::Accessibility,
        Page::Dock,
        Page::Keyboard,
        Page::Mouse,
        Page::Trackpad,
        Page::DateTime,
        Page::Spaces,
        Page::Notifications,
        Page::Battery,
        Page::LockScreen,
        Page::General,
        Page::About,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Page::Wifi => "Wi-Fi",
            Page::Bluetooth => "Bluetooth",
            Page::Sound => "Sound",
            Page::Displays => "Displays",
            Page::Appearance => "Appearance",
            Page::Accessibility => "Accessibility",
            Page::Dock => "Desktop & Dock",
            Page::Keyboard => "Keyboard",
            Page::Mouse => "Mouse",
            Page::Trackpad => "Trackpad",
            Page::DateTime => "Date & Time",
            Page::Spaces => "Spaces",
            Page::Notifications => "Notifications",
            Page::Battery => "Battery",
            Page::LockScreen => "Lock Screen",
            Page::General => "General",
            Page::About => "About",
        }
    }

    /// `--page appearance`, `--page date-time`, …: the page's name in lowercase, spaces as dashes.
    pub fn parse(name: &str) -> Option<Page> {
        let slug = |p: Page| p.name().to_lowercase().replace(" & ", "-").replace(' ', "-");
        Page::ALL.into_iter().find(|p| slug(*p) == name)
    }

    /// Pages that read the system services when opened.
    pub fn is_system(self) -> bool {
        matches!(self, Page::Wifi | Page::Bluetooth | Page::Sound | Page::DateTime | Page::Battery)
    }

    /// A little space starts a new block in the sidebar.
    pub fn gap_before(self) -> bool {
        matches!(self, Page::Appearance | Page::Notifications | Page::General)
    }

    /// Phosphor fill symbol and squircle colour.
    fn icon(self) -> (&'static str, u32) {
        match self {
            Page::Wifi => ("wifi-high", 0x007AFF),
            Page::Bluetooth => ("bluetooth", 0x007AFF),
            Page::Sound => ("speaker-high", 0xFF2D55),
            Page::Displays => ("monitor", 0x0A84FF),
            Page::Appearance => ("circle-half", 0x3A3A3C),
            Page::Accessibility => ("user-circle", 0x007AFF),
            Page::Dock => ("app-window", 0x3A3A3C),
            Page::Keyboard => ("keyboard", 0x8E8E93),
            Page::Mouse => ("mouse-simple", 0x8E8E93),
            Page::Trackpad => ("cursor-click", 0x8E8E93),
            Page::DateTime => ("clock", 0x007AFF),
            Page::Spaces => ("squares-four", 0x5856D6),
            Page::Notifications => ("bell", 0xFF3B30),
            Page::Battery => ("battery-high", 0x34C759),
            Page::LockScreen => ("lock-simple", 0x3A3A3C),
            Page::General => ("gear-six", 0x8E8E93),
            Page::About => ("info", 0x8E8E93),
        }
    }
}

macro_rules! symbols {
    ($($name:literal),* $(,)?) => {
        static SYMBOLS: LazyLock<HashMap<&'static str, svg::Handle>> = LazyLock::new(|| HashMap::from([
            $(($name, svg::Handle::from_memory(include_bytes!(concat!("../../assets/phosphor/fill/", $name, ".svg")).as_slice())),)*
        ]));
    };
}

symbols![
    "wifi-high",
    "wifi-medium",
    "wifi-low",
    "check",
    "bluetooth",
    "speaker-high",
    "monitor",
    "circle-half",
    "app-window",
    "keyboard",
    "mouse-simple",
    "cursor-click",
    "clock",
    "squares-four",
    "bell",
    "battery-high",
    "lock-simple",
    "gear-six",
    "info",
    "magnifying-glass",
    "user-circle",
    "check",
];

/// A Phosphor fill symbol, loaded once.
pub fn icon(name: &str) -> svg::Handle {
    SYMBOLS[name].clone()
}

/// The page's icon: a white symbol in a colourful squircle.
pub fn squircle<'a>(page: Page, size: f32) -> Element<'a, Message> {
    let (name, color) = page.icon();
    let glyph = svg(icon(name)).width(size * 0.65).height(size * 0.65).style(Tokens::tint(Color::WHITE));
    container(glyph).center(size).style(Tokens::squircle(crate::style::rgb(color), size * 0.24)).into()
}

/// Off or a wait, for dwell and hold clicks.
const DWELL: &[(i64, &str)] = &[(0, "Off"), (500, "0.5 s"), (1000, "1 s"), (1500, "1.5 s"), (2000, "2 s")];

/// What a hot corner can do: an action name from `[bindings]`, and its label.
const CORNER_ACTIONS: &[(&str, &str)] = &[
    ("none", "Nothing"),
    ("mission-control", "Mission Control"),
    ("app-expose", "App Exposé"),
    ("show-desktop", "Desktop"),
    ("launchpad", "Launchpad"),
    ("notification-center", "Notification Center"),
    ("lock", "Lock Screen"),
    ("display-off", "Turn Display Off"),
];

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Toggle,
    /// Value written, label shown.
    Choice(&'static [(&'static str, &'static str)]),
    /// As `Choice`, in a dropdown (too many for segments); a value not listed shows as written.
    Menu(&'static [(&'static str, &'static str)]),
    Slider {
        min: f64,
        max: f64,
        step: f64,
    },
    Text(&'static str),
    Accent,
    /// A time zone, searched as you type.
    Zone,
    /// One xkb option in a comma list (the key is `keyboard.options`): its family, and the choices ("" = none).
    XkbOption(fn(&str) -> bool, &'static [(&'static str, &'static str)]),
    /// A whole number from a few steps: value written, label shown.
    Steps(&'static [(i64, &'static str)]),
    /// A sound from the system's sound theme, with a button that plays it.
    Sound,
}

pub struct Setting {
    pub page: Page,
    pub group: &'static str,
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: Kind,
}

const fn s(page: Page, group: &'static str, key: &'static str, label: &'static str, help: &'static str, kind: Kind) -> Setting {
    Setting { page, group, key, label, help, kind }
}

/// Every setting in `config.toml` (`data/config.toml`), in page order.
pub const SETTINGS: &[Setting] = &[
    s(Page::Sound, "Sound Effects", "sound.alert", "Alert sound", "Played when an app rings the bell", Kind::Sound),
    s(Page::Sound, "Sound Effects", "sound.ui_sounds", "Play user interface sound effects", "Screenshots, emptying the Trash, volume keys", Kind::Toggle),
    s(
        Page::Appearance,
        "",
        "appearance.mode",
        "Appearance",
        "Auto follows the schedule (light until it is set)",
        Kind::Choice(&[("light", "Light"), ("dark", "Dark"), ("auto", "Auto")]),
    ),
    s(Page::Appearance, "", "appearance.dark_from", "Auto: dark from", "", Kind::Slider { min: 0.0, max: 23.0, step: 1.0 }),
    s(Page::Appearance, "", "appearance.dark_until", "Auto: light from", "", Kind::Slider { min: 0.0, max: 23.0, step: 1.0 }),
    s(
        Page::Appearance,
        "",
        "appearance.auto_sunset",
        "Auto: sunset to sunrise",
        "At the Weather city or coordinates; the hours above without one",
        Kind::Toggle,
    ),
    s(Page::Appearance, "", "appearance.accent", "Accent colour", "", Kind::Accent),
    s(Page::Appearance, "", "appearance.glass_opacity", "Glass opacity", "100% = solid (Reduce transparency)", Kind::Slider { min: 0.5, max: 1.0, step: 0.05 }),
    s(Page::Appearance, "Wallpaper", "appearance.wallpaper", "Light wallpaper", "PNG or JPEG; empty = opal gradient", Kind::Text("Built-in opal")),
    s(Page::Appearance, "Wallpaper", "appearance.wallpaper_dark", "Dark wallpaper", "Used in dark mode", Kind::Text("Built-in dark opal")),
    s(
        Page::Appearance,
        "Wallpaper",
        "appearance.wallpaper_fit",
        "Show picture",
        "",
        Kind::Choice(&[("fill", "Fill"), ("fit", "Fit"), ("centre", "Centre"), ("stretch", "Stretch")]),
    ),
    s(Page::Appearance, "Wallpaper", "appearance.wallpaper_folder", "Slideshow folder", "Shows its pictures in turn; empty = off", Kind::Text("Off")),
    s(
        Page::Appearance,
        "Wallpaper",
        "appearance.wallpaper_interval",
        "Change picture",
        "",
        Kind::Steps(&[(0, "At login"), (5, "Every 5 min"), (15, "Every 15 min"), (30, "Every 30 min"), (60, "Every hour"), (1440, "Every day")]),
    ),
    s(Page::Appearance, "Wallpaper", "appearance.wallpaper_shuffle", "Random order", "", Kind::Toggle),
    s(
        Page::Accessibility,
        "Display",
        "appearance.text_scale",
        "Text size",
        "Menus, menu bar, dock and notifications; GTK and Qt apps follow",
        Kind::Slider { min: 0.8, max: 1.5, step: 0.05 },
    ),
    s(
        Page::Accessibility,
        "Display",
        "appearance.pointer_size",
        "Pointer size",
        "Apps started after the change follow",
        Kind::Slider { min: 0.0, max: 96.0, step: 8.0 },
    ),
    s(Page::Accessibility, "Display", "appearance.reduce_motion", "Reduce motion", "Windows, menus and banners crossfade instead of moving", Kind::Toggle),
    s(Page::Accessibility, "Display", "accessibility.increase_contrast", "Increase contrast", "Glass near-opaque, with visible borders", Kind::Toggle),
    s(
        Page::Accessibility,
        "Display",
        "accessibility.colour_filter",
        "Colour filter",
        "Screenshots stay true to colour",
        Kind::Choice(&[("none", "None"), ("invert", "Invert"), ("invert-hue", "Invert, keep colours"), ("greyscale", "Greyscale")]),
    ),
    s(
        Page::Accessibility,
        "Zoom",
        "accessibility.zoom_style",
        "Zoom style",
        "Super+Alt+8 turns Zoom on or off, Super+Alt+= and Super+Alt+- zoom in and out",
        Kind::Choice(&[("full", "Full Screen"), ("lens", "Picture-in-Picture")]),
    ),
    s(Page::Accessibility, "Keyboard", "accessibility.sticky_keys", "Sticky keys", "Tap a modifier for the next key; tap twice to keep it", Kind::Toggle),
    s(Page::Accessibility, "Keyboard", "accessibility.sticky_keys_shortcut", "Press Shift five times for sticky keys", "", Kind::Toggle),
    s(
        Page::Accessibility,
        "Keyboard",
        "accessibility.slow_keys_ms",
        "Slow keys",
        "A key counts only once held this long",
        Kind::Steps(&[(0, "Off"), (150, "150 ms"), (300, "300 ms"), (500, "500 ms"), (1000, "1 s")]),
    ),
    s(
        Page::Accessibility,
        "Keyboard",
        "accessibility.bounce_keys_ms",
        "Bounce keys",
        "Ignore a key pressed again this soon",
        Kind::Steps(&[(0, "Off"), (100, "100 ms"), (250, "250 ms"), (500, "500 ms")]),
    ),
    s(Page::Accessibility, "Keyboard", "accessibility.mouse_keys", "Mouse keys", "Keypad 1–9 move the pointer, 5 clicks, 0 holds, . lets go", Kind::Toggle),
    s(
        Page::Accessibility,
        "Alerts",
        "accessibility.bell",
        "Alert bell",
        "When an app rings the bell",
        Kind::Choice(&[("sound", "Play sound"), ("flash", "Flash the screen"), ("both", "Sound and flash"), ("off", "Off")]),
    ),
    s(Page::Accessibility, "Keyboard", "accessibility.on_screen_keyboard", "On-screen keyboard", "Click keys to type, on the lock screen too", Kind::Toggle),
    s(Page::Accessibility, "Pointer", "accessibility.shake_to_find", "Shake pointer to locate", "Shake it quickly and it grows for a moment", Kind::Toggle),
    s(
        Page::Accessibility,
        "Pointer",
        "accessibility.dwell_click_ms",
        "Dwell click",
        "Resting the pointer clicks; a ring shrinks while it waits",
        Kind::Steps(DWELL),
    ),
    s(
        Page::Accessibility,
        "Pointer",
        "accessibility.hold_secondary_ms",
        "Hold for secondary click",
        "Holding the primary button still gives a right click",
        Kind::Steps(DWELL),
    ),
    s(
        Page::Accessibility,
        "Screen reader",
        "accessibility.screen_reader",
        "Screen reader",
        "Orca reads the screen aloud; Super+Alt+F5 toggles it",
        Kind::Toggle,
    ),
    s(Page::Appearance, "Desktop", "desktop.icons", "Show Desktop folder on the wallpaper", "", Kind::Toggle),
    s(Page::Appearance, "Desktop", "desktop.show_drives", "Show mounted drives on the desktop", "", Kind::Toggle),
    s(Page::Appearance, "Apps", "appearance.theme_apps", "Theme Qt apps", "Dark Mode also rewrites the Noobsbot Glass theme files", Kind::Toggle),
    s(Page::Displays, "Night Light", "night_light.enabled", "Night Light", "Warmer colours on a schedule; screenshots stay true", Kind::Toggle),
    s(
        Page::Displays,
        "Night Light",
        "night_light.schedule",
        "Schedule",
        "Sunset uses the Weather city or coordinates",
        Kind::Choice(&[("custom", "Custom"), ("sunset", "Sunset to Sunrise")]),
    ),
    s(Page::Displays, "Night Light", "night_light.from", "From", "", Kind::Slider { min: 0.0, max: 23.0, step: 1.0 }),
    s(Page::Displays, "Night Light", "night_light.until", "Until", "", Kind::Slider { min: 0.0, max: 23.0, step: 1.0 }),
    s(
        Page::Displays,
        "Night Light",
        "night_light.temperature",
        "Colour temperature",
        "Lower is warmer",
        Kind::Slider { min: 2000.0, max: 6500.0, step: 100.0 },
    ),
    s(Page::Dock, "Dock", "dock.icon_size", "Size", "Icons shrink when they don't fit the screen", Kind::Slider { min: 32.0, max: 128.0, step: 2.0 }),
    s(Page::Dock, "Dock", "dock.zoom", "Magnification", "Icons grow under the pointer", Kind::Toggle),
    s(Page::Dock, "Dock", "dock.zoom_percent", "Magnified size", "", Kind::Slider { min: 100.0, max: 300.0, step: 5.0 }),
    s(
        Page::Dock,
        "Dock",
        "dock.hide",
        "Hide the Dock",
        "Intelligent: while a window overlaps it",
        Kind::Choice(&[("intelligent", "Intelligent"), ("never", "Never")]),
    ),
    s(Page::Dock, "Dock", "dock.minimise_into_icon", "Minimise windows into application icon", "", Kind::Toggle),
    s(Page::Dock, "Dock", "dock.minimise_effect", "Minimise windows using", "", Kind::Choice(&[("scale", "Scale Effect"), ("genie", "Genie Effect")])),
    s(Page::Dock, "Dock", "dock.show_trash", "Show Trash in Dock", "", Kind::Toggle),
    s(
        Page::Dock,
        "Dock",
        "dock.click",
        "Click the active app to",
        "Shift-click opens a new window",
        Kind::Choice(&[("toggle", "Minimise it"), ("cycle", "Show its next window")]),
    ),
    s(Page::Dock, "Dock", "dock.middle", "Middle-click an app to", "", Kind::Choice(&[("new", "Open a new window"), ("close", "Close its windows")])),
    s(Page::Dock, "Dock", "dock.scroll", "Scroll on an app to cycle its windows", "", Kind::Choice(&[("on", "On"), ("reverse", "Reversed"), ("off", "Off")])),
    s(Page::Dock, "Menu Bar", "menu_bar.auto_hide", "Automatically hide and show the menu bar", "Shows when the pointer touches the top edge", Kind::Toggle),
    s(
        Page::Dock,
        "Windows",
        "windows.move_modifier",
        "Drag windows with",
        "Hold the key and drag anywhere in a window",
        Kind::Choice(&[("super", "Super"), ("alt", "Alt")]),
    ),
    s(Page::Dock, "Windows", "windows.magnetic_edges", "Magnetic edges", "Edges stick to screen and window edges", Kind::Toggle),
    s(Page::Dock, "Windows", "windows.snapping", "Tile at screen edges", "Drag to a side, a corner or the top; hold Ctrl to skip", Kind::Toggle),
    s(
        Page::Dock,
        "Windows",
        "windows.double_click_title",
        "Double-click a title bar to",
        "",
        Kind::Choice(&[("zoom", "Zoom"), ("minimise", "Minimise"), ("nothing", "Nothing")]),
    ),
    s(Page::Dock, "Hot Corners", "hot_corners.top_left", "Top left", "", Kind::Menu(CORNER_ACTIONS)),
    s(Page::Dock, "Hot Corners", "hot_corners.top_right", "Top right", "", Kind::Menu(CORNER_ACTIONS)),
    s(Page::Dock, "Hot Corners", "hot_corners.bottom_left", "Bottom left", "", Kind::Menu(CORNER_ACTIONS)),
    s(Page::Dock, "Hot Corners", "hot_corners.bottom_right", "Bottom right", "", Kind::Menu(CORNER_ACTIONS)),
    s(Page::Keyboard, "Key repeat", "keyboard.repeat_rate", "Key repeat rate", "", Kind::Slider { min: 0.0, max: 60.0, step: 1.0 }),
    s(Page::Keyboard, "Key repeat", "keyboard.repeat_delay", "Delay until repeat", "", Kind::Slider { min: 150.0, max: 1500.0, step: 50.0 }),
    s(Page::Keyboard, "Modifier keys", "keyboard.options", "Caps Lock key", "", Kind::XkbOption(crate::xkb::is_caps, crate::xkb::CAPS)),
    s(
        Page::Keyboard,
        "Modifier keys",
        "keyboard.options",
        "Compose key",
        "Then type a sequence, like ' e for é",
        Kind::XkbOption(crate::xkb::is_compose, crate::xkb::COMPOSE),
    ),
    s(Page::Keyboard, "Modifier keys", "keyboard.options", "Other xkb options", "Like grp:alt_shift_toggle; Caps Lock and Compose above", Kind::Text("None")),
    s(Page::Trackpad, "", "touchpad.enabled", "Trackpad", "Off: only a mouse moves the pointer", Kind::Toggle),
    s(Page::Trackpad, "", "touchpad.tap_to_click", "Tap to click", "", Kind::Toggle),
    s(Page::Trackpad, "", "touchpad.natural_scroll", "Natural scrolling", "Content follows your fingers", Kind::Toggle),
    s(Page::Trackpad, "", "touchpad.speed", "Tracking speed", "", Kind::Slider { min: -1.0, max: 1.0, step: 0.1 }),
    s(
        Page::Trackpad,
        "",
        "touchpad.secondary_click",
        "Secondary click",
        "",
        Kind::Choice(&[("two-fingers", "Two fingers"), ("corner", "Bottom-right corner"), ("off", "Off")]),
    ),
    s(
        Page::Trackpad,
        "",
        "touchpad.scroll_method",
        "Scroll with",
        "",
        Kind::Choice(&[("two-fingers", "Two fingers"), ("edge", "One finger on the edge"), ("none", "Off")]),
    ),
    s(Page::Trackpad, "", "touchpad.tap_to_drag", "Tap to drag", "Tap, then drag with the finger down", Kind::Toggle),
    s(Page::Trackpad, "", "touchpad.drag_lock", "Drag lock", "Keep dragging through a short lift", Kind::Toggle),
    s(Page::Trackpad, "", "touchpad.disable_while_typing", "Ignore while typing", "", Kind::Toggle),
    s(Page::Trackpad, "", "touchpad.ignore_with_mouse", "Ignore when a mouse is plugged in", "", Kind::Toggle),
    s(Page::Trackpad, "Gestures", "touchpad.gesture_spaces", "Swipe between Spaces", "Swipe left or right", Kind::Toggle),
    s(Page::Trackpad, "Gestures", "touchpad.gesture_mission_control", "Mission Control", "Swipe up", Kind::Toggle),
    s(Page::Trackpad, "Gestures", "touchpad.gesture_app_expose", "App Exposé", "Swipe down", Kind::Toggle),
    s(Page::Trackpad, "Gestures", "touchpad.gesture_launchpad", "Launchpad", "Pinch with four fingers", Kind::Toggle),
    s(Page::Trackpad, "Gestures", "touchpad.gesture_show_desktop", "Show Desktop", "Spread four fingers", Kind::Toggle),
    s(Page::Mouse, "", "mouse.speed", "Tracking speed", "", Kind::Slider { min: -1.0, max: 1.0, step: 0.1 }),
    s(Page::Mouse, "", "mouse.acceleration", "Pointer acceleration", "Faster movement goes further", Kind::Toggle),
    s(Page::Mouse, "", "mouse.natural_scroll", "Natural scrolling", "Content follows the wheel", Kind::Toggle),
    s(Page::Mouse, "", "mouse.left_handed", "Primary button on the right", "The trackpad follows", Kind::Toggle),
    s(Page::Mouse, "", "mouse.middle_click_emulation", "Left + right = middle click", "", Kind::Toggle),
    s(Page::Mouse, "", "mouse.double_click_ms", "Double-click speed", "Longest gap between two clicks", Kind::Slider { min: 150.0, max: 1000.0, step: 50.0 }),
    s(Page::Mouse, "", "mouse.middle_click_paste", "Middle click pastes", "The last selected text", Kind::Toggle),
    s(Page::DateTime, "", "clock.timezone", "Time zone", "Type a city; picking one also sets the system's", Kind::Zone),
    s(Page::DateTime, "", "clock.hour24", "24-hour time", "", Kind::Toggle),
    s(Page::DateTime, "Menu bar clock", "clock.weekday", "Show the day of the week", "", Kind::Toggle),
    s(Page::DateTime, "Menu bar clock", "clock.date", "Show the date", "", Kind::Toggle),
    s(Page::DateTime, "Menu bar clock", "clock.seconds", "Show seconds", "", Kind::Toggle),
    s(
        Page::DateTime,
        "Calendar",
        "clock.week_start",
        "Weeks start on",
        "",
        Kind::Choice(&[("auto", "Region default"), ("sunday", "Sunday"), ("monday", "Monday"), ("saturday", "Saturday")]),
    ),
    s(Page::DateTime, "Calendar", "clock.week_numbers", "Show week numbers", "In the Notification Center calendar", Kind::Toggle),
    s(Page::Spaces, "", "windows.spaces", "Spaces", "Dynamic: + in Mission Control adds more", Kind::Slider { min: 0.0, max: 16.0, step: 1.0 }),
    s(Page::Spaces, "", "spaces.per_display", "Displays have separate Spaces", "Off: every display switches Spaces together", Kind::Toggle),
    s(Page::Notifications, "", "notifications.dnd", "Do Not Disturb", "Banners stay quiet; Notification Center still collects them", Kind::Toggle),
    s(
        Page::Notifications,
        "",
        "notifications.previews",
        "Show previews",
        "A notification's text in banners and Notification Center",
        Kind::Choice(&[("always", "Always"), ("unlocked", "When Unlocked"), ("never", "Never")]),
    ),
    s(Page::Notifications, "", "notifications.full_screen", "Allow notifications when a window is full screen", "", Kind::Toggle),
    s(
        Page::Notifications,
        "Do Not Disturb",
        "notifications.dnd_scheduled",
        "Turn on on a schedule",
        "Turns itself on and off; the Control Center overrides it until then",
        Kind::Toggle,
    ),
    s(Page::Notifications, "Do Not Disturb", "notifications.dnd_from", "From", "", Kind::Slider { min: 0.0, max: 23.0, step: 1.0 }),
    s(Page::Notifications, "Do Not Disturb", "notifications.dnd_until", "To", "", Kind::Slider { min: 0.0, max: 23.0, step: 1.0 }),
    s(Page::Notifications, "Weather", "weather.enabled", "Weather widget", "Forecast from Open-Meteo every 30 minutes; sends only the place", Kind::Toggle),
    s(Page::Notifications, "Weather", "weather.city", "City", "Looked up once; never located by IP", Kind::Text("Kathmandu")),
    s(Page::Notifications, "Weather", "weather.units", "Units", "", Kind::Choice(&[("celsius", "°C"), ("fahrenheit", "°F")])),
    s(Page::LockScreen, "", "idle.dim", "Dim the display when inactive", "", Kind::Slider { min: 0.0, max: 600.0, step: 30.0 }),
    s(
        Page::LockScreen,
        "",
        "idle.screen_off",
        "Turn display off when inactive",
        "On the power adapter; on battery see Battery",
        Kind::Slider { min: 0.0, max: 3600.0, step: 60.0 },
    ),
    s(Page::LockScreen, "", "idle.lock", "Lock when the display turns off", "It always locks before sleeping", Kind::Toggle),
    s(
        Page::LockScreen,
        "",
        "idle.lock_delay",
        "Require password after the display turns off",
        "",
        Kind::Steps(&[(0, "Immediately"), (5, "5 s"), (60, "1 min"), (300, "5 min"), (3600, "1 h")]),
    ),
    s(Page::LockScreen, "", "idle.lock_message", "Message on the lock screen", "Shown under the clock", Kind::Text("If found, please call …")),
    s(Page::Battery, "", "battery.show_percentage", "Show percentage", "Beside the battery in the menu bar", Kind::Toggle),
    s(Page::Battery, "Display", "battery.dim_on_battery", "Slightly dim the display on battery", "Brighter again when plugged in", Kind::Toggle),
    s(Page::Battery, "Display", "idle.screen_off_on_battery", "On battery, turn display off after", "", Kind::Slider { min: 0.0, max: 3600.0, step: 60.0 }),
    s(
        Page::Battery,
        "Keyboard backlight",
        "idle.keyboard_backlight_off",
        "Turn off after inactivity",
        "Back on at the next key or move",
        Kind::Slider { min: 0.0, max: 300.0, step: 5.0 },
    ),
    s(Page::Battery, "Sleep", "idle.sleep_on_battery", "On battery, sleep after", "", Kind::Slider { min: 0.0, max: 3600.0, step: 60.0 }),
    s(Page::Battery, "Sleep", "idle.sleep_on_ac", "On power adapter, sleep after", "", Kind::Slider { min: 0.0, max: 7200.0, step: 300.0 }),
    s(Page::General, "", "terminal", "Terminal", "Opened by Super+Return", Kind::Text("noobsbot")),
    s(
        Page::General,
        "Storage",
        "housekeeping.empty_trash_days",
        "Empty Trash items",
        "Deleted for good this long after going to the Trash",
        Kind::Steps(&[(0, "Never"), (7, "After 7 days"), (30, "After 30 days"), (90, "After 90 days")]),
    ),
    s(
        Page::General,
        "Storage",
        "housekeeping.recent_days",
        "Forget recent files",
        "Older entries leave Recent Items",
        Kind::Steps(&[(0, "Never"), (7, "After 7 days"), (30, "After 30 days"), (90, "After 90 days")]),
    ),
    s(Page::General, "Software Update", "check_updates", "Check for updates daily", "Sends nothing but the version; installs at next login", Kind::Toggle),
    s(Page::General, "Session", "windows.reopen_at_login", "Reopen windows when logging back in", "Also the Log Out alert's checkbox", Kind::Toggle),
];

pub fn setting(key: &str) -> Option<&'static Setting> {
    SETTINGS.iter().find(|s| s.key == key)
}

/// A whole-number slider's value as written: Spaces has no fixed count of 1, so 1 reads as dynamic.
pub fn whole(key: &str, value: f64) -> i64 {
    let value = value.round() as i64;
    if key == "windows.spaces" && value == 1 { 0 } else { value }
}

/// The value beside a slider.
fn shown(key: &str, value: f64) -> String {
    match key {
        "appearance.glass_opacity" => format!("{:.0}%", value * 100.0),
        "dock.zoom_percent" => format!("{value:.0}%"),
        "appearance.pointer_size" if value < 1.0 => "System".into(),
        "dock.icon_size" | "appearance.pointer_size" => format!("{value:.0} px"),
        "appearance.text_scale" => format!("{:.0}%", value * 100.0),
        "windows.spaces" if whole(key, value) == 0 => "Dynamic".into(),
        "sound.output" | "sound.input" | "keyboard.backlight" | "display.brightness" => format!("{value:.0}%"),
        "sound.balance" if value.abs() < 0.5 => "Centre".into(),
        "sound.balance" => format!("{} {:.0}", if value < 0.0 { "L" } else { "R" }, value.abs()),
        "keyboard.repeat_rate" if value < 1.0 => "Off".into(),
        "keyboard.repeat_rate" => format!("{value:.0}/s"),
        "keyboard.repeat_delay" | "mouse.double_click_ms" => format!("{value:.0} ms"),
        "appearance.dark_from" | "appearance.dark_until" | "night_light.from" | "night_light.until" | "notifications.dnd_from" | "notifications.dnd_until" => {
            format!("{value:02.0}:00")
        }
        "night_light.temperature" => format!("{value:.0} K"),
        "mouse.speed" | "touchpad.speed" => format!("{:.0}%", (value + 1.0) * 50.0),
        k if k.starts_with("idle.") => match value.round() as u32 {
            0 => "Never".into(),
            s if s < 60 => format!("{s} s"),
            s if s % 60 == 0 => format!("{} min", s / 60),
            s => format!("{} min {} s", s / 60, s % 60),
        },
        _ => format!("{value:.0}"),
    }
}

fn title<'a>(name: &'a str) -> Element<'a, Message> {
    text(name).size(26).line_height(iced::Pixels(32.0)).into()
}

/// A grouped card: rows split by inset hairlines, with an optional header above (D4 Subheadline).
fn group<'a>(t: Tokens, header: &'a str, rows: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    let mut card = column![];
    let count = rows.len();
    for (i, row) in rows.into_iter().enumerate() {
        card = card.push(row);
        if i + 1 < count {
            card = card.push(container(container(space()).height(1).width(Length::Fill).style(t.rule())).padding([0, 12]));
        }
    }
    let card = container(card).width(Length::Fill).style(t.group());
    if header.is_empty() {
        return card.into();
    }
    column![container(text(header).size(13).font(font(Weight::Semibold))).padding([0, 4]), card].spacing(8).into()
}

/// Label (and help) left, control right.
fn line<'a>(t: Tokens, label: impl text::IntoFragment<'a>, help: &'a str, control: Element<'a, Message>) -> Element<'a, Message> {
    let mut words = column![text(label).size(13)].spacing(2);
    if !help.is_empty() {
        words = words.push(text(help).size(11).color(t.secondary()));
    }
    row![words.width(Length::Fill), control].spacing(12).align_y(Alignment::Center).padding([8, 12]).height(Length::Shrink).into()
}

/// A read-only row: label left, value right in `label-secondary`.
fn fact<'a>(t: Tokens, label: impl text::IntoFragment<'a>, value: String) -> Element<'a, Message> {
    line(t, label, "", text(value).size(13).color(t.secondary()).into())
}

fn toggle<'a>(t: Tokens, on: bool, message: impl Fn(bool) -> Message + 'a) -> Element<'a, Message> {
    toggler(on).on_toggle(message).size(18).style(t.toggle()).into()
}

fn control<'a>(app: &'a App, setting: &'static Setting) -> Element<'a, Message> {
    let t = app.tokens;
    let key = setting.key;
    match setting.kind {
        Kind::Toggle => toggle(t, app.config.bool(key), move |on| Message::Toggle(key, on)),
        Kind::Choice(options) => choice(t, app.config.str(key), options, key),
        Kind::Menu(options) => {
            let current = app.config.str(key);
            let selected = options.iter().find(|o| o.0 == current).map(|&(v, l)| Labelled(v, l));
            let pick =
                pick_list(options.iter().map(|&(v, l)| Labelled(v, l)).collect::<Vec<_>>(), selected, move |c: Labelled| Message::Choose(key.into(), c.0));
            pick.placeholder(current).text_size(13).padding([3, 8]).style(t.pick()).menu_style(t.menu()).into()
        }
        Kind::Steps(steps) => {
            let current = app.config.num(key) as i64;
            segments(t, steps.iter().map(|&(n, label)| (label.to_owned(), current == n, Message::Number(key, n))).collect())
        }
        Kind::Slider { min, max, step } => {
            let value = app.drafts.get(key).copied().unwrap_or_else(|| app.config.num(key));
            slider_row(t, key, min..=max, step, value)
        }
        Kind::Text(placeholder) => {
            let value = app.texts.get(key).map_or_else(|| app.config.str(key), String::as_str);
            text_input(placeholder, value)
                .on_input(move |v| Message::Type(key.into(), v))
                .on_submit(Message::Submit(key.into()))
                .size(13)
                .padding([3, 8])
                .width(180)
                .style(t.input(false))
                .into()
        }
        Kind::Accent => {
            let current = app.config.str(key);
            let mut swatches = row![].spacing(8).align_y(Alignment::Center);
            for &(name, light, dark) in &crate::style::ACCENTS {
                let color = crate::style::rgb(if t.dark { dark } else { light });
                let selected = current == name;
                let mark: Element<'_, Message> =
                    if selected { container(space()).width(6).height(6).style(Tokens::squircle(Color::WHITE, 3.0)).into() } else { space().into() };
                swatches = swatches.push(
                    button(container(mark).center(18))
                        .padding(0)
                        .width(18)
                        .height(18)
                        .style(t.swatch(color, selected))
                        .on_press(Message::Choose(key.into(), name)),
                );
            }
            swatches.into()
        }
        Kind::Zone => zone_picker(app, key),
        Kind::Sound => {
            let current = app.config.str(key);
            let sounds = &*crate::system::SOUNDS;
            let selected = sounds.iter().find(|s| s.0 == current).map(|(v, l)| Labelled(v, l));
            let pick = pick_list(sounds.iter().map(|(v, l)| Labelled(v, l)).collect::<Vec<_>>(), selected, |c: Labelled| Message::Sound(c.0, true));
            let play = selected.map(|s| Message::Sound(s.0, false));
            row![
                pick.placeholder(current).text_size(13).padding([3, 8]).style(t.pick()).menu_style(t.menu()),
                button(text("Play").size(12)).padding([3, 10]).style(t.push(false)).on_press_maybe(play),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into()
        }
        Kind::XkbOption(family, choices) => {
            let options = app.config.str(key);
            let current = crate::xkb::current(options, family);
            let selected = choices.iter().find(|c| c.0 == current).map(|&(v, l)| Labelled(v, l));
            let pick = pick_list(choices.iter().map(|&(v, l)| Labelled(v, l)).collect::<Vec<_>>(), selected, move |c: Labelled| {
                Message::Options(crate::xkb::with_option(options, family, c.0))
            });
            pick.placeholder(current).text_size(13).padding([3, 8]).style(t.pick()).menu_style(t.menu()).into()
        }
    }
}

/// A dropdown choice: the value written, the label shown.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Labelled(&'static str, &'static str);

impl std::fmt::Display for Labelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.1)
    }
}

/// The time-zone entry that writes empty: the system's zone, left alone.
const AUTOMATIC: &str = "Automatic (system)";

/// Date & Time ▸ Time zone: a field showing the zone; typing lists the zones that match, Return takes the first.
fn zone_picker<'a>(app: &'a App, key: &'static str) -> Element<'a, Message> {
    let t = app.tokens;
    let typed = app.texts.get(key).map_or("", String::as_str);
    let current = match app.config.str(key) {
        "" => AUTOMATIC,
        zone => zone,
    };
    let query = typed.trim().to_lowercase();
    let automatic = AUTOMATIC.to_lowercase().contains(&query).then_some(("", AUTOMATIC));
    // ponytail: the first 8 matches; a scrolling list if people want to browse.
    let matches: Vec<(&'static str, &'static str)> =
        if query.is_empty() { Vec::new() } else { automatic.into_iter().chain(crate::system::matching_zones(&query).map(|z| (z, z))).take(8).collect() };
    let field = text_input(current, typed)
        .on_input(move |v| Message::Type(key.into(), v))
        .on_submit_maybe(matches.first().map(|&(zone, _)| Message::Choose(key.into(), zone)))
        .size(13)
        .padding([3, 8])
        .style(t.input(false));
    let mut picker = column![field].spacing(4).width(220);
    if !query.is_empty() {
        let rows: Vec<Element<'a, Message>> = if matches.is_empty() {
            vec![container(text("No matching time zone").size(12).color(t.secondary())).padding([4, 8]).into()]
        } else {
            matches
                .into_iter()
                .map(|(zone, label)| {
                    button(text(label.replace('_', " ")).size(12))
                        .padding([3, 8])
                        .width(Length::Fill)
                        .style(t.sidebar_row(false))
                        .on_press(Message::Choose(key.into(), zone))
                        .into()
                })
                .collect()
        };
        picker = picker.push(container(column(rows)).padding(2).style(t.group()));
    }
    picker.into()
}

/// A segmented control writing `key`.
fn choice<'a>(t: Tokens, current: &str, options: &'static [(&'static str, &'static str)], key: &str) -> Element<'a, Message> {
    segments(t, options.iter().map(|&(value, label)| (label.to_owned(), current == value, Message::Choose(key.into(), value))).collect())
}

/// The `[notification_apps]` styles an allowed app can have (off is the Allow switch).
const APP_STYLES: &[(&str, &str)] = &[("banner", "Banners"), ("alert", "Alerts"), ("none", "None")];

/// One app's `[notification_apps]` rule, as the desktop reads it: a style alone, or a table with switches (missing
/// ones on).
#[derive(Clone, Debug, PartialEq)]
pub struct AppRule {
    pub style: String,
    pub sound: bool,
    pub badge: bool,
    pub center: bool,
}

impl AppRule {
    pub fn read(value: Option<&toml_edit::Value>) -> Self {
        let table = value.and_then(toml_edit::Value::as_inline_table);
        let style = value.and_then(toml_edit::Value::as_str).or_else(|| table?.get("style")?.as_str()).unwrap_or("banner");
        let switch = |key| table.and_then(|t| t.get(key)?.as_bool()).unwrap_or(true);
        AppRule { style: style.to_owned(), sound: switch("sound"), badge: switch("badge"), center: switch("center") }
    }

    /// As written: the style alone while every switch is on, else a table.
    pub fn value(&self) -> toml_edit::Value {
        if self.sound && self.badge && self.center {
            return self.style.as_str().into();
        }
        let mut table = toml_edit::InlineTable::new();
        table.insert("style", self.style.as_str().into());
        table.insert("sound", self.sound.into());
        table.insert("badge", self.badge.into());
        table.insert("center", self.center.into());
        table.into()
    }
}

/// One of an `AppRule`'s switches.
type Switch = fn(&mut AppRule) -> &mut bool;

/// Notifications ▸ Applications: each app that has notified (or has a rule), with Allow; opened, its style and
/// switches.
fn notifying_apps(app: &App) -> Vec<Element<'_, Message>> {
    let t = app.tokens;
    let mut apps = app.config.keys("notification_apps");
    apps.extend(app.notifiers.iter().cloned());
    apps.sort();
    apps.dedup();
    if apps.is_empty() {
        return vec![note(t, "Apps show here once they send a notification.")];
    }
    let mut rows = Vec::new();
    for name in apps {
        let rule = AppRule::read(app.config.get(&format!("notification_apps.{name}")));
        let allowed = rule.style != "off";
        let open = app.notify_app.as_ref() == Some(&name);
        let set = |change: &dyn Fn(&mut AppRule)| {
            let mut new = rule.clone();
            change(&mut new);
            Message::AppRule(name.clone(), new)
        };
        let allow = set(&|r| r.style = "off".into());
        let enable = set(&|r| r.style = "banner".into());
        let more = button(text(if open { "Done" } else { "Options…" }).size(12))
            .padding([3, 10])
            .style(t.push(false))
            .on_press(Message::NotifyApp((!open).then(|| name.clone())));
        let controls = row![toggle(t, allowed, move |on| if on { enable.clone() } else { allow.clone() }), more].spacing(8).align_y(Alignment::Center);
        rows.push(line(t, name.clone(), "", controls.into()));
        if !open || !allowed {
            continue;
        }
        let styles = APP_STYLES.iter().map(|&(value, label)| (label.to_owned(), rule.style == value, set(&|r| r.style = value.into()))).collect();
        rows.push(line(t, "Style", "Alerts stay until dismissed; None keeps them in Notification Center only", segments(t, styles)));
        let switches: [(&str, bool, Switch); 3] = [
            ("Play sound", rule.sound, |r| &mut r.sound),
            ("Badge app icon", rule.badge, |r| &mut r.badge),
            ("Show in Notification Center", rule.center, |r| &mut r.center),
        ];
        for (label, on, field) in switches {
            let (yes, no) = (set(&|r| *field(r) = true), set(&|r| *field(r) = false));
            rows.push(line(t, label, "", toggle(t, on, move |v| if v { yes.clone() } else { no.clone() })));
        }
    }
    vec![group(t, "Applications", rows)]
}

/// Sound ▸ Input's live level: 16 bars, lit up to `level` (0 … 1).
fn meter<'a>(t: Tokens, level: f32) -> Element<'a, Message> {
    let lit = (level * 16.0).round() as usize;
    let bars = (0..16).map(|i| container(space()).width(6).height(12).style(Tokens::squircle(if i < lit { t.accent } else { t.fill() }, 2.0)).into());
    iced::widget::Row::with_children(bars).spacing(3).width(206).into()
}

fn slider_row<'a>(t: Tokens, key: &'a str, range: std::ops::RangeInclusive<f64>, step: f64, value: f64) -> Element<'a, Message> {
    let bar = slider(range, value, move |v| Message::Slide(key.into(), v)).step(step).on_release(Message::Release(key.into())).width(150).style(t.slider());
    row![bar, text(shown(key, value)).size(12).color(t.secondary()).width(48).align_x(iced::alignment::Horizontal::Right)]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}

/// The table's settings for `page`, one card per group, in table order.
fn settings_of<'a>(app: &'a App, filter: impl Fn(&Setting) -> bool) -> Vec<(&'static str, Vec<Element<'a, Message>>)> {
    let mut groups: Vec<(&'static str, Vec<Element<'a, Message>>)> = Vec::new();
    for setting in SETTINGS.iter().filter(|s| filter(s)) {
        let row = line(app.tokens, setting.label, setting.help, control(app, setting));
        match groups.last_mut() {
            Some((group, rows)) if *group == setting.group => rows.push(row),
            _ => groups.push((setting.group, vec![row])),
        }
    }
    groups
}

pub fn page<'a>(app: &'a App, page: Page) -> Element<'a, Message> {
    let t = app.tokens;
    let mut parts: Vec<Element<'a, Message>> = vec![title(page.name())];
    parts.extend(system_part(app, page));
    for (header, rows) in settings_of(app, |s| s.page == page) {
        parts.push(group(t, header, rows));
    }
    parts.extend(after_part(app, page));
    column(parts).spacing(20).into()
}

/// Search (E7.1: finds any setting): matching settings with their controls, under their page's name.
pub fn search<'a>(app: &'a App, query: &str) -> Element<'a, Message> {
    let t = app.tokens;
    let query = query.trim().to_lowercase();
    let hit = |s: &Setting| [s.label, s.help, s.group, s.page.name(), s.key].iter().any(|w| w.to_lowercase().contains(&query));
    let mut parts: Vec<Element<'a, Message>> = vec![title("Search")];
    let pages: Vec<Page> = Page::ALL.into_iter().filter(|p| p.name().to_lowercase().contains(&query)).collect();
    if !pages.is_empty() {
        let rows = pages
            .into_iter()
            .map(|p| {
                button(row![squircle(p, 20.0), text(p.name()).size(13)].spacing(8).align_y(Alignment::Center))
                    .padding([6, 12])
                    .width(Length::Fill)
                    .style(t.sidebar_row(false))
                    .on_press(Message::Page(p))
                    .into()
            })
            .collect();
        parts.push(group(t, "Pages", rows));
    }
    let mut found = false;
    for page in Page::ALL {
        let rows: Vec<_> = settings_of(app, |s| s.page == page && hit(s)).into_iter().flat_map(|(_, rows)| rows).collect();
        if !rows.is_empty() {
            found = true;
            parts.push(group(t, page.name(), rows));
        }
    }
    if !found && parts.len() == 1 {
        parts.push(text(format!("No results for “{}”", query.trim())).size(13).color(t.secondary()).into());
    }
    column(parts).spacing(20).into()
}

/// The system services' rows, above the page's config settings.
fn system_part<'a>(app: &'a App, page: Page) -> Vec<Element<'a, Message>> {
    let t = app.tokens;
    let status = &app.status;
    let missing = |what: &'static str| vec![group(t, "", vec![fact(t, what, String::new())])];
    match page {
        Page::Wifi => match &status.wifi {
            Some(wifi) => network_part(app, wifi),
            None => missing("NetworkManager isn't running"),
        },
        Page::Bluetooth => match &status.bluetooth {
            Some(bt) => {
                let mut parts = vec![group(t, "", vec![line(t, "Bluetooth", "", toggle(t, bt.powered, Message::Bluetooth))])];
                if bt.powered {
                    let push = |label: &'static str, message: Message| -> Element<'a, Message> {
                        button(text(label).size(12)).padding([3, 10]).style(t.push(false)).on_press(message).into()
                    };
                    let (mine, nearby): (Vec<_>, Vec<_>) = bt.devices.iter().partition(|d| d.paired);
                    if !mine.is_empty() {
                        let rows = mine
                            .into_iter()
                            .map(|d| {
                                let (state, action) = if d.connected { ("Connected", "Disconnect") } else { ("Not connected", "Connect") };
                                let buttons =
                                    row![push(action, Message::Device(d.path.clone(), !d.connected)), push("Forget", Message::Forget(d.path.clone()))];
                                line(t, &d.name, state, buttons.spacing(8).into())
                            })
                            .collect();
                        parts.push(group(t, "My Devices", rows));
                    }
                    let rows: Vec<_> = nearby.into_iter().map(|d| line(t, &d.name, "", push("Connect", Message::Device(d.path.clone(), true)))).collect();
                    let rows = if rows.is_empty() { vec![fact(t, "Searching…", String::new())] } else { rows };
                    parts.push(group(t, "Nearby Devices", rows));
                }
                parts
            }
            None => missing("No Bluetooth adapter"),
        },
        Page::Sound => {
            let mut parts = Vec::new();
            for (output, header, device, choices) in [(true, "Output", &status.output, &status.outputs), (false, "Input", &status.input, &status.inputs)] {
                // More than one: pick the default, a check by the current one.
                if choices.len() > 1 {
                    let rows = choices
                        .iter()
                        .map(|c| {
                            let mark: Element<'_, Message> =
                                if c.current { svg(icon("check")).width(14).height(14).style(Tokens::tint(t.accent)).into() } else { space().width(14).into() };
                            let item = row![text(&c.name).size(13).width(Length::Fill), mark].spacing(12).align_y(Alignment::Center);
                            button(item).padding([8, 12]).width(Length::Fill).style(t.sidebar_row(false)).on_press(Message::DefaultDevice(c.id)).into()
                        })
                        .collect();
                    parts.push(group(t, if output { "Output device" } else { "Input device" }, rows));
                }
                let Some(device) = device else {
                    parts.push(group(t, header, vec![fact(t, if output { "No output device" } else { "No microphone" }, String::new())]));
                    continue;
                };
                let key = if output { "sound.output" } else { "sound.input" };
                let value = app.drafts.get(key).copied().unwrap_or(f64::from(device.volume.min(100)));
                let mut rows = vec![
                    fact(t, "Device", device.name.clone()),
                    line(t, if output { "Volume" } else { "Input level" }, "", slider_row(t, key, 0.0..=100.0, 1.0, value)),
                ];
                if let Some(balance) = status.balance.filter(|_| output) {
                    let value = app.drafts.get("sound.balance").copied().unwrap_or(f64::from(balance) * 100.0);
                    rows.push(line(t, "Balance", "Left … right", slider_row(t, "sound.balance", -100.0..=100.0, 5.0, value)));
                }
                if !output {
                    rows.push(line(t, "Level", "", meter(t, app.level)));
                }
                rows.push(line(t, "Mute", "", toggle(t, device.muted, move |m| Message::Mute(output, m))));
                parts.push(group(t, header, rows));
            }
            parts
        }
        Page::Displays => displays_part(app),
        Page::DateTime => match &status.time {
            Some(time) => {
                let help = if time.can_ntp { "Asks for your password" } else { "No time service is installed" };
                let ntp = toggler(time.ntp).on_toggle_maybe(time.can_ntp.then_some(Message::Ntp)).size(18).style(t.toggle());
                let mut rows = vec![line(t, "Set time and date automatically", help, ntp.into())];
                if !time.ntp {
                    // Shows the time the page was drawn until typed over; Set (or Enter) sends both.
                    let now = chrono::Local::now();
                    let field = |key: &'static str, now: String, width| {
                        let value = app.texts.get(key).cloned().unwrap_or(now);
                        text_input("", &value)
                            .on_input(move |v| Message::Type(key.into(), v))
                            .on_submit(Message::SetTime)
                            .size(13)
                            .padding([3, 8])
                            .width(width)
                            .style(t.input(false))
                    };
                    let date = field(crate::system::DATE, now.format("%Y-%m-%d").to_string(), 100);
                    let clock = field(crate::system::CLOCK, now.format("%H:%M").to_string(), 64);
                    let set = button(text("Set").size(12)).padding([3, 10]).style(t.push(true)).on_press(Message::SetTime);
                    rows.push(line(t, "Date and time", "Asks for your password", row![date, clock, set].spacing(8).align_y(Alignment::Center).into()));
                }
                rows.push(fact(t, "System time zone", time.zone.clone()));
                vec![group(t, "", rows)]
            }
            None => missing("systemd-timedated isn't available"),
        },
        Page::About => {
            let version = version();
            let head = column![
                svg(svg::Handle::from_memory(include_bytes!("../../assets/logo.svg").as_slice())).width(72).height(72).style(Tokens::tint(t.label())),
                text("Noobsbot Desktop").size(22).font(font(Weight::Semibold)),
                text(format!("Opal · Version {version}")).size(12).color(t.secondary()),
            ]
            .spacing(6)
            .align_x(Alignment::Center);
            let rows = crate::system::about().into_iter().map(|(label, value)| fact(t, label, value)).collect();
            vec![container(head).center_x(Length::Fill).into(), group(t, "", rows)]
        }
        Page::Battery => {
            let rows = match &status.battery {
                Some(b) => {
                    let mut rows = vec![fact(t, "Battery level", format!("{}%", b.percent)), fact(t, "Power source", power_source(b.state).into())];
                    if b.seconds > 0 {
                        rows.push(fact(t, if b.state == 1 { "Until full" } else { "Time remaining" }, duration(b.seconds)));
                    }
                    rows
                }
                None => vec![fact(t, "No battery", String::new())],
            };
            let mode = match &status.power_mode {
                Some(mode) => {
                    let items = mode.available.iter().map(|p| (profile_name(p).to_owned(), *p == mode.active, Message::PowerMode(p.clone()))).collect();
                    line(t, "Power mode", "Low Power saves energy; Performance runs faster and warmer", segments(t, items))
                }
                None => fact(t, "Power mode", "Needs power-profiles-daemon".into()),
            };
            let mut parts = vec![group(t, "", rows), group(t, "", vec![mode])];
            if let Some(level) = status.kbd_backlight {
                let value = app.drafts.get("keyboard.backlight").copied().unwrap_or(f64::from(level));
                parts.push(group(t, "", vec![line(t, "Keyboard brightness", "", slider_row(t, "keyboard.backlight", 0.0..=100.0, 1.0, value))]));
            }
            parts
        }
        Page::LockScreen => vec![note(t, "Videos and calls keep the display awake.")],
        Page::Appearance => vec![gallery(app)],
        _ => Vec::new(),
    }
}

/// UPower's state as the power source.
fn power_source(state: u32) -> &'static str {
    match state {
        1 => "Charging",
        2 | 6 => "Battery",
        4 => "Power adapter, fully charged",
        _ => "Power adapter",
    }
}

/// `11400` s → `3 h 10 min`.
fn duration(seconds: i64) -> String {
    let minutes = seconds / 60;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

fn profile_name(profile: &str) -> &str {
    match profile {
        "power-saver" => "Low Power",
        "balanced" => "Balanced",
        "performance" => "Performance",
        other => other,
    }
}

/// Wi-Fi (E8.2 Network): the switch, networks in range, Known Networks, and the details of the links that are up.
fn network_part<'a>(app: &'a App, wifi: &'a crate::system::Wifi) -> Vec<Element<'a, Message>> {
    let t = app.tokens;
    let net = &wifi.network;
    let network = wifi.connection.clone().unwrap_or_else(|| "Not connected".into());
    let mut parts = vec![group(t, "", vec![line(t, "Wi-Fi", "", toggle(t, wifi.enabled, Message::Wifi)), fact(t, "Network", network)])];
    let symbol = |name: &'static str| svg(icon(name)).width(14).height(14).style(Tokens::tint(t.secondary()));
    if wifi.enabled && !net.networks.is_empty() {
        let mut rows = Vec::new();
        for ap in &net.networks {
            let check: Element<'a, Message> = if ap.connected { symbol("check").into() } else { space().width(14).into() };
            let bars = match ap.strength {
                0..=33 => "wifi-low",
                34..=66 => "wifi-medium",
                _ => "wifi-high",
            };
            let mut item = row![check, text(&ap.ssid).size(13).width(Length::Fill)].spacing(8).align_y(Alignment::Center);
            if ap.secure {
                item = item.push(symbol("lock-simple"));
            }
            item = item.push(symbol(bars));
            let press = (!ap.connected).then(|| Message::Join(ap.ssid.clone()));
            rows.push(button(item).padding([6, 12]).width(Length::Fill).style(t.sidebar_row(false)).on_press_maybe(press).into());
            let key = format!("{}{}", crate::network::PASSWORD, ap.ssid);
            if let Some(password) = app.texts.get(&key) {
                let field = text_input("Password", password)
                    .secure(true)
                    .on_input({
                        let key = key.clone();
                        move |v| Message::Type(key.clone(), v)
                    })
                    .on_submit(Message::Submit(key.clone()))
                    .size(13)
                    .padding([3, 8])
                    .width(180)
                    .style(t.input(false));
                let join = button(text("Join").size(12)).padding([3, 10]).style(t.push(true)).on_press(Message::Submit(key));
                rows.push(line(t, "Password", "", row![field, join].spacing(8).align_y(Alignment::Center).into()));
            }
        }
        parts.push(group(t, "Networks", rows));
    }
    if wifi.enabled && net.wifi_device.is_some() {
        parts.push(other_network(app));
    }
    for (wifi, header) in [(true, "Known Networks"), (false, "Wired Connections")] {
        let rows: Vec<_> = net
            .known
            .iter()
            .filter(|k| k.wifi == wifi)
            .map(|k| {
                let details = button(text("Details…").size(12)).padding([3, 10]).style(t.push(false)).on_press(Message::NetDetails(Some(k.path.clone())));
                line(t, k.ssid.as_str(), "", details.into())
            })
            .collect();
        if !rows.is_empty() {
            parts.push(group(t, header, rows));
        }
    }
    let vpns = net.vpns.iter().map(|v| {
        let vpn = v.clone();
        line(t, v.name.as_str(), if v.active.is_some() { "Connected" } else { "" }, toggle(t, v.active.is_some(), move |_| Message::Vpn(vpn.clone())))
    });
    let vpns: Vec<_> = vpns.collect();
    if !vpns.is_empty() {
        parts.push(group(t, "VPN", vpns));
    }
    if let Some(k) = net.known.iter().find(|k| app.texts.get(&format!("{}path", crate::network::DETAILS)) == Some(&k.path)) {
        parts.push(connection_details(app, k));
    }
    for d in &net.details {
        let rows = [("IP address", &d.ip), ("Router", &d.router), ("DNS", &d.dns), ("MAC address", &d.mac)]
            .into_iter()
            .filter(|(_, value)| !value.is_empty())
            .map(|(label, value)| fact(t, label, value.clone()))
            .collect();
        parts.push(group(t, if d.wifi { "Wi-Fi Details" } else { "Ethernet Details" }, rows));
    }
    if net.editor {
        let advanced = button(text("Advanced…").size(12)).padding([3, 10]).style(t.push(false)).on_press(Message::Advanced);
        parts.push(container(advanced).align_right(Length::Fill).into());
    }
    if let Some(error) = &net.error {
        parts.push(note(t, error));
    }
    parts
}

/// A saved connection's Details sheet: Auto-Join, IPv4 (DHCP or manual) and DNS, Forget (fields in `App::texts`).
fn connection_details<'a>(app: &'a App, k: &'a crate::network::Known) -> Element<'a, Message> {
    let t = app.tokens;
    let push = |label: &'static str, primary: bool, message: Message| button(text(label).size(12)).padding([3, 10]).style(t.push(primary)).on_press(message);
    let key = |field: &str| format!("{}{field}", crate::network::DETAILS);
    let value = |field: &str| app.texts.get(&key(field)).map_or("", String::as_str);
    let field = |field: &'static str, placeholder: &'static str| -> Element<'a, Message> {
        text_input(placeholder, value(field)).on_input(move |v| Message::Type(key(field), v)).size(13).padding([3, 8]).width(180).style(t.input(false)).into()
    };
    let path = k.path.clone();
    let manual = value("method") == "manual";
    let method = segments(
        t,
        [("auto", "Automatic (DHCP)"), ("manual", "Manual")]
            .map(|(v, label)| (label.to_owned(), value("method") == v, Message::Type(key("method"), v.into())))
            .to_vec(),
    );
    let mut rows = vec![line(t, "Auto-Join", "", toggle(t, k.auto_join, move |on| Message::AutoJoin(path.clone(), on))), line(t, "Configure IPv4", "", method)];
    if manual {
        rows.push(line(t, "IP address", "", field("address", "192.168.1.20")));
        rows.push(line(t, "Prefix length", "24 = subnet mask 255.255.255.0", field("prefix", "24")));
        rows.push(line(t, "Router", "", field("gateway", "192.168.1.1")));
    }
    rows.push(line(t, "DNS servers", "Comma separated; empty = from the network", field("dns", "1.1.1.1, 9.9.9.9")));
    let forget = push(if k.wifi { "Forget This Network" } else { "Forget This Connection" }, false, Message::ForgetNetwork(k.path.clone()));
    let buttons =
        row![forget, space().width(Length::Fill), push("Cancel", false, Message::NetDetails(None)), push("Update", true, Message::UpdateIp4)].spacing(8);
    rows.push(container(buttons).padding([8, 12]).into());
    group(t, &k.ssid, rows)
}

/// Networks ▸ "Other…", or its form for a network that doesn't announce its name (fields in `App::texts`).
fn other_network(app: &App) -> Element<'_, Message> {
    let t = app.tokens;
    let push = |label: &'static str, primary: bool, message: Option<Message>| {
        button(text(label).size(12)).padding([3, 10]).style(t.push(primary)).on_press_maybe(message)
    };
    let key = |field: &str| format!("{}{field}", crate::network::OTHER);
    let Some(ssid) = app.texts.get(&key("ssid")) else {
        return container(push("Other…", false, Some(Message::OtherNetwork(true)))).align_right(Length::Fill).into();
    };
    let value = |field: &str| app.texts.get(&key(field)).map_or("", String::as_str);
    let field = |field: &'static str, placeholder: &'static str, secure: bool| -> Element<'_, Message> {
        text_input(placeholder, value(field))
            .secure(secure)
            .on_input(move |v| Message::Type(key(field), v))
            .size(13)
            .padding([3, 8])
            .width(220)
            .style(t.input(false))
            .into()
    };
    let pick = |field: &'static str, options: &[(&'static str, &'static str)]| {
        segments(t, options.iter().map(|&(v, label)| (label.to_owned(), value(field) == v, Message::Type(key(field), v.into()))).collect())
    };
    let security = value("security");
    let mut rows = vec![
        line(t, "Network name", "", field("ssid", "Name", false)),
        line(t, "Security", "", pick("security", &[("none", "None"), ("psk", "WPA2/WPA3 Personal"), ("eap", "WPA2 Enterprise")])),
    ];
    if security == "eap" {
        rows.push(line(t, "EAP method", "", pick("eap", &[("peap", "PEAP"), ("ttls", "TTLS")])));
        rows.push(line(t, "Inner authentication", "", pick("inner", &[("mschapv2", "MSCHAPv2"), ("pap", "PAP")])));
        rows.push(line(t, "Username", "", field("identity", "Identity", false)));
    }
    if security != "none" {
        rows.push(line(t, "Password", "", field("password", "Password", true)));
    }
    if security == "eap" {
        rows.push(line(t, "CA certificate", "Optional; without one the server isn't checked", field("ca", "/path/to/ca.pem", false)));
    }
    let ready = !ssid.trim().is_empty() && (security == "none" || !value("password").is_empty());
    let buttons = row![push("Cancel", false, Some(Message::OtherNetwork(false))), push("Join", true, ready.then_some(Message::JoinOther))].spacing(8);
    rows.push(container(buttons).align_right(Length::Fill).padding([8, 12]).into());
    group(t, "Join Other Network", rows)
}

/// Appearance ▸ Wallpaper: the built-in opal, then the pictures found (`gallery`), as tiles that wrap.
fn gallery(app: &App) -> Element<'_, Message> {
    let t = app.tokens;
    let current = app.config.str(if t.dark { "appearance.wallpaper_dark" } else { "appearance.wallpaper" });
    let (w, h) = (crate::gallery::W as f32 / 2.0, crate::gallery::H as f32 / 2.0);
    let tile = |content: Element<'static, Message>, path: String| -> Element<'static, Message> {
        let selected = current == path;
        button(content).padding(3).style(t.tile(selected)).on_press(Message::Wallpaper(path)).into()
    };
    // The opal's own blobs (render::opal), as a gradient.
    let blobs = if t.dark { [0x344E80, 0x683454, 0x285C48] } else { [0xB0CCF4, 0xF6C4DA, 0xC2E8D2] };
    let gradient = iced::gradient::Linear::new(iced::Radians(0.6)).add_stop(0.0, rgb(blobs[0])).add_stop(0.5, rgb(blobs[1])).add_stop(1.0, rgb(blobs[2]));
    let opal = container(space()).width(w).height(h).style(move |_| container::Style {
        background: Some(gradient.into()),
        border: iced::Border { radius: 5.0.into(), ..Default::default() },
        ..Default::default()
    });
    let mut tiles = vec![tile(opal.into(), String::new())];
    for (path, handle) in &app.thumbs {
        let picture = iced::widget::image(handle.clone()).width(w).height(h).border_radius(5.0);
        tiles.push(tile(picture.into(), path.to_string_lossy().into_owned()));
    }
    let grid = row(tiles).spacing(8).wrap().vertical_spacing(8);
    group(t, "Wallpaper", vec![container(grid).padding(12).width(Length::Fill).into()])
}

/// Rows below the config settings.
fn after_part<'a>(app: &'a App, page: Page) -> Vec<Element<'a, Message>> {
    let t = app.tokens;
    match page {
        Page::Keyboard => {
            let open =
                button(text("Keyboard Shortcuts…").size(13)).padding([3, 10]).style(t.push(false)).on_press(Message::Shortcuts(crate::shortcuts::Msg::Open));
            let mut parts = input_sources(app);
            parts.push(group(t, "Shortcuts", vec![line(t, "Keyboard shortcuts", "Change, turn off or add the desktop's keys", open.into())]));
            parts
        }
        Page::Notifications => notifying_apps(app),
        Page::Dock => vec![default_apps(app)],
        Page::General => {
            let clear = button(text("Clear").size(12)).padding([3, 10]).style(t.push(false)).on_press(Message::ClearRecent);
            vec![
                login_items(app),
                group(t, "Privacy", vec![line(t, "Recent files", "Every app's recent documents list", clear.into())]),
                group(t, "", vec![fact(t, "Noobsbot Desktop", format!("Version {}", version()))]),
            ]
        }
        _ => Vec::new(),
    }
}

/// Desktop & Dock ▸ Default Apps: a picker of the apps that open each kind (`mimeapps.list`), and the terminal.
fn default_apps(app: &App) -> Element<'_, Message> {
    let t = app.tokens;
    let pick = |apps: Vec<crate::autostart::Installed>, selected, placeholder: String, message: Box<dyn Fn(crate::autostart::Installed) -> Message>| {
        pick_list(apps, selected, message).placeholder(placeholder).text_size(13).padding([3, 8]).style(t.pick()).menu_style(t.menu())
    };
    let mut rows: Vec<Element<'_, Message>> = crate::defaults::KINDS
        .iter()
        .zip(&app.default_apps)
        .enumerate()
        .map(|(i, ((label, _), (current, apps)))| {
            let selected = apps.iter().find(|a| Some(crate::defaults::id(a)) == *current).cloned();
            let placeholder = current.as_deref().map_or("None", |id| id.trim_end_matches(".desktop")).to_owned();
            line(t, *label, "", pick(apps.clone(), selected, placeholder, Box::new(move |a| Message::DefaultApp(i, a))).into())
        })
        .collect();
    let terminal = app.config.str("terminal");
    // `noobsbot` is also the entry's `/usr/bin/noobsbot`.
    let selected = app.terminals.iter().find(|(_, c)| c == terminal || c.rsplit_once('/').is_some_and(|(_, p)| p == terminal)).map(|(a, _)| a.clone());
    let apps = app.terminals.iter().map(|(a, _)| a.clone()).collect();
    rows.push(line(t, "Terminal", "Opened by Super+Return", pick(apps, selected, terminal.to_owned(), Box::new(Message::Terminal)).into()));
    group(t, "Default Apps", rows)
}

/// General ▸ Login Items (`autostart`): a toggle each, − for the user's own, + from the installed apps.
fn login_items(app: &App) -> Element<'_, Message> {
    let t = app.tokens;
    let mut rows: Vec<Element<'_, Message>> = app
        .login_items
        .iter()
        .map(|item| {
            let file = item.file.clone();
            let mut controls = row![toggle(t, item.on, move |on| Message::LoginItem(file.clone(), on))].spacing(8).align_y(Alignment::Center);
            if item.removable {
                controls = controls.push(button(text("−").size(13)).padding([0, 8]).style(t.push(false)).on_press(Message::RemoveLoginItem(item.file.clone())));
            }
            line(t, item.name.as_str(), "", controls.into())
        })
        .collect();
    let add = pick_list(app.installed.as_slice(), None::<crate::autostart::Installed>, Message::AddLoginItem)
        .placeholder("+ Add an app…")
        .text_size(13)
        .padding([3, 8])
        .style(t.pick())
        .menu_style(t.menu());
    rows.push(line(t, "Open at login", "Off keeps the entry, hidden", add.into()));
    group(t, "Login Items", rows)
}

/// Keyboard ▸ Input Sources: the chosen layouts in switching order (Super+Ctrl+Space), with + to pick more.
fn input_sources(app: &App) -> Vec<Element<'_, Message>> {
    let t = app.tokens;
    let sources = crate::xkb::chosen(app.config.str("keyboard.layout"), app.config.str("keyboard.variant"));
    let small = |label: &'static str, message: Option<Message>| {
        button(text(label).size(12).center()).padding([2, 8]).height(22).style(t.segment(false)).on_press_maybe(message)
    };
    let mut rows: Vec<Element<'_, Message>> = Vec::new();
    for (i, (layout, variant)) in sources.iter().enumerate() {
        let moved = |to: usize| {
            let mut list = sources.clone();
            list.swap(i, to);
            Message::Sources(list)
        };
        let mut rest = sources.clone();
        rest.remove(i);
        let buttons = row![
            small("↑", (i > 0).then(|| moved(i - 1))),
            small("↓", (i + 1 < sources.len()).then(|| moved(i + 1))),
            small("−", Some(Message::Sources(rest))),
        ]
        .spacing(2);
        let code = if variant.is_empty() { layout.clone() } else { format!("{layout} ({variant})") };
        rows.push(line(
            t,
            crate::xkb::name(layout, variant),
            "",
            row![text(code).size(12).color(t.secondary()), container(buttons).padding(2).style(t.field())].spacing(8).align_y(Alignment::Center).into(),
        ));
    }
    if rows.is_empty() {
        rows.push(fact(t, "The system's layout", String::new()));
    }
    let add = if app.picker.is_some() { small("Done", Some(Message::Picker(None))) } else { small("+", Some(Message::Picker(Some(String::new())))) };
    rows.push(line(t, "Add an input source", "", add.into()));
    let mut parts = vec![group(t, "Input Sources", rows)];
    if let Some(query) = &app.picker {
        let field = text_input("Search layouts", query).on_input(|q| Message::Picker(Some(q))).size(13).padding([4, 8]).style(t.input(false));
        let query = query.trim().to_lowercase();
        let mut picks: Vec<Element<'_, Message>> = vec![container(field).padding([8, 12]).into()];
        // ponytail: the first 40 matches; typing narrows it.
        for source in crate::xkb::ALL
            .iter()
            .filter(|s| !sources.contains(&(s.layout.clone(), s.variant.clone())))
            .filter(|s| query.is_empty() || s.name.to_lowercase().contains(&query) || s.layout == query)
            .take(40)
        {
            let mut list = sources.clone();
            list.push((source.layout.clone(), source.variant.clone()));
            picks.push(
                button(text(&source.name).size(13)).padding([6, 12]).width(Length::Fill).style(t.sidebar_row(false)).on_press(Message::Sources(list)).into(),
            );
        }
        parts.push(group(t, "", picks));
    }
    parts
}

/// `eDP-1` → `Built-in display`, `winit` → `Nested window`, `HDMI-A-1` → `HDMI-A-1`.
pub fn display_name(connector: &str) -> String {
    match connector {
        _ if crate::display_info::built_in(connector) => "Built-in display".into(),
        "winit" => "Nested window".into(),
        _ => connector.to_owned(),
    }
}

/// A resolution in a dropdown.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Resolution(i32, i32);

impl std::fmt::Display for Resolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} × {}", self.0, self.1)
    }
}

/// A refresh rate (mHz) in a dropdown.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rate(i32);

impl std::fmt::Display for Rate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.2} Hz", f64::from(self.0) / 1000.0)
    }
}

/// The scale presets (the common desktop steps).
const SCALES: [f64; 5] = [1.0, 1.25, 1.5, 1.75, 2.0];

/// `1.25` → `125%`, `1.333` → `133%`.
fn percent(scale: f64) -> String {
    format!("{:.0}%", scale * 100.0)
}

/// A typed custom scale: `133%`, `133` or `1.33` → 1.33; `None` outside 100 … 300%.
pub fn parse_scale(typed: &str) -> Option<f64> {
    let typed = typed.trim().trim_end_matches('%').trim();
    let value: f64 = typed.parse().ok()?;
    let scale = if value >= 10.0 { value / 100.0 } else { value };
    (1.0..=3.0).contains(&scale).then(|| (scale * 1000.0).round() / 1000.0)
}

/// A segmented control (D8) of `(label, selected, message)`.
fn segments<'a>(t: Tokens, items: Vec<(String, bool, Message)>) -> Element<'a, Message> {
    let mut row = row![].spacing(2);
    for (label, selected, message) in items {
        row = row.push(button(text(label).size(12).center()).padding([2, 8]).height(22).style(t.segment(selected)).on_press(message));
    }
    container(row).padding(2).style(t.field()).into()
}

/// The Displays page (E6.2): the arrangement, then the selected display's settings, from `displays.json`.
fn displays_part<'a>(app: &'a App) -> Vec<Element<'a, Message>> {
    let t = app.tokens;
    if app.displays.is_empty() {
        // The desktop isn't running (or is older): what the kernel says is connected.
        let rows: Vec<_> = crate::system::displays().into_iter().map(|(name, mode)| fact(t, display_name(&name), mode)).collect();
        let rows = if rows.is_empty() { vec![fact(t, "No display information", String::new())] } else { rows };
        return vec![group(t, "Connected displays", rows), note(t, "Arrangement, resolution and scale show while Noobsbot Desktop is running.")];
    }
    let selected = app.selected_display();
    let mut parts = Vec::new();
    let on: Vec<&Display> = app.displays.iter().filter(|d| d.enabled).collect();
    let arrangement = Arrangement { displays: on, selected: selected.map(|d| d.name.as_str()), tokens: t };
    parts.push(group(t, "", vec![container(canvas(arrangement).width(Length::Fill).height(200)).padding(4).into()]));
    parts.push(note(
        t,
        if app.displays.len() > 1 {
            "Drag the displays to arrange them; the white bar marks the main display (the menu bar and the Dock)."
        } else {
            "The white bar marks the main display: it has the menu bar and the Dock."
        },
    ));
    let identify = button(text("Identify").size(12)).padding([3, 10]).style(t.push(false)).on_press(Message::Identify);
    let reset =
        (app.displays.len() > 1).then(|| button(text("Reset Arrangement").size(12)).padding([3, 10]).style(t.push(false)).on_press(Message::ResetArrangement));
    parts.push(row![space().width(Length::Fill)].push(reset).push(identify).spacing(8).into());
    // Displays that are off aren't in the arrangement: pick any one here.
    if app.displays.len() > 1 {
        let items =
            app.displays.iter().map(|d| (display_name(&d.name), selected.is_some_and(|s| s.name == d.name), Message::SelectDisplay(d.name.clone()))).collect();
        parts.push(container(segments(t, items)).center_x(Length::Fill).into());
    }
    let Some(d) = selected else { return parts };
    let name: &'a str = &d.name;
    let change = move |changes: Vec<(&'static str, toml_edit::Value)>| Message::Output(name.to_owned(), changes);
    let mut rows = Vec::new();
    let maker = [d.make.as_str(), d.model.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
    let size = (d.width_mm > 0).then(|| {
        let inches = f64::from(d.width_mm).hypot(f64::from(d.height_mm)) / 25.4;
        format!("{inches:.1}-inch, {} × {} mm", d.width_mm, d.height_mm)
    });
    let about = [Some(maker).filter(|m| !m.is_empty()), size].into_iter().flatten().collect::<Vec<_>>().join(" · ");
    if !about.is_empty() {
        rows.push(fact(t, "Display", about));
    }
    if let Some(level) = app.brightness.filter(|_| d.built_in) {
        let value = app.drafts.get("display.brightness").copied().unwrap_or(f64::from(level));
        rows.push(line(t, "Brightness", "", slider_row(t, "display.brightness", 1.0..=100.0, 1.0, value)));
    }
    if !d.nested {
        rows.push(line(t, "Use this display", "", toggle(t, d.enabled, move |on| change(vec![("enabled", on.into())]))));
    }
    if d.enabled && app.displays.len() > 1 {
        let control: Element<'a, Message> = if d.primary {
            text("Main display").size(13).color(t.secondary()).into()
        } else {
            button(text("Set as Primary").size(12)).padding([3, 10]).style(t.push(false)).on_press(Message::Primary(name.to_owned())).into()
        };
        rows.push(line(t, "Menu bar and Dock", "", control));
    }
    if d.nested {
        rows.push(fact(t, "Resolution", format!("{} (the window's size)", Resolution(d.mode.width, d.mode.height))));
    } else {
        let mut sizes: Vec<Resolution> = Vec::new();
        for m in &d.modes {
            if !sizes.contains(&Resolution(m.width, m.height)) {
                sizes.push(Resolution(m.width, m.height));
            }
        }
        sizes.sort_by_key(|r| std::cmp::Reverse(r.0 * r.1));
        let preferred = d.modes.iter().find(|m| m.preferred).map(|m| (m.width, m.height));
        let help = if preferred == Some((d.mode.width, d.mode.height)) { "Recommended" } else { "" };
        let pick = pick_list(sizes, Some(Resolution(d.mode.width, d.mode.height)), move |r: Resolution| {
            change(vec![("mode", format!("{}x{}", r.0, r.1).into()), ("refresh", 0.0.into())])
        });
        rows.push(line(t, "Resolution", help, pick.text_size(13).padding([3, 8]).style(t.pick()).menu_style(t.menu()).into()));
        let mut rates: Vec<Rate> = d.modes.iter().filter(|m| (m.width, m.height) == (d.mode.width, d.mode.height)).map(|m| Rate(m.refresh)).collect();
        rates.sort_by_key(|r| std::cmp::Reverse(r.0));
        rates.dedup();
        let pick = pick_list(rates, Some(Rate(d.mode.refresh)), move |r: Rate| change(vec![("refresh", ((f64::from(r.0) / 10.0).round() / 100.0).into())]));
        rows.push(line(t, "Refresh rate", "", pick.text_size(13).padding([3, 8]).style(t.pick()).menu_style(t.menu()).into()));
    }
    // The entry's own scale; 0 or none = the default ([display] scale, else auto).
    let own = app.config.output(&d.name).and_then(|e| e.get("scale")).and_then(|v| v.as_float().or(v.as_integer().map(|i| i as f64))).unwrap_or(0.0);
    let global = app.config.num("display.scale");
    let default = if global > 0.0 { global } else { d.auto_scale };
    let mut items = vec![("Default".to_string(), own <= 0.0, change(vec![("scale", 0.0.into())]))];
    items.extend(SCALES.iter().map(|&s| (percent(s), own == s, change(vec![("scale", s.into())]))));
    let custom_key = format!("display.custom_scale.{}", d.name);
    let typed = app.texts.get(&custom_key).cloned().unwrap_or_else(|| if own > 0.0 && !SCALES.contains(&own) { percent(own) } else { String::new() });
    let custom = text_input("Custom", &typed)
        .on_input({
            let key = custom_key.clone();
            move |v| Message::Type(key.clone(), v)
        })
        .on_submit(Message::Submit(custom_key))
        .size(12)
        .padding([3, 6])
        .width(64)
        .style(t.input(false));
    let help = format!("Default: {} ({})", percent(default), if global > 0.0 { "every display" } else { "from the pixel density" });
    let scale_row = row![segments(t, items), custom].spacing(8).align_y(Alignment::Center);
    rows.push(
        column![
            line(t, "Scale", "", space().into()),
            container(scale_row).padding(iced::Padding { top: 0.0, right: 12.0, bottom: 4.0, left: 12.0 }),
            container(text(help).size(11).color(t.secondary())).padding(iced::Padding { top: 0.0, right: 12.0, bottom: 8.0, left: 12.0 })
        ]
        .into(),
    );
    let rotations = [(0, "Normal"), (90, "90°"), (180, "180°"), (270, "270°")];
    let items = rotations.iter().map(|&(deg, label)| (label.to_string(), d.rotation == deg, change(vec![("rotation", i64::from(deg).into())]))).collect();
    rows.push(line(t, "Rotation", "", segments(t, items)));
    parts.push(group(t, if d.nested { "Nested window" } else { "Display settings" }, rows));
    if d.nested {
        parts.push(note(t, "Nested in another desktop, only scale and rotation apply; the window's size is the resolution."));
    }
    parts
}

/// "Keep these display settings?" over the page, counting down to the revert (a safety prompt).
pub fn keep_sheet<'a>(t: Tokens, left: u64) -> Element<'a, Message> {
    let card = column![
        text("Keep these display settings?").size(15).font(font(Weight::Semibold)),
        text(format!("The previous settings come back in {left} second{}.", if left == 1 { "" } else { "s" })).size(12).color(t.secondary()),
        row![
            space().width(Length::Fill),
            button(text("Revert").size(13)).padding([4, 14]).style(t.push(false)).on_press(Message::Revert),
            button(text("Keep Changes").size(13)).padding([4, 14]).style(t.push(true)).on_press(Message::Keep),
        ]
        .spacing(8),
    ]
    .spacing(10)
    .width(340);
    let sheet = container(card).padding(18).style(t.sheet());
    iced::widget::opaque(container(sheet).center(Length::Fill).style(t.dim()))
}

/// The desktop's version: the root package's, which release-plz bumps (this crate's own stays 0.1.0).
fn version() -> &'static str {
    let manifest = include_str!("../../Cargo.toml");
    manifest.lines().find_map(|l| l.strip_prefix("version = ")).map_or("", |v| v.trim_matches('"'))
}

fn note<'a>(t: Tokens, words: &'a str) -> Element<'a, Message> {
    container(text(words).size(11).color(t.secondary())).padding([0, 4]).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_rules_read_and_write_both_forms() {
        let doc: toml_edit::DocumentMut = "a = \"alert\"\nb = { style = \"none\", badge = false }\n".parse().unwrap();
        let a = AppRule::read(doc.get("a").and_then(toml_edit::Item::as_value));
        assert_eq!(a, AppRule { style: "alert".into(), sound: true, badge: true, center: true });
        assert_eq!(a.value().as_str(), Some("alert"), "every switch on: the style alone");
        let b = AppRule::read(doc.get("b").and_then(toml_edit::Item::as_value));
        assert_eq!(b, AppRule { style: "none".into(), sound: true, badge: false, center: true });
        assert_eq!(AppRule::read(Some(&b.value())), b);
        assert_eq!(AppRule::read(None).style, "banner");
    }

    #[test]
    fn every_setting_is_in_the_shipped_config() {
        let (config, _) = crate::config::Config::load(std::env::temp_dir().join("noobsbot-settings-none/config.toml"));
        for setting in SETTINGS {
            assert!(config.get(setting.key).is_some(), "{} is not in data/config.toml", setting.key);
        }
    }

    #[test]
    fn page_names_parse() {
        assert_eq!(Page::parse("desktop-dock"), Some(Page::Dock));
        assert_eq!(Page::parse("date-time"), Some(Page::DateTime));
        assert_eq!(Page::parse("wi-fi"), Some(Page::Wifi));
        assert_eq!(Page::parse("battery"), Some(Page::Battery));
        assert_eq!(Page::parse("nope"), None);
    }

    #[test]
    fn desktop_version() {
        assert!(semver_like(version()), "{}", version());
    }

    fn semver_like(v: &str) -> bool {
        v.split('.').count() == 3 && v.split('.').all(|p| p.chars().next().is_some_and(|c| c.is_ascii_digit()))
    }

    #[test]
    fn custom_scales() {
        assert_eq!(parse_scale("133%"), Some(1.33));
        assert_eq!(parse_scale(" 133 "), Some(1.33));
        assert_eq!(parse_scale("1.333"), Some(1.333));
        assert_eq!(parse_scale("2"), Some(2.0));
        assert_eq!(parse_scale("90%"), None);
        assert_eq!(parse_scale("400"), None);
        assert_eq!(parse_scale("big"), None);
        assert_eq!(percent(1.25), "125%");
        assert_eq!(display_name("eDP-1"), "Built-in display");
        assert_eq!(display_name("HDMI-A-1"), "HDMI-A-1");
    }

    #[test]
    fn battery_words() {
        assert_eq!(duration(11_400), "3 h 10 min");
        assert_eq!(duration(7_200), "2 h");
        assert_eq!(duration(300), "5 min");
        assert_eq!(power_source(2), "Battery");
        assert_eq!(profile_name("power-saver"), "Low Power");
    }

    #[test]
    fn spaces_skip_one() {
        assert_eq!(whole("windows.spaces", 1.0), 0);
        assert_eq!(whole("windows.spaces", 4.2), 4);
        assert_eq!(whole("dock.icon_size", 1.0), 1);
        assert_eq!(shown("windows.spaces", 0.0), "Dynamic");
        assert_eq!(shown("appearance.glass_opacity", 0.85), "85%");
        assert_eq!((shown("appearance.pointer_size", 0.0), shown("appearance.pointer_size", 32.0)), ("System".into(), "32 px".into()));
    }
}
