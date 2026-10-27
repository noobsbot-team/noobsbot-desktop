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
    Network,
    Bluetooth,
    Sound,
    Displays,
    Appearance,
    Accessibility,
    Dock,
    Keyboard,
    Mouse,
    Trackpad,
    Printers,
    DateTime,
    Spaces,
    Notifications,
    Battery,
    LockScreen,
    Privacy,
    General,
    Users,
    About,
}

impl Page {
    pub const ALL: [Page; 20] = [
        Page::Network,
        Page::Bluetooth,
        Page::Sound,
        Page::Displays,
        Page::Appearance,
        Page::Accessibility,
        Page::Dock,
        Page::Keyboard,
        Page::Mouse,
        Page::Trackpad,
        Page::Printers,
        Page::DateTime,
        Page::Spaces,
        Page::Notifications,
        Page::Battery,
        Page::LockScreen,
        Page::Privacy,
        Page::General,
        Page::Users,
        Page::About,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Page::Network => "Network",
            Page::Bluetooth => "Bluetooth",
            Page::Sound => "Sound",
            Page::Displays => "Displays",
            Page::Appearance => "Appearance",
            Page::Accessibility => "Accessibility",
            Page::Dock => "Desktop & Dock",
            Page::Keyboard => "Keyboard",
            Page::Mouse => "Mouse",
            Page::Trackpad => "Trackpad",
            Page::Printers => "Printers",
            Page::DateTime => "Date & Time",
            Page::Spaces => "Spaces",
            Page::Notifications => "Notifications",
            Page::Battery => "Battery",
            Page::LockScreen => "Lock Screen",
            Page::Privacy => "Privacy & Security",
            Page::General => "General",
            Page::Users => "Users",
            Page::About => "About",
        }
    }

    /// `--page appearance`, `--page date-time`, …: the page's name in lowercase, spaces as dashes (`wi-fi` and
    /// `wifi`, the Network page's old slugs, still open it).
    pub fn parse(name: &str) -> Option<Page> {
        if matches!(name, "wi-fi" | "wifi") {
            return Some(Page::Network);
        }
        let slug = |p: Page| p.name().to_lowercase().replace(" & ", "-").replace(' ', "-");
        Page::ALL.into_iter().find(|p| slug(*p) == name)
    }

    /// Pages that read the system services when opened.
    pub fn is_system(self) -> bool {
        matches!(self, Page::Network | Page::Bluetooth | Page::Sound | Page::DateTime | Page::Battery | Page::Privacy)
    }

    /// A little space starts a new block in the sidebar.
    pub fn gap_before(self) -> bool {
        matches!(self, Page::Appearance | Page::Notifications | Page::General)
    }

    /// Phosphor fill symbol and squircle colour.
    fn icon(self) -> (&'static str, u32) {
        match self {
            Page::Network => ("wifi-high", 0x007AFF),
            Page::Bluetooth => ("bluetooth", 0x007AFF),
            Page::Sound => ("speaker-high", 0xFF2D55),
            Page::Displays => ("monitor", 0x0A84FF),
            Page::Appearance => ("circle-half", 0x3A3A3C),
            Page::Accessibility => ("user-circle", 0x007AFF),
            Page::Dock => ("app-window", 0x3A3A3C),
            Page::Keyboard => ("keyboard", 0x8E8E93),
            Page::Mouse => ("mouse-simple", 0x8E8E93),
            Page::Trackpad => ("cursor-click", 0x8E8E93),
            // No printer among the symbols: the nearest, a plugged-in device.
            Page::Printers => ("plugs-connected", 0x8E8E93),
            Page::DateTime => ("clock", 0x007AFF),
            Page::Spaces => ("squares-four", 0x5856D6),
            Page::Notifications => ("bell", 0xFF3B30),
            Page::Battery => ("battery-high", 0x34C759),
            Page::LockScreen => ("lock-simple", 0x3A3A3C),
            Page::Privacy => ("lock-simple", 0x007AFF),
            Page::General => ("gear-six", 0x8E8E93),
            Page::Users => ("user-circle", 0xFF9500),
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
    "plugs-connected",
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
    /// `display.gpu`: Automatic, or one of `system::GPUS`.
    Gpu,
    /// Degrees typed in a field, checked to lie within ± this.
    Degrees(f64),
    /// A switch for a two-word key: the value written off, then on.
    Flag(&'static str, &'static str),
    /// A list of paths, a row each with Remove; with an Add… button that opens the file chooser for this `texts` key.
    List(Option<&'static str>),
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

/// Date & Time ▸ Calendars ▸ Add…: the chooser's, or the path field's, `texts` key.
pub const CALENDAR_ADD: &str = "calendar.add";
/// Notifications ▸ World Clock ▸ Add…: the typed city's `texts` key (no chooser).
pub const CLOCK_ADD: &str = "widgets.clock_add";
/// Date & Time ▸ CalDAV accounts ▸ Add…: the server (or calendar URL), user and password fields' `texts` keys.
pub const CALDAV_ADD: &str = "calendar.caldav_url";
pub const CALDAV: [&str; 3] = [CALDAV_ADD, "calendar.caldav_user", "calendar.caldav_password"];

/// A `[calendar] caldav` entry's URL and user.
pub fn account(entry: &toml_edit::Value) -> Option<(String, String)> {
    let table = entry.as_inline_table()?;
    let field = |key| table.get(key).and_then(toml_edit::Value::as_str).map(str::to_owned);
    Some((field("url")?, field("user")?))
}

/// Every setting in `config.toml` (`data/config.toml`), in page order.
pub const SETTINGS: &[Setting] = &[
    s(Page::Sound, "Sound Effects", "sound.alert", "Alert sound", "Played when an app rings the bell", Kind::Sound),
    s(Page::Sound, "Sound Effects", "sound.alert_volume", "Alert volume", "", Kind::Slider { min: 0.0, max: 100.0, step: 5.0 }),
    s(Page::Sound, "Sound Effects", "sound.ui_sounds", "Play user interface sound effects", "Screenshots, emptying the Trash, volume keys", Kind::Toggle),
    s(Page::Sound, "Sound Effects", "sound.startup_sound", "Play sound on startup", "Once each time you log in", Kind::Toggle),
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
        Kind::Choice(&[("fill", "Fill"), ("fit", "Fit"), ("centre", "Centre"), ("stretch", "Stretch"), ("tile", "Tile")]),
    ),
    s(Page::Appearance, "Wallpaper", "appearance.gallery_folders", "Gallery folders", "Added with + Add Folder…; separate with :", Kind::Text("None")),
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
    s(
        Page::Accessibility,
        "Zoom",
        "accessibility.zoom_follows_focus",
        "Follow keyboard focus",
        "Zoom follows the text cursor and the focused window, not only the pointer",
        Kind::Toggle,
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
    s(Page::Displays, "Graphics", "display.gpu", "Draw the desktop with", "Applies at the next login", Kind::Gpu),
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
    s(Page::Dock, "Dock", "dock.style", "Style", "", Kind::Choice(&[("default", "Default"), ("whitesur-dark", "Dark"), ("glass", "Glass")])),
    s(
        Page::Dock,
        "Dock",
        "dock.hide",
        "Hide the Dock",
        "Touch the bottom edge to show a hidden Dock",
        Kind::Menu(&[
            ("intelligent", "While the focused app's window overlaps it"),
            ("dodge-active", "While the focused window overlaps it"),
            ("window-dodge", "While any window overlaps it"),
            ("dodge-maximized", "While the focused window is maximised"),
            ("auto", "Unless the pointer is on it"),
            ("never", "Never (windows keep clear of it)"),
        ]),
    ),
    s(Page::Dock, "Dock", "dock.hide_delay", "Wait before hiding", "", Kind::Slider { min: 0.0, max: 2000.0, step: 50.0 }),
    s(Page::Dock, "Dock", "dock.unhide_delay", "Wait before showing", "When the pointer calls it up", Kind::Slider { min: 0.0, max: 2000.0, step: 50.0 }),
    s(Page::Dock, "Dock", "dock.minimise_into_icon", "Minimise windows into application icon", "", Kind::Toggle),
    s(Page::Dock, "Dock", "dock.minimise_effect", "Minimise windows using", "", Kind::Choice(&[("scale", "Scale Effect"), ("genie", "Genie Effect")])),
    s(Page::Dock, "Dock", "dock.show_trash", "Show Trash in Dock", "", Kind::Toggle),
    s(
        Page::Dock,
        "Dock",
        "dock.displays",
        "Show the Dock on",
        "",
        Kind::Choice(&[("all", "All Displays"), ("main", "Main Display"), ("pointer", "Display with the Pointer")]),
    ),
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
    s(
        Page::Dock,
        "Dock",
        "dock.cycle_across_apps",
        "Cycle on into the next app",
        "After an app's last window, clicking or scrolling goes to the next app",
        Kind::Toggle,
    ),
    s(Page::Dock, "Dock", "dock.tooltips", "Show app names", "Over the icon under the pointer", Kind::Toggle),
    s(Page::Dock, "Dock", "dock.pinned_only", "Show only kept apps", "Running apps not kept in the Dock stay out", Kind::Toggle),
    s(
        Page::Dock,
        "Dock",
        "dock.current_workspace_only",
        "Show only this Space's apps",
        "Running apps and minimised windows of the current Space",
        Kind::Toggle,
    ),
    s(Page::Dock, "Dock", "dock.lock_items", "Lock the Dock's contents", "No dragging icons in, out or around, and no Keep in Dock", Kind::Toggle),
    s(Page::Dock, "Menu Bar", "menu_bar.auto_hide", "Automatically hide and show the menu bar", "Shows when the pointer touches the top edge", Kind::Toggle),
    s(
        Page::Dock,
        "Menu Bar",
        "menu_bar.space_indicator",
        "Show the current Space",
        "A click opens Mission Control",
        Kind::Choice(&[("off", "Off"), ("number", "Number"), ("dots", "Dots")]),
    ),
    s(
        Page::Dock,
        "Windows",
        "windows.move_modifier",
        "Drag windows with",
        "Hold the key and drag anywhere in a window",
        Kind::Choice(&[("super", "Super"), ("alt", "Alt")]),
    ),
    s(
        Page::Dock,
        "Windows",
        "windows.focus",
        "Focus follows the pointer",
        "The window under the pointer gets the keyboard after a moment, without coming to the front",
        Kind::Flag("click", "hover"),
    ),
    s(Page::Dock, "Windows", "windows.magnetic_edges", "Magnetic edges", "Edges stick to screen and window edges", Kind::Toggle),
    s(Page::Dock, "Windows", "windows.snapping", "Tile at screen edges", "Drag to a side, a corner or the top; hold Ctrl to skip", Kind::Toggle),
    s(
        Page::Dock,
        "Windows",
        "windows.keyboard_nudge",
        "Move and resize with the keyboard",
        "Super+Alt+arrows move the window 16 px; add Shift to resize it",
        Kind::Toggle,
    ),
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
    s(
        Page::Keyboard,
        "Input source switching",
        "keyboard.per_window_source",
        "Remember the input source per window",
        "Each window switches back to the layout it was typed in",
        Kind::Toggle,
    ),
    s(
        Page::Keyboard,
        "Input method",
        "keyboard.input_method",
        "Input method engine",
        "For Pinyin, Nepali and other scripts; from the next login",
        Kind::Menu(&[("fcitx5", "fcitx5"), ("ibus", "ibus"), ("auto", "Automatic"), ("none", "None")]),
    ),
    s(Page::Keyboard, "Modifier keys", "keyboard.options", "Caps Lock key", "", Kind::XkbOption(crate::xkb::is_caps, crate::xkb::CAPS)),
    s(
        Page::Keyboard,
        "Modifier keys",
        "keyboard.options",
        "Compose key",
        "Then type a sequence, like ' e for é",
        Kind::XkbOption(crate::xkb::is_compose, crate::xkb::COMPOSE),
    ),
    s(Page::Keyboard, "Modifier keys", "keyboard.options", "Control and Alt", "", Kind::XkbOption(crate::xkb::is_ctrl_alt, crate::xkb::CTRL_ALT)),
    s(
        Page::Keyboard,
        "Modifier keys",
        "keyboard.options",
        "Other xkb options",
        "Like grp:alt_shift_toggle; Caps Lock, Compose, Control and Alt above",
        Kind::Text("None"),
    ),
    s(
        Page::Keyboard,
        "Modifier keys",
        "app_drawer.super_tap",
        "Tap Super to open the app drawer",
        "Press and release it on its own; shortcuts and Super+drag don't count",
        Kind::Toggle,
    ),
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
    s(Page::Trackpad, "", "touchpad.three_finger_drag", "Three-finger drag", "Drag windows and selections with three fingers", Kind::Toggle),
    s(Page::Trackpad, "", "touchpad.disable_while_typing", "Ignore while typing", "", Kind::Toggle),
    s(Page::Trackpad, "", "touchpad.ignore_with_mouse", "Ignore when a mouse is plugged in", "", Kind::Toggle),
    s(
        Page::Trackpad,
        "Gestures",
        "touchpad.swipe_fingers",
        "Swipe with",
        "Four leaves three-finger swipes to apps (always four with three-finger drag)",
        Kind::Steps(&[(3, "Three fingers"), (4, "Four fingers")]),
    ),
    s(
        Page::Trackpad,
        "Gestures",
        "touchpad.gesture_sensitivity",
        "Gesture sensitivity",
        "Higher: a shorter swipe or pinch is enough",
        Kind::Slider { min: 0.25, max: 4.0, step: 0.25 },
    ),
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
    s(
        Page::Network,
        "Proxy",
        "network.proxy",
        "Proxy",
        "For apps started from the desktop; Automatic reaches only GTK apps",
        Kind::Choice(&[("none", "None"), ("manual", "Manual"), ("auto", "Automatic")]),
    ),
    s(Page::Network, "Proxy", "network.proxy_http", "HTTP proxy", "Manual: host:port", Kind::Text("None")),
    s(Page::Network, "Proxy", "network.proxy_https", "HTTPS proxy", "Manual: host:port", Kind::Text("None")),
    s(Page::Network, "Proxy", "network.proxy_socks", "SOCKS proxy", "Manual: host:port", Kind::Text("None")),
    s(Page::Network, "Proxy", "network.proxy_bypass", "Bypass proxies for", "Comma separated hosts and networks", Kind::Text("localhost, .lan")),
    s(Page::Network, "Proxy", "network.proxy_pac", "Auto-config URL", "Automatic: the PAC file", Kind::Text("http://wpad/wpad.dat")),
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
    s(
        Page::DateTime,
        "Calendar",
        "calendar.files",
        "Calendars",
        "Events for Up Next and reminders: .ics files, folders or dir/*.ics",
        Kind::List(Some(CALENDAR_ADD)),
    ),
    s(
        Page::DateTime,
        "Calendar",
        "calendar.caldav",
        "CalDAV accounts",
        "A server address (or one calendar's URL), user and app password; events are fetched every 15 minutes",
        Kind::List(Some(CALDAV_ADD)),
    ),
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
    s(Page::Notifications, "Weather", "weather.city", "City", "Looked up once, never located by IP; empty = the coordinates below", Kind::Text("Kathmandu")),
    s(Page::Notifications, "Weather", "weather.latitude", "Latitude", "North is positive; setting it clears City", Kind::Degrees(90.0)),
    s(Page::Notifications, "Weather", "weather.longitude", "Longitude", "East is positive; setting it clears City", Kind::Degrees(180.0)),
    s(Page::Notifications, "Weather", "weather.units", "Units", "", Kind::Choice(&[("celsius", "°C"), ("fahrenheit", "°F")])),
    s(
        Page::Notifications,
        "World Clock",
        "widgets.clocks",
        "Cities",
        "Their time in Notification Center; Add… takes a city or zone name",
        Kind::List(Some(CLOCK_ADD)),
    ),
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
    s(
        Page::Privacy,
        "Screen Recording",
        "privacy.screen_recording",
        "Allowed to record the screen",
        "Programs ask the first time; Remove asks again",
        Kind::List(None),
    ),
    s(Page::Privacy, "Screen Recording", "privacy.screen_recording_denied", "Denied", "", Kind::List(None)),
    s(
        Page::Privacy,
        "Keyboard & Mouse Control",
        "privacy.input_control",
        "Allowed to type and click",
        "Programs ask the first time; Remove asks again",
        Kind::List(None),
    ),
    s(Page::Privacy, "Keyboard & Mouse Control", "privacy.input_control_denied", "Denied", "", Kind::List(None)),
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
    s(Page::General, "Software Update", "update.channel", "Updates", "Beta also offers pre-releases", Kind::Choice(&[("stable", "Stable"), ("beta", "Beta")])),
    s(
        Page::General,
        "Software Update",
        "update.install_at_login",
        "Install updates automatically at login",
        "Admins only, no password; never a beta or a reverted version",
        Kind::Toggle,
    ),
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
        "sound.output" | "sound.input" | "sound.alert_volume" | "keyboard.backlight" | "display.brightness" => format!("{value:.0}%"),
        "sound.balance" if value.abs() < 0.5 => "Centre".into(),
        "sound.balance" => format!("{} {:.0}", if value < 0.0 { "L" } else { "R" }, value.abs()),
        "keyboard.repeat_rate" if value < 1.0 => "Off".into(),
        "keyboard.repeat_rate" => format!("{value:.0}/s"),
        "keyboard.repeat_delay" | "mouse.double_click_ms" | "dock.hide_delay" | "dock.unhide_delay" => format!("{value:.0} ms"),
        "appearance.dark_from" | "appearance.dark_until" | "night_light.from" | "night_light.until" | "notifications.dnd_from" | "notifications.dnd_until" => {
            format!("{value:02.0}:00")
        }
        "night_light.temperature" => format!("{value:.0} K"),
        "touchpad.gesture_sensitivity" => format!("{}×", (value * 100.0).round() / 100.0),
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
        Kind::Flag(off, on) => toggle(t, app.config.str(key) == on, move |yes| Message::Choose(key.into(), if yes { on } else { off })),
        Kind::Choice(options) => choice(t, app.config.str(key), options, key),
        Kind::Menu(options) => {
            let current = app.config.str(key);
            let selected = options.iter().find(|o| o.0 == current).map(|&(v, l)| Labelled(v, l));
            let pick =
                pick_list(options.iter().map(|&(v, l)| Labelled(v, l)).collect::<Vec<_>>(), selected, move |c: Labelled| Message::Choose(key.into(), c.0));
            pick.placeholder(current).text_size(13).padding([3, 8]).style(t.pick()).menu_style(t.menu()).into()
        }
        // Three-finger drag holds three fingers: the swipes take four (src/config.rs `effective_swipe_fingers`).
        Kind::Steps(_) if key == "touchpad.swipe_fingers" && app.config.bool("touchpad.three_finger_drag") => {
            text("Four fingers").size(13).color(t.secondary()).into()
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
        Kind::Gpu => {
            let current = app.config.str(key);
            let gpus = std::iter::once(Labelled("", "Automatic")).chain(crate::system::GPUS.iter().map(|(v, l)| Labelled(v, l)));
            let gpus: Vec<_> = gpus.collect();
            let selected = gpus.iter().copied().find(|g| g.0 == current);
            let pick = pick_list(gpus, selected, move |c: Labelled| Message::Choose(key.into(), c.0));
            pick.placeholder(current).text_size(13).padding([3, 8]).style(t.pick()).menu_style(t.menu()).into()
        }
        Kind::Degrees(limit) => {
            let saved = app.config.num(key);
            let value = app.texts.get(key).cloned().unwrap_or_else(|| if saved == 0.0 { String::new() } else { saved.to_string() });
            text_input(&format!("−{limit:.0} … {limit:.0}"), &value)
                .on_input(move |v| Message::Type(key.into(), v))
                .on_submit(Message::Submit(key.into()))
                .size(13)
                .padding([3, 8])
                .width(120)
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
        Kind::List(None) => space().into(),
        Kind::List(Some(add)) => button(text("Add…").size(12)).padding([3, 10]).style(t.push(false)).on_press(Message::OpenFile(add)).into(),
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
/// ones on, but `lock` off).
#[derive(Clone, Debug, PartialEq)]
pub struct AppRule {
    pub style: String,
    pub sound: bool,
    pub badge: bool,
    pub center: bool,
    pub lock: bool,
}

impl AppRule {
    pub fn read(value: Option<&toml_edit::Value>) -> Self {
        let table = value.and_then(toml_edit::Value::as_inline_table);
        let style = value.and_then(toml_edit::Value::as_str).or_else(|| table?.get("style")?.as_str()).unwrap_or("banner");
        let switch = |key| table.and_then(|t| t.get(key)?.as_bool()).unwrap_or(true);
        let lock = table.and_then(|t| t.get("lock")?.as_bool()).unwrap_or(false);
        AppRule { style: style.to_owned(), sound: switch("sound"), badge: switch("badge"), center: switch("center"), lock }
    }

    /// As written: the style alone while every switch is as by default, else a table.
    pub fn value(&self) -> toml_edit::Value {
        if self.sound && self.badge && self.center && !self.lock {
            return self.style.as_str().into();
        }
        let mut table = toml_edit::InlineTable::new();
        table.insert("style", self.style.as_str().into());
        table.insert("sound", self.sound.into());
        table.insert("badge", self.badge.into());
        table.insert("center", self.center.into());
        table.insert("lock", self.lock.into());
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
        let switches: [(&str, &str, bool, Switch); 4] = [
            ("Play sound", "", rule.sound, |r| &mut r.sound),
            ("Badge app icon", "", rule.badge, |r| &mut r.badge),
            ("Show in Notification Center", "", rule.center, |r| &mut r.center),
            ("Show on lock screen", "Its titles under the clock; texts too with previews Always", rule.lock, |r| &mut r.lock),
        ];
        for (label, help, on, field) in switches {
            let (yes, no) = (set(&|r| *field(r) = true), set(&|r| *field(r) = false));
            rows.push(line(t, label, help, toggle(t, on, move |v| if v { yes.clone() } else { no.clone() })));
        }
    }
    vec![group(t, "Applications", rows)]
}

/// `[widgets]` as the desktop reads it: Notification Center's order, the desktop's, and the hidden ids.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Widgets {
    pub center: Vec<String>,
    pub desktop: Vec<String>,
    pub hidden: Vec<String>,
}

impl Widgets {
    pub const KEYS: [&str; 3] = ["widgets.center", "widgets.desktop", "widgets.hidden"];

    pub fn read(config: &crate::config::Config) -> Widgets {
        let [center, desktop, hidden] = Self::KEYS.map(|key| config.list(key).into_iter().map(str::to_owned).collect());
        Widgets { center, desktop, hidden }
    }

    /// What one editor shows, in order, as the desktop places it: the desktop's list, or Notification Center's
    /// then every `live` id listed nowhere. Ids not on the bus stay where they are.
    pub fn shown(&self, live: &[String], desktop: bool) -> Vec<String> {
        if desktop {
            return self.desktop.clone();
        }
        let listed = |id: &String| [&self.center, &self.desktop, &self.hidden].iter().any(|list| list.contains(id));
        let mut shown = self.center.clone();
        shown.extend(live.iter().filter(|id| !listed(id)).cloned());
        shown.dedup();
        shown
    }

    /// One editor's rows: (id, shown), the shown in order, then every other known id, sorted.
    pub fn rows(&self, live: &[String], desktop: bool) -> Vec<(String, bool)> {
        let shown = self.shown(live, desktop);
        let mut rest: Vec<String> = [live, &self.center, &self.desktop, &self.hidden].concat().into_iter().filter(|id| !shown.contains(id)).collect();
        rest.sort();
        rest.dedup();
        shown.into_iter().map(|id| (id, true)).chain(rest.into_iter().map(|id| (id, false))).collect()
    }

    /// `id` shown at the end of the editor's list, or taken out of it (out of Notification Center: into `hidden`).
    pub fn toggled(&self, live: &[String], desktop: bool, id: &str, on: bool) -> Widgets {
        let mut new = self.clone();
        let mut list = self.shown(live, desktop);
        list.retain(|i| i != id);
        if on {
            list.push(id.to_owned());
            new.hidden.retain(|i| i != id);
        } else if !desktop && !self.desktop.iter().any(|i| i == id) && !self.hidden.iter().any(|i| i == id) {
            new.hidden.push(id.to_owned());
        }
        *new.list(desktop) = list;
        new
    }

    /// `id` one place up (`-1`) or down in the editor's list.
    pub fn moved(&self, live: &[String], desktop: bool, id: &str, by: isize) -> Widgets {
        let mut new = self.clone();
        let mut list = self.shown(live, desktop);
        if let Some(i) = list.iter().position(|i| i == id)
            && let Some(j) = i.checked_add_signed(by).filter(|&j| j < list.len())
        {
            list.swap(i, j);
        }
        *new.list(desktop) = list;
        new
    }

    fn list(&mut self, desktop: bool) -> &mut Vec<String> {
        if desktop { &mut self.desktop } else { &mut self.center }
    }

    /// The keys to save, as TOML arrays (the same writer as `Kind::List`).
    pub fn values(&self) -> [(&'static str, toml_edit::Value); 3] {
        let array = |list: &Vec<String>| list.iter().collect::<toml_edit::Array>().into();
        [(Self::KEYS[0], array(&self.center)), (Self::KEYS[1], array(&self.desktop)), (Self::KEYS[2], array(&self.hidden))]
    }
}

/// Notifications ▸ Notification Center widgets and Desktop widgets: a switch per widget; ↑/↓ on the shown ones.
fn widget_editors(app: &App) -> Vec<Element<'_, Message>> {
    let t = app.tokens;
    let places = Widgets::read(&app.config);
    let live = &app.widget_ids;
    [("Notification Center widgets", false), ("Desktop widgets", true)]
        .into_iter()
        .map(|(title, desktop)| {
            let rows = places.rows(live, desktop);
            let count = rows.iter().filter(|(_, on)| *on).count();
            let mut lines: Vec<Element<'_, Message>> = rows
                .into_iter()
                .enumerate()
                .map(|(i, (id, on))| {
                    let arrow = |label, by: isize, enabled: bool| {
                        let b = button(text(label).size(12)).padding([3, 8]).style(t.push(false));
                        b.on_press_maybe(enabled.then(|| Message::Widgets(places.moved(live, desktop, &id, by))))
                    };
                    let (yes, no) = (places.toggled(live, desktop, &id, true), places.toggled(live, desktop, &id, false));
                    let mut controls = row![].spacing(6).align_y(Alignment::Center);
                    if on {
                        controls = controls.push(arrow("↑", -1, i > 0)).push(arrow("↓", 1, i + 1 < count));
                    }
                    controls = controls.push(toggle(t, on, move |v| Message::Widgets(if v { yes.clone() } else { no.clone() })));
                    line(t, id, "", controls.into())
                })
                .collect();
            if lines.is_empty() {
                lines.push(note(t, "Widgets show here once an app adds one."));
            }
            group(t, title, lines)
        })
        .collect()
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
        let mut row = line(app.tokens, setting.label, setting.help, control(app, setting));
        if setting.page == Page::Trackpad {
            row = crate::preview::room(app, setting.key, row);
        }
        let mut rows = vec![row];
        if let Kind::List(add) = setting.kind {
            rows.extend(list_rows(app, setting.key, add));
        }
        match groups.last_mut() {
            Some((group, old)) if *group == setting.group => old.extend(rows),
            _ => groups.push((setting.group, rows)),
        }
    }
    groups
}

/// A `Kind::List`'s entries under its row, each with Remove; then Add…'s fields: the path when it found no chooser,
/// the city, or a CalDAV account's.
fn list_rows<'a>(app: &'a App, key: &'static str, add: Option<&'static str>) -> Vec<Element<'a, Message>> {
    let t = app.tokens;
    let items = app.config.get(key).and_then(toml_edit::Value::as_array).map(|a| a.iter().collect::<Vec<_>>()).unwrap_or_default();
    let mut rows: Vec<Element<'a, Message>> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let name = match (item.as_str(), account(item)) {
                (Some(path), _) => path.to_owned(),
                (_, Some((url, user))) => format!("{user} · {url}"),
                _ => item.to_string(),
            };
            let remove = button(text("Remove").size(12)).padding([3, 10]).style(t.push(false)).on_press(Message::Remove(key, i));
            line(t, name, "", remove.into())
        })
        .collect();
    if items.is_empty() {
        rows.push(container(text("None").size(12).color(t.secondary())).padding([8, 12]).into());
    }
    let caldav = add == Some(CALDAV_ADD);
    let fields = match add {
        Some(CALDAV_ADD) => vec![(CALDAV[0], "Server"), (CALDAV[1], "User"), (CALDAV[2], "Password")],
        Some(CLOCK_ADD) => vec![(CLOCK_ADD, "City")],
        Some(add) => vec![(add, "Path")],
        None => Vec::new(),
    };
    for (field, label) in fields {
        let Some(typed) = app.texts.get(field) else { continue };
        let input = text_input(label, typed)
            .secure(field == CALDAV[2])
            .on_input(move |v| Message::Type(field.into(), v))
            .on_submit(if caldav { Message::AddCalDav } else { Message::Submit(field.into()) })
            .size(13)
            .padding([3, 8])
            .width(240)
            .style(t.input(false));
        rows.push(line(t, label, "", input.into()));
    }
    if caldav && app.texts.contains_key(CALDAV_ADD) {
        let label = if app.caldav_checking { "Checking…" } else { "Add Account" };
        let go = button(text(label).size(12)).padding([3, 10]).style(t.push(true)).on_press_maybe((!app.caldav_checking).then_some(Message::AddCalDav));
        rows.push(line(t, "", "Its calendars are found and fetched once to check the password", go.into()));
    }
    rows
}

pub fn page<'a>(app: &'a App, page: Page) -> Element<'a, Message> {
    let t = app.tokens;
    let mut parts: Vec<Element<'a, Message>> = vec![title(page.name())];
    parts.extend(system_part(app, page));
    for (header, mut rows) in settings_of(app, |s| s.page == page) {
        if header == "Software Update" {
            let (before, after) = update_rows(app);
            rows.splice(0..0, before);
            rows.extend(after);
        }
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
        Page::Network => match &status.wifi {
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
                        let mut rows = Vec::new();
                        for d in mine {
                            let (state, action) = if d.connected { ("Connected", "Disconnect") } else { ("Not connected", "Connect") };
                            let key = format!("{}{}", crate::system::RENAME, d.path);
                            let buttons = row![
                                push(action, Message::Device(d.path.clone(), !d.connected)),
                                push("Rename", Message::Type(key.clone(), d.name.clone())),
                                push("Forget", Message::Forget(d.path.clone()))
                            ];
                            rows.push(line(t, &d.name, state, buttons.spacing(8).into()));
                            // Rename opens a field under the row; Return saves it.
                            if let Some(name) = app.texts.get(&key) {
                                let field = text_input(&d.name, name)
                                    .on_input({
                                        let key = key.clone();
                                        move |v| Message::Type(key.clone(), v)
                                    })
                                    .on_submit(Message::Submit(key.clone()))
                                    .size(13)
                                    .padding([3, 8])
                                    .width(180)
                                    .style(t.input(false));
                                let save = button(text("Save").size(12)).padding([3, 10]).style(t.push(true)).on_press(Message::Submit(key));
                                rows.push(line(t, "Name", "", row![field, save].spacing(8).align_y(Alignment::Center).into()));
                            }
                        }
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
            let copy = button(text("Copy System Report").size(12)).padding([3, 10]).style(t.push(false)).on_press(Message::CopyReport);
            vec![container(head).center_x(Length::Fill).into(), group(t, "", rows), container(copy).align_right(Length::Fill).into()]
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
        Page::Privacy => {
            let check = match status.connectivity {
                Some(on) => line(
                    t,
                    "Connectivity check",
                    "NetworkManager asks a web address whether the internet is reachable; off also stops Wi-Fi sign-in page detection",
                    toggle(t, on, Message::Connectivity),
                ),
                None => fact(t, "Connectivity check", "Off in NetworkManager's configuration".into()),
            };
            let mut parts = vec![group(t, "Network", vec![check])];
            for (device, label) in crate::system::DEVICES {
                let rows: Vec<_> = status
                    .grants
                    .iter()
                    .filter(|g| g.device == device)
                    .map(|g| {
                        let app = g.app.clone();
                        line(t, g.app.as_str(), "", toggle(t, g.allowed, move |on| Message::Grant(device.into(), app.clone(), on)))
                    })
                    .collect();
                if !rows.is_empty() {
                    parts.push(group(t, label, rows));
                }
            }
            if status.grants.is_empty() {
                parts.push(note(t, "Sandboxed apps that ask for the camera or microphone show here, each with Allow."));
            }
            parts
        }
        Page::Appearance => vec![gallery(app)],
        Page::Users => users_part(app),
        Page::Printers => printers_part(app),
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
    let mut vpns: Vec<_> = vpns.collect();
    // The file chooser; the path field only when there is no portal.
    let import: Element<'_, Message> = match app.texts.get(crate::network::WIREGUARD) {
        Some(typed) => {
            let field = text_input("/path/to/wg0.conf", typed)
                .on_input(|v| Message::Type(crate::network::WIREGUARD.into(), v))
                .on_submit(Message::ImportWireGuard)
                .size(13)
                .padding([3, 8])
                .width(180)
                .style(t.input(false));
            let import = button(text("Import").size(12))
                .padding([3, 10])
                .style(t.push(false))
                .on_press_maybe((!typed.trim().is_empty()).then_some(Message::ImportWireGuard));
            row![field, import].spacing(8).align_y(Alignment::Center).into()
        }
        None => button(text("Import…").size(12)).padding([3, 10]).style(t.push(false)).on_press(Message::OpenFile(crate::network::WIREGUARD)).into(),
    };
    vpns.push(line(t, "Import WireGuard", "A wg-quick .conf file", import));
    parts.push(group(t, "VPN", vpns));
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

/// A saved connection's Details sheet: Auto-Join, IPv4 and IPv6 (automatic or manual) with DNS, Forget (fields in
/// `App::texts`, IPv6's ending in "6").
fn connection_details<'a>(app: &'a App, k: &'a crate::network::Known) -> Element<'a, Message> {
    let t = app.tokens;
    let push = |label: &'static str, primary: bool, message: Message| button(text(label).size(12)).padding([3, 10]).style(t.push(primary)).on_press(message);
    let key = |field: &str| format!("{}{field}", crate::network::DETAILS);
    let value = |field: &str| app.texts.get(&key(field)).map_or("", String::as_str);
    let field = |field: &'static str, placeholder: &'static str| -> Element<'a, Message> {
        text_input(placeholder, value(field)).on_input(move |v| Message::Type(key(field), v)).size(13).padding([3, 8]).width(180).style(t.input(false)).into()
    };
    let path = k.path.clone();
    let method = |field: &'static str, auto: &'static str| {
        segments(
            t,
            [("auto", auto), ("manual", "Manual")].map(|(v, label)| (label.to_owned(), value(field) == v, Message::Type(key(field), v.into()))).to_vec(),
        )
    };
    let mut rows = vec![
        line(t, "Auto-Join", "", toggle(t, k.auto_join, move |on| Message::AutoJoin(path.clone(), on))),
        line(t, "Configure IPv4", "", method("method", "Automatic (DHCP)")),
    ];
    if value("method") == "manual" {
        rows.push(line(t, "IP address", "", field("address", "192.168.1.20")));
        rows.push(line(t, "Prefix length", "24 = subnet mask 255.255.255.0", field("prefix", "24")));
        rows.push(line(t, "Router", "", field("gateway", "192.168.1.1")));
    }
    rows.push(line(t, "DNS servers", "Comma separated; empty = from the network", field("dns", "1.1.1.1, 9.9.9.9")));
    rows.push(line(t, "Configure IPv6", "", method("method6", "Automatic")));
    if value("method6") == "manual" {
        rows.push(line(t, "IPv6 address", "", field("address6", "2001:db8::20")));
        rows.push(line(t, "Prefix length", "1–128, usually 64", field("prefix6", "64")));
        rows.push(line(t, "Router", "", field("gateway6", "fe80::1")));
    }
    rows.push(line(t, "IPv6 DNS servers", "Comma separated; empty = from the network", field("dns6", "2606:4700:4700::1111")));
    let forget = push(if k.wifi { "Forget This Network" } else { "Forget This Connection" }, false, Message::ForgetNetwork(k.path.clone()));
    let buttons =
        row![forget, space().width(Length::Fill), push("Cancel", false, Message::NetDetails(None)), push("Update", true, Message::UpdateIp)].spacing(8);
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
        line(t, "Security", "", pick("security", &[("none", "None"), ("psk", "WPA2/WPA3 Personal"), ("sae", "WPA3 Personal"), ("eap", "WPA2 Enterprise")])),
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
    // "+ Add Folder…" opens the folder chooser (a path field under the pictures without a portal); its pictures join the gallery.
    let add = container(text("+ Add Folder…").size(12)).center_x(w).center_y(h);
    tiles.push(button(add).padding(3).style(t.tile(false)).on_press(Message::OpenFile(crate::gallery::ADD)).into());
    let grid = row(tiles).spacing(8).wrap().vertical_spacing(8);
    let mut rows = vec![container(grid).padding(12).width(Length::Fill).into()];
    if let Some(path) = app.texts.get(crate::gallery::ADD) {
        let field = text_input("~/Pictures/Holiday", path)
            .on_input(|v| Message::Type(crate::gallery::ADD.into(), v))
            .on_submit(Message::Submit(crate::gallery::ADD.into()))
            .size(13)
            .padding([3, 8])
            .width(220)
            .style(t.input(false));
        let save = button(text("Add").size(12)).padding([3, 10]).style(t.push(true)).on_press(Message::Submit(crate::gallery::ADD.into()));
        rows.push(line(t, "Folder", "", row![field, save].spacing(8).align_y(Alignment::Center).into()));
    }
    group(t, "Wallpaper", rows)
}

/// Printers: each CUPS queue with its state, Options… for default, test page, removal and its jobs; Add Printer….
fn printers_part(app: &App) -> Vec<Element<'_, Message>> {
    use crate::printers::{Act, suggest_name};
    let t = app.tokens;
    // Still reading.
    let Some(found) = &app.printers else { return Vec::new() };
    let Some(cups) = found else { return vec![note(t, "CUPS isn't installed: printing needs the cups package.")] };
    let push = |label: &'static str, message: Message| button(text(label).size(12)).padding([3, 10]).style(t.push(false)).on_press(message);
    let mut rows = Vec::new();
    for p in &cups.printers {
        let open = app.printer.as_ref() == Some(&p.name);
        let state = if p.default { format!("{} · Default", p.state.name()) } else { p.state.name().to_owned() };
        let more = push(if open { "Done" } else { "Options…" }, Message::SelectPrinter((!open).then(|| p.name.clone())));
        rows.push(line(t, p.name.as_str(), "", row![text(state).size(12).color(t.secondary()), more].spacing(8).align_y(Alignment::Center).into()));
        if !open {
            continue;
        }
        let default: Element<'_, Message> = match p.default {
            true => text("This printer").size(13).color(t.secondary()).into(),
            false => push("Set as Default", Message::PrinterAct(Act::Default(p.name.clone()))).into(),
        };
        rows.push(line(t, "Default printer", "", default));
        rows.push(line(t, "Test page", "", push("Print Test Page", Message::PrinterAct(Act::TestPage(p.name.clone()))).into()));
        let jobs: Vec<_> = cups.jobs.iter().filter(|j| j.printer == p.name).collect();
        if jobs.is_empty() {
            rows.push(fact(t, "Queue", "No jobs".into()));
        }
        for j in jobs {
            let words = format!("{} · {} · {} KB", j.id, j.user, j.size.div_ceil(1024));
            rows.push(line(t, words, "", push("Cancel", Message::PrinterAct(Act::Cancel(j.id.clone()))).into()));
        }
        rows.push(line(t, "", "", push("Remove Printer", Message::PrinterAct(Act::Remove(p.name.clone()))).into()));
    }
    let mut parts = vec![if rows.is_empty() { note(t, "No printers yet.") } else { group(t, "Printers", rows) }];
    match &app.devices {
        None => parts.push(row![space().width(Length::Fill), push("Add Printer…", Message::AddPrinter(true))].into()),
        Some(devices) => {
            let mut rows = match devices {
                None => vec![fact(t, "Looking for printers…", String::new())],
                Some(found) if found.is_empty() => vec![fact(t, "None found nearby", String::new())],
                Some(found) => found
                    .iter()
                    .map(|uri| {
                        let words = column![text(suggest_name(uri)).size(13), text(uri.as_str()).size(11).color(t.secondary())].spacing(2);
                        let add = push("Add", Message::PrinterAct(Act::Add(suggest_name(uri), uri.clone())));
                        row![words.width(Length::Fill), add].spacing(12).align_y(Alignment::Center).padding([8, 12]).into()
                    })
                    .collect(),
            };
            let typed = app.texts.get(crate::printers::URI).map_or("", String::as_str);
            let uri = text_input("ipp://printer.local/ipp/print", typed)
                .on_input(|v| Message::Type(crate::printers::URI.into(), v))
                .on_submit(Message::PrinterAct(Act::Add(suggest_name(typed.trim()), typed.trim().to_owned())))
                .size(13)
                .padding([3, 8])
                .width(260)
                .style(t.input(false));
            rows.push(line(t, "Address", "A driverless (IPP Everywhere) printer's URI; press Return", uri.into()));
            parts.push(group(t, "Add Printer", rows));
            parts.push(row![space().width(Length::Fill), push("Done", Message::AddPrinter(false))].into());
        }
    }
    parts
}

/// Users: who is signed in, with their picture and names, then everyone else, and Add User….
fn users_part(app: &App) -> Vec<Element<'_, Message>> {
    let t = app.tokens;
    // Still reading.
    let Some(users) = &app.users else { return Vec::new() };
    let Some(me) = &users.me else { return vec![group(t, "", vec![fact(t, "AccountsService isn't running", String::new())])] };
    let head =
        row![picture(t, me, 64.0), column![text(me.shown()).size(17).font(font(Weight::Semibold)), text(&me.login).size(12).color(t.secondary())].spacing(2),]
            .spacing(14)
            .align_y(Alignment::Center);
    let name = app.texts.get(crate::users::NAME).unwrap_or(&me.name);
    let name = text_input(&me.login, name)
        .on_input(|v| Message::Type(crate::users::NAME.into(), v))
        .on_submit(Message::Submit(crate::users::NAME.into()))
        .size(13)
        .padding([3, 8])
        .width(200)
        .style(t.input(false));
    let change = button(text("Change Picture…").size(12)).padding([3, 10]).style(t.push(false)).on_press(Message::PictureChooser);
    let password = button(text("Change Password…").size(12)).padding([3, 10]).style(t.push(false)).on_press(Message::PasswordForm);
    let mut rows =
        vec![line(t, "Picture", "", change.into()), line(t, "Full name", "Press Return to save", name.into()), line(t, "Password", "", password.into())];
    if app.texts.contains_key(crate::users::PASSWORD[0]) {
        for (key, label) in crate::users::PASSWORD.into_iter().zip(["Current password", "New password", "Verify"]) {
            let field = text_input("", app.texts.get(key).map_or("", String::as_str))
                .secure(true)
                .on_input(move |v| Message::Type(key.into(), v))
                .on_submit(Message::ChangePassword)
                .size(13)
                .padding([3, 8])
                .width(200)
                .style(t.input(false));
            rows.push(line(t, label, "", field.into()));
        }
        let go = button(text("Change Password").size(12)).padding([3, 10]).style(t.push(true)).on_press(Message::ChangePassword);
        rows.push(line(t, "", "", go.into()));
    }
    // With a fingerprint reader only.
    if let Some(n) = users.fingers {
        let count = match n {
            0 => "None enrolled".to_owned(),
            1 => "1 finger".to_owned(),
            n => format!("{n} fingers"),
        };
        let add = button(text("Add Fingerprint…").size(12)).padding([3, 10]).style(t.push(false));
        let mut controls = row![text(count).size(12).color(t.secondary()), add.on_press_maybe((n < 10).then_some(Message::Enroll(true)))]
            .spacing(8)
            .align_y(Alignment::Center);
        if n > 0 {
            controls = controls.push(button(text("Delete All").size(12)).padding([3, 10]).style(t.push(false)).on_press(Message::DeleteFingerprints));
        }
        rows.push(line(t, "Fingerprints", "", controls.into()));
    }
    if app.texts.contains_key(crate::users::PICTURES) {
        // The wallpaper gallery's pictures, then any file from the chooser (by its path without a portal).
        let tiles = app.thumbs.iter().map(|(path, handle)| {
            let tile = iced::widget::image(handle.clone()).width(48).height(48).content_fit(iced::ContentFit::Cover).border_radius(24.0);
            button(tile).padding(2).style(t.tile(false)).on_press(Message::Picture(path.clone())).into()
        });
        rows.insert(1, container(row(tiles).spacing(8).wrap().vertical_spacing(8)).padding(12).into());
        let other = match app.texts.get(crate::users::PICTURE_FILE) {
            Some(typed) => {
                let file = text_input("/path/to/picture.png", typed)
                    .on_input(|v| Message::Type(crate::users::PICTURE_FILE.into(), v))
                    .on_submit(Message::Picture(typed.trim().into()))
                    .size(13)
                    .padding([3, 8])
                    .width(260)
                    .style(t.input(false));
                line(t, "Other picture", "A PNG or JPEG file; press Return", file.into())
            }
            None => {
                let choose = button(text("Choose…").size(12)).padding([3, 10]).style(t.push(false)).on_press(Message::OpenFile(crate::users::PICTURE_FILE));
                line(t, "Other picture", "A PNG or JPEG file", choose.into())
            }
        };
        rows.insert(2, other);
        if users.camera.is_some() {
            let take = button(text("Take Photo…").size(12)).padding([3, 10]).style(t.push(false)).on_press(Message::TakePhoto(true));
            rows.insert(3, line(t, "Camera", "", take.into()));
        }
    }
    let mut parts = vec![container(head).padding([0, 4]).into(), group(t, "", rows)];
    if !users.others.is_empty() {
        let rows = users
            .others
            .iter()
            .map(|a| {
                let words = column![text(a.shown()).size(13), text(&a.login).size(11).color(t.secondary())].spacing(2);
                let delete = button(text("Delete User…").size(12)).padding([3, 10]).style(t.push(false)).on_press(Message::DeleteUser(Some(a.path.clone())));
                row![picture(t, a, 32.0), words.width(Length::Fill), delete].spacing(10).align_y(Alignment::Center).padding([6, 12]).into()
            })
            .collect();
        parts.push(group(t, "Other Users", rows));
    }
    let add = button(text("Add User…").size(12)).padding([3, 10]).style(t.push(false)).on_press(Message::AddUserForm);
    parts.push(row![space().width(Length::Fill), add].into());
    parts
}

/// Users' Add User… sheet or Delete User… alert over the window, when one is open.
pub fn users_sheet(app: &App) -> Option<Element<'_, Message>> {
    use crate::users::{NEW, Shot};
    let t = app.tokens;
    let push = |label: &'static str, primary: bool, message: Message| button(text(label).size(13)).padding([4, 14]).style(t.push(primary)).on_press(message);
    if let Some(e) = &app.enroll {
        let count = if e.stages > 0 { format!("{} of {}", e.touches.min(e.stages), e.stages) } else { e.touches.to_string() };
        let words = match (&e.note, e.done) {
            (Some(note), _) => text(note).size(12).color(crate::style::rgb(0xFF3B30)),
            (None, true) => text("Fingerprint added").size(12),
            (None, false) => text("Touch the sensor, lift your finger, and touch again").size(12).color(t.secondary()),
        };
        let shown = container(text(count).size(32).font(font(Weight::Semibold))).center(128).style(Tokens::squircle(t.card(), 64.0));
        let close = push(if e.done { "Done" } else { "Cancel" }, e.done, Message::Enroll(false));
        let card = column![
            text("Add Fingerprint").size(15).font(font(Weight::Semibold)),
            container(shown).center_x(Length::Fill),
            container(words).center_x(Length::Fill),
            row![space().width(Length::Fill), close],
        ]
        .spacing(12)
        .width(320);
        let sheet = container(card).padding(18).style(t.sheet());
        return Some(iced::widget::opaque(container(sheet).center(Length::Fill).style(t.dim())));
    }
    if let Some(shot) = &app.shot {
        // A 128 px square, as the picture will be cut.
        let frame = |inner: Element<'static, Message>| container(inner).center(128).style(Tokens::squircle(t.card(), 64.0));
        let (shown, buttons): (Element<'_, Message>, Vec<Element<'_, Message>>) = match shot {
            Shot::Count(n) => (frame(text(n.to_string()).size(48).font(font(Weight::Semibold)).into()).into(), Vec::new()),
            Shot::Taking => (frame(text("Smile").size(15).into()).into(), Vec::new()),
            Shot::Taken(photo) => (
                iced::widget::image(photo.clone()).width(128).height(128).border_radius(64.0).into(),
                vec![push("Retake", false, Message::TakePhoto(true)).into(), push("Use Photo", true, Message::Picture(crate::users::photo_file())).into()],
            ),
            Shot::Failed(why) => (text(why).size(12).color(crate::style::rgb(0xFF3B30)).into(), vec![push("Retry", false, Message::TakePhoto(true)).into()]),
        };
        let mut buttons = buttons;
        buttons.insert(0, space().width(Length::Fill).into());
        buttons.insert(1, push("Cancel", false, Message::TakePhoto(false)).into());
        let card = column![text("Take Photo").size(15).font(font(Weight::Semibold)), container(shown).center_x(Length::Fill), row(buttons).spacing(8)]
            .spacing(12)
            .width(320);
        let sheet = container(card).padding(18).style(t.sheet());
        return Some(iced::widget::opaque(container(sheet).center(Length::Fill).style(t.dim())));
    }
    if let Some(path) = app.texts.get(crate::users::DELETE) {
        let a = app.users.as_ref()?.others.iter().find(|a| &a.path == path)?;
        let card = column![
            text(format!("Delete the user “{}”?", a.shown())).size(15).font(font(Weight::Semibold)),
            text(format!("{} can't sign in after this. Their home folder can stay, or go with the account.", a.login)).size(12).color(t.secondary()),
            row![
                space().width(Length::Fill),
                push("Cancel", false, Message::DeleteUser(None)),
                push("Keep Home Folder", true, Message::DeleteUserNow(false)),
                push("Delete Home Folder", false, Message::DeleteUserNow(true)),
            ]
            .spacing(8),
        ]
        .spacing(10)
        .width(420);
        let sheet = container(card).padding(18).style(t.sheet());
        return Some(iced::widget::opaque(container(sheet).center(Length::Fill).style(t.dim())));
    }
    let value = |key: &str| app.texts.get(key).map(String::as_str);
    let full = value(NEW[0])?;
    let suggested = crate::users::suggest(full);
    let field = |key: &'static str, placeholder: &str, secure: bool| -> Element<'_, Message> {
        text_input(placeholder, value(key).unwrap_or_default())
            .secure(secure)
            .on_input(move |v| Message::Type(key.into(), v))
            .on_submit(Message::AddUser)
            .size(13)
            .padding([3, 8])
            .width(200)
            .style(t.input(false))
            .into()
    };
    let admin = value(NEW[4]).is_some_and(|v| !v.is_empty());
    let kind = segments(
        t,
        vec![
            ("Standard".into(), !admin, Message::Type(NEW[4].into(), String::new())),
            ("Administrator".into(), admin, Message::Type(NEW[4].into(), "admin".into())),
        ],
    );
    let mut card = column![
        text("Add User").size(15).font(font(Weight::Semibold)),
        line(t, "Full name", "", field(NEW[0], "", false)),
        line(t, "Account name", "Can't be changed later", field(NEW[1], &suggested, false)),
        line(t, "Password", "", field(NEW[2], "", true)),
        line(t, "Verify", "", field(NEW[3], "", true)),
        line(t, "Account type", "Administrators can change the system", kind),
    ]
    .spacing(2)
    .width(420);
    if let Some(error) = &app.error {
        card = card.push(container(text(error).size(11).color(rgb(0xFF3B30))).padding([4, 12]));
    }
    let buttons = row![
        space().width(Length::Fill),
        button(text("Cancel").size(13)).padding([4, 14]).style(t.push(false)).on_press(Message::AddUserForm),
        button(text("Add User").size(13)).padding([4, 14]).style(t.push(true)).on_press(Message::AddUser),
    ]
    .spacing(8);
    let sheet = container(card.push(buttons)).padding(18).style(t.sheet());
    Some(iced::widget::opaque(container(sheet).center(Length::Fill).style(t.dim())))
}

/// An account's picture as a circle, or the person symbol without one.
fn picture<'a>(t: Tokens, account: &crate::users::Account, size: f32) -> Element<'a, Message> {
    match &account.picture {
        Some(handle) => iced::widget::image(handle.clone()).width(size).height(size).border_radius(size / 2.0).into(),
        None => svg(icon("user-circle")).width(size).height(size).style(Tokens::tint(t.secondary())).into(),
    }
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
        Page::Notifications => {
            let mut parts = notifying_apps(app);
            parts.extend(widget_editors(app));
            parts
        }
        Page::Dock => vec![default_apps(app)],
        Page::General => {
            let clear = button(text("Clear").size(12)).padding([3, 10]).style(t.push(false)).on_press(Message::ClearRecent);
            vec![login_items(app), group(t, "Privacy", vec![line(t, "Recent files", "Every app's recent documents list", clear.into())])]
        }
        _ => Vec::new(),
    }
}

/// General ▸ Software Update's rows above the settings (the desktop's and noobsbot-terminal's versions, Update Now,
/// More Info…) and below them (Last checked with Check Now, Revert to). The desktop checks; this only shows its
/// `update.json`.
fn update_rows(app: &App) -> (Vec<Element<'_, Message>>, Vec<Element<'_, Message>>) {
    let t = app.tokens;
    let u = &app.update;
    let push = |label: &'static str, message: Option<Message>| button(text(label).size(12)).padding([3, 10]).style(t.push(false)).on_press_maybe(message);
    // A version row: its name, status, the newer release's size and date, More Info… and `update`.
    let version_row = |name: String, status: String, info: String, url: &Option<String>, update: Option<Element<'static, Message>>| {
        let mut controls = row![].spacing(8).align_y(Alignment::Center);
        if let Some(url) = url {
            controls = controls.push(push("More Info…", Some(Message::OpenUrl(url.clone()))));
        }
        controls = controls.extend(update);
        let mut words = column![text(name).size(13)].spacing(2);
        for line in [status, info].into_iter().filter(|l| !l.is_empty()) {
            words = words.push(text(line).size(11).color(t.secondary()));
        }
        Element::from(row![words.width(Length::Fill), controls].spacing(12).align_y(Alignment::Center).padding([8, 12]))
    };
    let status = match (&u.available, u.ready) {
        (Some(v), true) => format!("{v} is available, downloaded and verified"),
        (Some(v), false) => format!("{v} is available; install it with ./install.sh --update or your package manager"),
        (None, _) if u.checked > 0 => "Up to date".to_owned(),
        (None, _) => String::new(),
    };
    let update = u.ready.then(|| button(text("Update Now").size(12)).padding([3, 10]).style(t.push(true)).on_press(Message::UpdateCall("Install")).into());
    let info = u.available.as_ref().map(|_| release_info(u.size, u.published.as_deref())).unwrap_or_default();
    let mut before = vec![version_row(format!("Noobsbot Desktop {}", version()), status, info, &u.url, update)];
    // Its own installer or package updates it: only shown, with its release page.
    if let Some(term) = &u.terminal {
        let status = match &term.available {
            Some(v) => format!("{v} is available; install it with its installer or your package manager"),
            None => "Up to date".to_owned(),
        };
        let info = term.available.as_ref().map(|_| release_info(term.size, term.published.as_deref())).unwrap_or_default();
        before.push(version_row(format!("Noobsbot Terminal {}", term.installed), status, info, &term.url, None));
    }
    let check = push(if app.checking { "Checking…" } else { "Check Now" }, (!app.checking).then_some(Message::UpdateCall("Check")));
    let mut after = vec![line(
        t,
        "Last checked",
        "",
        row![text(checked(u.checked, chrono::Local::now())).size(13).color(t.secondary()), check].spacing(12).align_y(Alignment::Center).into(),
    )];
    if let Some(v) = &u.revert {
        let revert = push("Revert", Some(Message::UpdateCall("Revert")));
        after.push(line(t, format!("Revert to {v}"), "The version before the last update, at next login", revert.into()));
    }
    (before, after)
}

/// "12.3 MB · 27 Oct 2026": a release's download size and date, either left out when unknown.
fn release_info(size: Option<u64>, published: Option<&str>) -> String {
    let size = size.map(|bytes| format!("{:.1} MB", bytes as f64 / 1e6));
    let date = published.and_then(|p| chrono::DateTime::parse_from_rfc3339(p).ok()).map(|d| d.with_timezone(&chrono::Local).format("%-d %b %Y").to_string());
    size.into_iter().chain(date).collect::<Vec<_>>().join(" · ")
}

/// "Last checked": Never, Today, 9:41, Yesterday, 9:41, or 12 Oct 2026.
fn checked(unix: i64, now: chrono::DateTime<chrono::Local>) -> String {
    let Some(at) = chrono::DateTime::from_timestamp(unix, 0).filter(|_| unix > 0).map(|at| at.with_timezone(&chrono::Local)) else {
        return "Never".into();
    };
    let days = (now.date_naive() - at.date_naive()).num_days();
    match days {
        0 => at.format("Today, %-H:%M").to_string(),
        1 => at.format("Yesterday, %-H:%M").to_string(),
        _ => at.format("%-d %b %Y").to_string(),
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
        let moved = |to: usize| Message::Sources(crate::xkb::moved(&sources, i, to));
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
    // The engine's methods, switched with its own key.
    for (id, name) in app.methods.iter().flat_map(|m| &m.chosen) {
        let remove = container(small("−", Some(Message::Method(id.clone(), false)))).padding(2).style(t.field());
        let kind = text("Input method").size(12).color(t.secondary());
        rows.push(line(t, name.as_str(), "", row![kind, remove].spacing(8).align_y(Alignment::Center).into()));
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
        match &app.methods {
            Some(methods) => {
                for (id, name) in methods.offered.iter().filter(|(id, name)| query.is_empty() || name.to_lowercase().contains(&query) || *id == query).take(40)
                {
                    let label = row![text(name).size(13), space().width(Length::Fill), text("Input method").size(12).color(t.secondary())];
                    picks.push(
                        button(label).padding([6, 12]).width(Length::Fill).style(t.sidebar_row(false)).on_press(Message::Method(id.clone(), true)).into(),
                    );
                }
            }
            // ponytail: fcitx5's own files aren't edited; its controller is the only way in.
            None => match crate::ibus::engine(app.config.str("keyboard.input_method")) {
                "fcitx5" if crate::ibus::on_path("fcitx5") => picks.push(fact(t, "Start fcitx5 to add its input methods (Pinyin, Mozc…) here", String::new())),
                "ibus" if crate::ibus::on_path("ibus") => picks.push(fact(t, "Start ibus to add its input methods (Pinyin, Anthy…) here", String::new())),
                "ibus" => picks.push(fact(t, "Install the ibus package to use its input methods", String::new())),
                _ => {}
            },
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

/// Typed degrees: `27.7`, `-85.3°` or `−85.3`; `None` when not a number within ±`limit`.
pub fn parse_degrees(typed: &str, limit: f64) -> Option<f64> {
    let value: f64 = typed.trim().trim_end_matches('°').replace('−', "-").parse().ok()?;
    (value.is_finite() && value.abs() <= limit).then_some(value)
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
    // Mirrors show another display's picture: they have no place of their own.
    let on: Vec<&Display> = app.displays.iter().filter(|d| d.enabled && d.mirror.is_empty()).collect();
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
    if let Some(level) = d.brightness {
        let value = app.drafts.get("display.brightness").copied().unwrap_or(f64::from(level));
        rows.push(line(t, "Brightness", "", slider_row(t, "display.brightness", 1.0..=100.0, 1.0, value)));
    }
    if !d.nested {
        rows.push(line(t, "Use this display", "", toggle(t, d.enabled, move |on| change(vec![("enabled", on.into())]))));
    }
    // Extended or a mirror of another display that is itself extended (no chains); a display others mirror stays.
    let sources: Vec<&Display> = app.displays.iter().filter(|o| o.name != d.name && o.enabled && o.mirror.is_empty()).collect();
    if d.enabled && !sources.is_empty() && !app.displays.iter().any(|o| o.mirror == d.name) {
        let mut items = vec![("Extended".to_string(), d.mirror.is_empty(), change(vec![("mirror", "".into())]))];
        items.extend(
            sources.iter().map(|o| (format!("Mirror for {}", display_name(&o.name)), d.mirror == o.name, change(vec![("mirror", o.name.clone().into())]))),
        );
        rows.push(line(t, "Use as", "", segments(t, items)));
    }
    if d.enabled && d.mirror.is_empty() && app.displays.len() > 1 {
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
        if d.vrr_capable {
            rows.push(line(t, "Variable refresh rate", "", toggle(t, d.vrr, move |on| change(vec![("vrr", on.into())]))));
        }
        // TVs crop the picture's edges (overscan); they come on HDMI.
        if d.name.starts_with("HDMI-") {
            let key = format!("display.underscan.{}", d.name);
            let value = app.drafts.get(&key).copied().unwrap_or(f64::from(d.underscan));
            let slide = key.clone();
            let bar =
                slider(0.0..=10.0, value, move |v| Message::Slide(slide.clone(), v)).step(1.0).on_release(Message::Release(key)).width(150).style(t.slider());
            let shown = text(format!("{value:.0}%")).size(12).color(t.secondary()).width(48).align_x(iced::alignment::Horizontal::Right);
            rows.push(line(t, "Underscan", "Shrinks the picture when a TV cuts off its edges", row![bar, shown].spacing(8).align_y(Alignment::Center).into()));
        }
    }
    // The entry's own scale; 0 or none = the default ([display] scale, else auto). A mirror is drawn in its own pixels.
    if d.mirror.is_empty() {
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
    }
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
pub fn version() -> &'static str {
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
    fn last_checked_reads_like_a_day() {
        use chrono::TimeZone;
        let now = chrono::Local.with_ymd_and_hms(2026, 10, 27, 15, 0, 0).unwrap();
        let at = |d, h, m| chrono::Local.with_ymd_and_hms(2026, 10, d, h, m, 0).unwrap().timestamp();
        assert_eq!(checked(0, now), "Never");
        assert_eq!(checked(at(27, 9, 41), now), "Today, 9:41");
        assert_eq!(checked(at(26, 23, 5), now), "Yesterday, 23:05");
        assert_eq!(checked(at(12, 9, 41), now), "12 Oct 2026");
    }

    #[test]
    fn release_size_and_date() {
        assert_eq!(release_info(Some(12_345_678), Some("2026-10-27T12:00:00Z")), "12.3 MB · 27 Oct 2026");
        assert_eq!(release_info(None, Some("2026-10-27T12:00:00Z")), "27 Oct 2026");
        assert_eq!(release_info(Some(900_000), Some("garbage")), "0.9 MB");
        assert_eq!(release_info(None, None), "");
    }

    #[test]
    fn widget_lists_toggle_move_and_keep_unknown_ids() {
        let live: Vec<String> = ["ci", "music", "mail"].map(String::from).into();
        let w = Widgets { center: vec!["gone".into(), "mail".into()], desktop: vec!["music".into()], hidden: vec![] };
        let ids = |rows: Vec<(String, bool)>| rows.into_iter().map(|(id, on)| format!("{id}{}", if on { "" } else { "-" })).collect::<Vec<_>>();
        assert_eq!(ids(w.rows(&live, false)), ["gone", "mail", "ci", "music-"]);
        assert_eq!(ids(w.rows(&live, true)), ["music", "ci-", "gone-", "mail-"]);
        // Off in Notification Center: hidden, so it doesn't come back as unlisted.
        let off = w.toggled(&live, false, "ci", false);
        assert_eq!((off.center.clone(), off.hidden.clone()), (vec!["gone".into(), "mail".into()], vec!["ci".into()]));
        assert_eq!(ids(off.rows(&live, false)), ["gone", "mail", "ci-", "music-"]);
        // On again: last, and no longer hidden.
        let on = off.toggled(&live, false, "ci", true);
        assert_eq!((on.center, on.hidden), (vec!["gone".into(), "mail".into(), "ci".into()], vec![]));
        // Unknown "gone" keeps its place as others move around it.
        let moved = w.moved(&live, false, "ci", -1);
        assert_eq!(moved.center, ["gone", "ci", "mail"]);
        assert_eq!(moved.moved(&live, false, "gone", -1), moved, "the first goes nowhere");
        assert_eq!(w.moved(&live, false, "ci", 1).center, ["gone", "mail", "ci"], "the last goes nowhere");
        // The desktop: on appends, off just removes (Notification Center takes it back).
        let desk = w.toggled(&live, true, "ci", true);
        assert_eq!((desk.desktop.clone(), desk.hidden.clone()), (vec!["music".into(), "ci".into()], vec![]));
        assert_eq!(desk.moved(&live, true, "ci", -1).desktop, ["ci", "music"]);
        assert_eq!(w.toggled(&live, true, "music", false).desktop, Vec::<String>::new());
        assert_eq!(ids(w.toggled(&live, true, "music", false).rows(&live, false)), ["gone", "mail", "ci", "music"]);
    }

    #[test]
    fn widget_lists_round_trip() {
        let dir = std::env::temp_dir().join(format!("noobsbot-settings-widgets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "[widgets]\ncenter = [\"gone\", \"mail\"]\n").unwrap();
        let (mut config, _) = crate::config::Config::load(path.clone());
        let w = Widgets::read(&config).toggled(&["ci".into()], false, "ci", false);
        config.set_all(w.values()).unwrap();
        assert_eq!(Widgets::read(&crate::config::Config::load(path).0), w);
        assert_eq!(w, Widgets { center: vec!["gone".into(), "mail".into()], desktop: vec![], hidden: vec!["ci".into()] });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn app_rules_read_and_write_both_forms() {
        let doc: toml_edit::DocumentMut = "a = \"alert\"\nb = { style = \"none\", badge = false }\nc = { lock = true }\n".parse().unwrap();
        let a = AppRule::read(doc.get("a").and_then(toml_edit::Item::as_value));
        assert_eq!(a, AppRule { style: "alert".into(), sound: true, badge: true, center: true, lock: false });
        assert_eq!(a.value().as_str(), Some("alert"), "every switch on: the style alone");
        let b = AppRule::read(doc.get("b").and_then(toml_edit::Item::as_value));
        assert_eq!(b, AppRule { style: "none".into(), sound: true, badge: false, center: true, lock: false });
        assert_eq!(AppRule::read(Some(&b.value())), b);
        let c = AppRule::read(doc.get("c").and_then(toml_edit::Item::as_value));
        assert!(c.lock && c.value().is_inline_table(), "lock on: a table");
        assert_eq!(AppRule::read(Some(&c.value())), c);
        assert_eq!(AppRule::read(None).style, "banner");
    }

    #[test]
    fn degrees_parse_within_range() {
        assert_eq!(parse_degrees(" 27.7 ", 90.0), Some(27.7));
        assert_eq!(parse_degrees("−85.3°", 180.0), Some(-85.3));
        assert_eq!(parse_degrees("-180", 180.0), Some(-180.0));
        assert_eq!(parse_degrees("91", 90.0), None);
        assert_eq!(parse_degrees("NaN", 90.0), None);
        assert_eq!(parse_degrees("north", 90.0), None);
        assert_eq!(parse_degrees("", 90.0), None);
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
        assert_eq!(Page::parse("network"), Some(Page::Network));
        assert_eq!(Page::parse("wi-fi"), Some(Page::Network));
        assert_eq!(Page::parse("wifi"), Some(Page::Network));
        assert_eq!(Page::parse("battery"), Some(Page::Battery));
        assert_eq!(Page::parse("privacy-security"), Some(Page::Privacy));
        assert_eq!(Page::parse("users"), Some(Page::Users));
        assert_eq!(Page::parse("printers"), Some(Page::Printers));
        assert_eq!(Page::parse("nope"), None);
    }

    #[test]
    fn users_sit_between_general_and_about() {
        let at = |p| Page::ALL.iter().position(|&q| q == p).unwrap();
        assert_eq!((at(Page::General) + 1, at(Page::Users) + 1), (at(Page::Users), at(Page::About)));
        assert_eq!(Page::ALL.last(), Some(&Page::About));
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
