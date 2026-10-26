//! The pages: every config setting once, in a table (`SETTINGS`) that both the pages and Search read, plus the
//! system pages' own rows. Grouped card rows, label left and control right (E7.1).

use std::{collections::HashMap, sync::LazyLock};

use iced::{
    Alignment, Color, Element, Length,
    font::Weight,
    widget::{button, canvas, column, container, pick_list, row, slider, space, svg, text, text_input, toggler},
};

use crate::{App, Message, arrange::Arrangement, display_info::Display, font, style::Tokens};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Wifi,
    Bluetooth,
    Sound,
    Displays,
    Appearance,
    Dock,
    Keyboard,
    Trackpad,
    DateTime,
    Spaces,
    Notifications,
    LockScreen,
    General,
    About,
}

impl Page {
    pub const ALL: [Page; 14] = [
        Page::Wifi,
        Page::Bluetooth,
        Page::Sound,
        Page::Displays,
        Page::Appearance,
        Page::Dock,
        Page::Keyboard,
        Page::Trackpad,
        Page::DateTime,
        Page::Spaces,
        Page::Notifications,
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
            Page::Dock => "Desktop & Dock",
            Page::Keyboard => "Keyboard",
            Page::Trackpad => "Trackpad",
            Page::DateTime => "Date & Time",
            Page::Spaces => "Spaces",
            Page::Notifications => "Notifications",
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
        matches!(self, Page::Wifi | Page::Bluetooth | Page::Sound)
    }

    /// A little space starts a new block in the sidebar, as in macOS.
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
            Page::Dock => ("app-window", 0x3A3A3C),
            Page::Keyboard => ("keyboard", 0x8E8E93),
            Page::Trackpad => ("cursor-click", 0x8E8E93),
            Page::DateTime => ("clock", 0x007AFF),
            Page::Spaces => ("squares-four", 0x5856D6),
            Page::Notifications => ("bell", 0xFF3B30),
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
    "bluetooth",
    "speaker-high",
    "monitor",
    "circle-half",
    "app-window",
    "keyboard",
    "cursor-click",
    "clock",
    "squares-four",
    "bell",
    "lock-simple",
    "gear-six",
    "info",
    "magnifying-glass",
    "user-circle",
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

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Toggle,
    /// Value written, label shown.
    Choice(&'static [(&'static str, &'static str)]),
    Slider {
        min: f64,
        max: f64,
        step: f64,
    },
    Text(&'static str),
    Accent,
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
    s(
        Page::Appearance,
        "",
        "appearance.mode",
        "Appearance",
        "Auto follows the schedule (light until it is set)",
        Kind::Choice(&[("light", "Light"), ("dark", "Dark"), ("auto", "Auto")]),
    ),
    s(Page::Appearance, "", "appearance.accent", "Accent colour", "", Kind::Accent),
    s(Page::Appearance, "", "appearance.glass_opacity", "Glass opacity", "100% = solid (Reduce transparency)", Kind::Slider { min: 0.5, max: 1.0, step: 0.05 }),
    s(Page::Appearance, "Wallpaper", "appearance.wallpaper", "Light wallpaper", "PNG or JPEG; empty = opal gradient", Kind::Text("Built-in opal")),
    s(Page::Appearance, "Wallpaper", "appearance.wallpaper_dark", "Dark wallpaper", "Used in dark mode", Kind::Text("Built-in dark opal")),
    s(Page::Appearance, "Apps", "appearance.theme_apps", "Theme Qt and KDE apps", "Dark Mode also rewrites the Noobsbot Glass theme files", Kind::Toggle),
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
    s(
        Page::Dock,
        "Windows",
        "windows.move_modifier",
        "Drag windows with",
        "Hold the key and drag anywhere in a window",
        Kind::Choice(&[("super", "Super"), ("alt", "Alt")]),
    ),
    s(Page::Dock, "Windows", "windows.magnetic_edges", "Magnetic edges", "Edges stick to screen and window edges", Kind::Toggle),
    s(Page::Dock, "Windows", "windows.snapping", "Snap to halves and quarters", "Hold a window at a screen edge", Kind::Toggle),
    s(
        Page::Dock,
        "Windows",
        "windows.double_click_title",
        "Double-click a title bar to",
        "",
        Kind::Choice(&[("zoom", "Zoom"), ("minimise", "Minimise"), ("nothing", "Nothing")]),
    ),
    s(Page::Keyboard, "Input", "keyboard.layout", "Layouts", "Like us,np; empty = the system's", Kind::Text("System")),
    s(Page::Keyboard, "Input", "keyboard.variant", "Variants", "", Kind::Text("None")),
    s(Page::Keyboard, "Input", "keyboard.options", "Options", "Like caps:escape", Kind::Text("None")),
    s(Page::Keyboard, "Input", "keyboard.mac_keys", "Super acts as Cmd", "Super+C, V, X, Z … reach apps as Ctrl+…", Kind::Toggle),
    s(Page::Trackpad, "", "touchpad.tap_to_click", "Tap to click", "", Kind::Toggle),
    s(Page::Trackpad, "", "touchpad.natural_scroll", "Natural scrolling", "Content follows your fingers", Kind::Toggle),
    s(Page::DateTime, "", "clock.timezone", "Time zone", "Like Asia/Kathmandu; empty = the system's", Kind::Text("System")),
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
        Kind::Choice(&[("sunday", "Sunday"), ("monday", "Monday"), ("saturday", "Saturday")]),
    ),
    s(Page::Spaces, "", "windows.spaces", "Spaces", "Dynamic: + in Mission Control adds more", Kind::Slider { min: 0.0, max: 16.0, step: 1.0 }),
    s(Page::Notifications, "", "notifications.dnd", "Do Not Disturb", "Banners stay quiet; Notification Center still collects them", Kind::Toggle),
    s(Page::Notifications, "Weather", "weather.enabled", "Weather widget", "Forecast from Open-Meteo every 30 minutes; sends only the place", Kind::Toggle),
    s(Page::Notifications, "Weather", "weather.city", "City", "Looked up once; never located by IP", Kind::Text("Kathmandu")),
    s(Page::Notifications, "Weather", "weather.units", "Units", "", Kind::Choice(&[("celsius", "°C"), ("fahrenheit", "°F")])),
    s(Page::LockScreen, "", "idle.dim", "Dim the display when inactive", "", Kind::Slider { min: 0.0, max: 600.0, step: 30.0 }),
    s(Page::LockScreen, "", "idle.screen_off", "Turn display off when inactive", "", Kind::Slider { min: 0.0, max: 3600.0, step: 60.0 }),
    s(Page::LockScreen, "", "idle.lock", "Lock when the display turns off", "It always locks before sleeping", Kind::Toggle),
    s(Page::LockScreen, "Sleep", "idle.sleep_on_battery", "On battery, sleep after", "", Kind::Slider { min: 0.0, max: 3600.0, step: 60.0 }),
    s(Page::LockScreen, "Sleep", "idle.sleep_on_ac", "On power adapter, sleep after", "", Kind::Slider { min: 0.0, max: 7200.0, step: 300.0 }),
    s(Page::General, "", "terminal", "Terminal", "Opened by Super+Return", Kind::Text("noobsbot")),
    s(Page::General, "Software Update", "check_updates", "Check for updates daily", "Sends nothing but the version; installs at next login", Kind::Toggle),
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
        "dock.icon_size" => format!("{value:.0} px"),
        "windows.spaces" if whole(key, value) == 0 => "Dynamic".into(),
        "sound.output" | "sound.input" => format!("{value:.0}%"),
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
        Kind::Choice(options) => {
            let current = app.config.str(key);
            let mut segments = row![].spacing(2);
            for &(value, label) in options {
                let selected = current == value;
                let segment =
                    button(text(label).size(12).center()).padding([2, 10]).height(22).style(t.segment(selected)).on_press(Message::Choose(key, value));
                segments = segments.push(segment);
            }
            container(segments).padding(2).style(t.field()).into()
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
                    button(container(mark).center(18)).padding(0).width(18).height(18).style(t.swatch(color, selected)).on_press(Message::Choose(key, name)),
                );
            }
            swatches.into()
        }
    }
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
            Some(wifi) => {
                let network = wifi.connection.clone().unwrap_or_else(|| "Not connected".into());
                vec![group(t, "", vec![line(t, "Wi-Fi", "", toggle(t, wifi.enabled, Message::Wifi)), fact(t, "Network", network)])]
            }
            None => missing("NetworkManager isn't running"),
        },
        Page::Bluetooth => match &status.bluetooth {
            Some(bt) => {
                let mut parts = vec![group(t, "", vec![line(t, "Bluetooth", "", toggle(t, bt.powered, Message::Bluetooth))])];
                if bt.powered && !bt.devices.is_empty() {
                    let rows = bt.devices.iter().map(|(name, on)| fact(t, name, if *on { "Connected" } else { "Not connected" }.into())).collect();
                    parts.push(group(t, "My devices", rows));
                }
                parts
            }
            None => missing("No Bluetooth adapter"),
        },
        Page::Sound => {
            let mut parts = Vec::new();
            for (output, header, device) in [(true, "Output", &status.output), (false, "Input", &status.input)] {
                let Some(device) = device else {
                    parts.push(group(t, header, vec![fact(t, if output { "No output device" } else { "No microphone" }, String::new())]));
                    continue;
                };
                let key = if output { "sound.output" } else { "sound.input" };
                let value = app.drafts.get(key).copied().unwrap_or(f64::from(device.volume.min(100)));
                let rows = vec![
                    fact(t, "Device", device.name.clone()),
                    line(t, if output { "Volume" } else { "Input level" }, "", slider_row(t, key, 0.0..=100.0, 1.0, value)),
                    line(t, "Mute", "", toggle(t, device.muted, move |m| Message::Mute(output, m))),
                ];
                parts.push(group(t, header, rows));
            }
            parts
        }
        Page::Displays => displays_part(app),
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
        Page::Notifications => vec![note(t, "Per-app banners and the Do Not Disturb schedule come here.")],
        Page::LockScreen => vec![note(t, "Videos and calls keep the display awake.")],
        _ => Vec::new(),
    }
}

/// Rows below the config settings.
fn after_part<'a>(app: &'a App, page: Page) -> Vec<Element<'a, Message>> {
    let t = app.tokens;
    match page {
        Page::Keyboard => {
            let rows = app
                .config
                .bindings()
                .into_iter()
                .map(|(keys, action)| {
                    let key = format!("bindings.{keys}");
                    let value = app.texts.get(&key).cloned().unwrap_or(action);
                    let field = text_input("none", &value)
                        .on_input({
                            let key = key.clone();
                            move |v| Message::Type(key.clone(), v)
                        })
                        .on_submit(Message::Submit(key))
                        .size(12)
                        .padding([3, 8])
                        .width(220)
                        .style(t.input(false));
                    line(t, keys.replace('+', " + "), "", field.into())
                })
                .collect();
            vec![group(t, "Shortcuts", rows), note(t, "Type an action and press Return; “none” frees the key.")]
        }
        Page::General => vec![group(t, "", vec![fact(t, "Noobsbot Desktop", format!("Version {}", version()))])],
        _ => Vec::new(),
    }
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

/// The scale presets (the common desktop and macOS steps).
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
    fn spaces_skip_one() {
        assert_eq!(whole("windows.spaces", 1.0), 0);
        assert_eq!(whole("windows.spaces", 4.2), 4);
        assert_eq!(whole("dock.icon_size", 1.0), 1);
        assert_eq!(shown("windows.spaces", 0.0), "Dynamic");
        assert_eq!(shown("appearance.glass_opacity", 0.85), "85%");
    }
}
