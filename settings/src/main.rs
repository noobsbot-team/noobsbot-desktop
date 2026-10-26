//! Noobsbot Desktop System Settings (E7.1): the one separate program. It edits `config.toml`, which the desktop
//! reloads the moment it is saved, and asks the system services (NetworkManager, BlueZ, PipeWire) for the rest.

mod arrange;
mod autostart;
mod config;
mod defaults;
#[path = "../../src/display_info.rs"]
mod display_info;
mod gallery;
mod network;
#[path = "../../src/pactl.rs"]
mod pactl;
mod pages;
mod shortcuts;
mod style;
mod system;
mod xkb;

use std::{collections::HashMap, sync::OnceLock, time::Instant};

use iced::{
    Alignment, Element, Font, Length, Task,
    font::Weight,
    widget::{button, column, container, row, scrollable, space, stack, svg, text, text_input},
};

use pages::Page;
use style::Tokens;

fn main() -> iced::Result {
    let window = iced::window::Settings {
        size: iced::Size::new(720.0, 560.0),
        min_size: Some(iced::Size::new(620.0, 420.0)),
        platform_specific: iced::window::settings::PlatformSpecific { application_id: "noobsbot-desktop-settings".into(), ..Default::default() },
        ..Default::default()
    };
    iced::application(App::new, App::update, App::view)
        .title("System Settings")
        .theme(|app: &App| app.tokens.theme())
        .subscription(|app: &App| {
            let mut all = vec![iced::Subscription::run_with(config::path(), config::changes).map(|()| Message::Reload), iced::event::listen_with(type_ahead)];
            if let Some(path) = display_info::path() {
                all.push(iced::Subscription::run_with(path, config::changes).map(|()| Message::Displays));
            }
            // Only while "Keep these display settings?" counts down: no timer otherwise.
            if app.keep.is_some() {
                all.push(iced::Subscription::run_with(std::time::Duration::from_millis(500), arrange::ticks).map(|()| Message::Tick));
            }
            // Nearby devices: discovery, and a re-read every 3 s, only while the Bluetooth page shows.
            if app.page == Page::Bluetooth {
                all.push(iced::Subscription::run(system::discover).map(|()| Message::Rescan));
                all.push(iced::Subscription::run_with(std::time::Duration::from_secs(3), arrange::ticks).map(|()| Message::Rescan));
            }
            // The microphone's live level, only while Sound shows it.
            if app.page == Page::Sound && app.status.input.is_some() {
                all.push(iced::Subscription::run(system::level).map(Message::Level));
            }
            if app.shortcuts.as_ref().is_some_and(|e| e.recording.is_some()) {
                all.push(iced::event::listen_with(shortcuts::key_pressed));
            }
            iced::Subscription::batch(all)
        })
        .window(window)
        .default_font(font(Weight::Normal))
        .run()
}

/// Typing while no field has focus searches. The search field isn't focused up front: a focused field's
/// caret blinks, which woke the app every 500 ms for as long as its window was active.
fn type_ahead(event: iced::Event, status: iced::event::Status, _: iced::window::Id) -> Option<Message> {
    let iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { text: Some(text), modifiers, .. }) = event else { return None };
    let typed = status == iced::event::Status::Ignored && !(modifiers.control() || modifiers.alt() || modifiers.logo());
    (typed && !text.chars().any(char::is_control)).then(|| Message::TypeAhead(text.to_string()))
}

/// `displays.json`, or nothing when the desktop isn't running (or wrote something unreadable).
fn read_displays() -> Vec<display_info::Display> {
    display_info::path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|json| serde_json::from_str(&json).ok()).unwrap_or_default()
}

/// Inter (D4) in `weight`, or the system sans-serif when it isn't installed.
pub fn font(weight: Weight) -> Font {
    static FAMILY: OnceLock<Option<&'static str>> = OnceLock::new();
    let family = FAMILY.get_or_init(|| {
        let mut fonts = iced::advanced::graphics::text::font_system().write().ok()?;
        let db = fonts.raw().db();
        ["Inter", "Inter Variable"].into_iter().find(|name| db.faces().any(|face| face.families.iter().any(|(family, _)| family == name)))
    });
    let mut font = family.map_or(Font::DEFAULT, Font::with_name);
    font.weight = weight;
    font
}

pub struct App {
    config: config::Config,
    tokens: Tokens,
    page: Page,
    query: String,
    /// Slider positions while dragging, by key; written on release.
    drafts: HashMap<String, f64>,
    /// Text fields being typed in, by key; written on Enter.
    texts: HashMap<String, String>,
    status: system::Status,
    /// The last save or read failure, shown under the page.
    error: Option<String>,
    user: (String, String),
    /// `displays.json`: the connected displays as the desktop sees them (empty when it isn't running).
    displays: Vec<display_info::Display>,
    /// The display the Displays page shows, by connector name; `None`: the primary.
    selected: Option<String>,
    /// A display change waiting for "Keep Changes" before it reverts.
    keep: Option<arrange::Keep>,
    /// The built-in panel's brightness in percent; `None` without a backlight.
    brightness: Option<u8>,
    /// Appearance's wallpaper gallery, loaded on start.
    thumbs: Vec<(std::path::PathBuf, iced::widget::image::Handle)>,
    /// General ▸ Login Items, and the apps "+" offers; read when General opens.
    login_items: Vec<autostart::Item>,
    installed: Vec<autostart::Installed>,
    /// Desktop & Dock ▸ Default Apps, and the terminal emulators with their commands; read when the page opens.
    default_apps: defaults::Kinds,
    terminals: Vec<(autostart::Installed, String)>,
    /// Keyboard ▸ Input Sources' picker, open, with what is typed in its search field.
    picker: Option<String>,
    /// Apps in the notification history, read when Notifications opens.
    notifiers: Vec<String>,
    /// Notifications ▸ Applications: the app whose options are open.
    notify_app: Option<String>,
    /// Keyboard ▸ Keyboard Shortcuts…, open.
    shortcuts: Option<shortcuts::Editor>,
    /// Sound ▸ Input: the microphone's level, 0 … 1.
    level: f32,
}

#[derive(Clone, Debug)]
pub enum Message {
    Page(Page),
    Search(String),
    Toggle(&'static str, bool),
    Choose(String, &'static str),
    Number(&'static str, i64),
    /// General ▸ Privacy: empty every app's recent files.
    ClearRecent,
    Slide(String, f64),
    Release(String),
    Type(String, String),
    Submit(String),
    Status(system::Status),
    Wifi(bool),
    /// A network in range, by name: joins it, or opens its password field.
    Join(String),
    /// A saved connection's path: Auto-Join on or off.
    AutoJoin(String, bool),
    /// Known Networks ▸ Forget: a saved connection's path.
    ForgetNetwork(String),
    /// Opens `nm-connection-editor`.
    Advanced,
    Bluetooth(bool),
    /// A power-profiles-daemon profile.
    PowerMode(String),
    /// Output (`true`) or input muted.
    Mute(bool, bool),
    /// Date & Time ▸ Set time and date automatically.
    Ntp(bool),
    /// Date & Time ▸ Set: the typed date and time become the system's.
    SetTime,
    /// A device (BlueZ path): pair if need be and connect (`true`), or disconnect.
    Device(String, bool),
    /// A paired device is removed.
    Forget(String),
    /// A Bluetooth action finished: the new status and what went wrong.
    Acted((system::Status, Option<String>)),
    /// Re-read while looking for nearby devices.
    Rescan,
    /// A PipeWire sink or source becomes the default.
    DefaultDevice(u32),
    /// `config.toml` was written (here or elsewhere, e.g. the Control Center's Dark toggle).
    Reload,
    /// Typed with no field focused: goes to the search field.
    TypeAhead(String),
    /// `displays.json` was rewritten by the desktop.
    Displays,
    SelectDisplay(String),
    /// One display's `[[display.outputs]]` keys.
    Output(String, Vec<(&'static str, toml_edit::Value)>),
    /// Every display that is on, at its new place (dragged in the arrangement).
    Arrange(Vec<(String, i32, i32)>),
    /// The menu bar and Dock go to this display.
    Primary(String),
    Keep,
    Revert,
    /// Half a second of "Keep these display settings?" went by.
    Tick,
    /// Every display shows its name for 2 s.
    Identify,
    /// The arrangement goes back to automatic: the built-in display first, the others in a row right of it.
    ResetArrangement,
    /// A call to the desktop came back: what went wrong, if anything.
    Called(Option<String>),
    Thumbs(Vec<(std::path::PathBuf, iced::widget::image::Handle)>),
    /// A gallery picture (empty: the built-in opal), for light and dark mode alike.
    Wallpaper(String),
    /// A login item (by file name) on or off.
    LoginItem(String, bool),
    AddLoginItem(autostart::Installed),
    RemoveLoginItem(String),
    /// A Default Apps row (by index in `defaults::KINDS`) gets this app.
    DefaultApp(usize, autostart::Installed),
    /// Default Apps ▸ Terminal: this app's command becomes `terminal`.
    Terminal(autostart::Installed),
    /// The chosen input sources, as (layout, variant), in order.
    Sources(Vec<(String, String)>),
    /// Open (or filter) the input source picker; `None` closes it.
    Picker(Option<String>),
    /// `keyboard.options` rewritten by a Modifier Keys dropdown.
    Options(String),
    Shortcuts(shortcuts::Msg),
    /// Notifications ▸ Applications: open one app's options, or close them.
    NotifyApp(Option<String>),
    /// An app's new `[notification_apps]` rule.
    AppRule(String, pages::AppRule),
    /// The microphone's level, 0 … 1.
    Level(f32),
    /// A theme sound plays; `true`: it becomes the alert sound.
    Sound(&'static str, bool),
}

impl App {
    fn new() -> (App, Task<Message>) {
        let (config, error) = config::Config::load(config::path());
        let page = std::env::args().skip_while(|a| a != "--page").nth(1).and_then(|name| Page::parse(&name)).unwrap_or(Page::Appearance);
        let mut app = App {
            tokens: Tokens::new(false, "blue"),
            config,
            page,
            query: String::new(),
            drafts: HashMap::new(),
            texts: HashMap::new(),
            status: system::Status::default(),
            error,
            user: system::user(),
            displays: read_displays(),
            selected: None,
            keep: None,
            brightness: display_info::brightness(),
            thumbs: Vec::new(),
            login_items: Vec::new(),
            installed: Vec::new(),
            default_apps: Vec::new(),
            terminals: Vec::new(),
            picker: None,
            notifiers: config::notifying_apps(),
            notify_app: None,
            shortcuts: None,
            level: 0.0,
        };
        app.retheme();
        if page == Page::General {
            (app.login_items, app.installed) = autostart::load();
        }
        if page == Page::Dock {
            (app.default_apps, app.terminals) = defaults::load();
        }
        // Typing searches at once (see `type_ahead`).
        (app, Task::batch([Task::perform(system::read(), Message::Status), Task::perform(gallery::load(), Message::Thumbs)]))
    }

    fn retheme(&mut self) {
        // `auto` follows the hours the desktop uses (render::dark_at).
        let dark = match self.config.str("appearance.mode") {
            "dark" => true,
            "auto" => {
                let (from, until) = (self.config.num("appearance.dark_from") as u32 % 24, self.config.num("appearance.dark_until") as u32 % 24);
                let hour = chrono::Timelike::hour(&chrono::Local::now());
                if from <= until { (from..until).contains(&hour) } else { hour >= from || hour < until }
            }
            _ => false,
        };
        self.tokens = Tokens::new(dark, self.config.str("appearance.accent"));
    }

    fn save(&mut self, key: &str, value: impl Into<toml_edit::Value>) {
        self.error = self.config.set(key, value).err().map(|err| format!("Couldn't save {key}: {err}"));
        self.retheme();
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Page(page) => {
                self.page = page;
                self.query.clear();
                if page == Page::General {
                    (self.login_items, self.installed) = autostart::load();
                }
                if page == Page::Dock {
                    (self.default_apps, self.terminals) = defaults::load();
                }
                if page == Page::Notifications {
                    self.notifiers = config::notifying_apps();
                }
                if page.is_system() {
                    return Task::perform(system::read(), Message::Status);
                }
            }
            Message::Search(query) => self.query = query,
            Message::Toggle(key, on) => self.save(key, on),
            Message::Choose(key, value) => {
                self.texts.remove(&key);
                self.save(&key, value);
            }
            Message::Number(key, value) => self.save(key, value),
            Message::NotifyApp(app) => self.notify_app = app,
            Message::AppRule(app, rule) => self.save(&format!("notification_apps.{app}"), rule.value()),
            Message::ClearRecent => self.error = config::clear_recent_files().err().map(|err| format!("Couldn't clear recent files: {err}")),
            Message::Slide(key, value) => {
                self.drafts.insert(key, value);
            }
            Message::Release(key) => {
                let Some(value) = self.drafts.remove(&key) else { return Task::none() };
                match key.as_str() {
                    "sound.output" | "sound.input" => return Task::perform(system::set_volume(key == "sound.output", value.round() as u8), Message::Status),
                    "sound.balance" => return Task::perform(system::set_balance((value / 100.0) as f32), Message::Status),
                    "keyboard.backlight" => return Task::perform(system::set_kbd_backlight(value.round() as u8), Message::Status),
                    "display.brightness" => {
                        let percent = value.round() as u8;
                        self.brightness = Some(percent);
                        return Task::perform(system::displays_call("SetBrightness", percent), Message::Called);
                    }
                    _ => {}
                }
                match pages::setting(&key).map(|s| s.kind) {
                    Some(pages::Kind::Slider { step, .. }) if step >= 1.0 => self.save(&key, pages::whole(&key, value)),
                    _ => self.save(&key, (value * 100.0).round() / 100.0),
                }
            }
            Message::Type(key, value) => {
                self.texts.insert(key, value);
            }
            Message::Submit(key) => {
                let Some(value) = self.texts.remove(&key) else { return Task::none() };
                if let Some(ssid) = key.strip_prefix(network::PASSWORD) {
                    if value.is_empty() {
                        self.texts.insert(key.clone(), value);
                        return Task::none();
                    }
                    return self.join(ssid, Some(value));
                } else if let Some(name) = key.strip_prefix("display.custom_scale.") {
                    match pages::parse_scale(&value) {
                        Some(scale) => self.change_outputs(vec![(name.to_owned(), "scale", scale.into())], false),
                        None => self.error = Some(format!("“{}” isn't a scale from 100% to 300%", value.trim())),
                    }
                } else {
                    self.save(&key, value.trim());
                }
            }
            Message::Status(status) => self.status = status,
            Message::Level(level) => self.level = level,
            Message::Sound(name, save) => {
                if save {
                    self.save("sound.alert", name);
                }
                system::play(name);
            }
            Message::Thumbs(thumbs) => self.thumbs = thumbs,
            Message::Wallpaper(path) => {
                self.save("appearance.wallpaper", path.as_str());
                self.save("appearance.wallpaper_dark", path);
            }
            Message::Sources(sources) => {
                let (layouts, variants) = xkb::lists(&sources);
                self.picker = None;
                self.error = self
                    .config
                    .set_all([("keyboard.layout", layouts.into()), ("keyboard.variant", variants.into())])
                    .err()
                    .map(|err| format!("Couldn't save the input sources: {err}"));
            }
            Message::Picker(query) => self.picker = query,
            Message::Options(options) => self.save("keyboard.options", options),
            Message::Wifi(on) => return Task::perform(system::set_wifi(on), Message::Status),
            Message::Join(ssid) => return self.join(&ssid, None),
            Message::AutoJoin(path, on) => return Task::perform(network::change(move |bus| network::auto_join(bus, path, on)), Message::Status),
            Message::ForgetNetwork(path) => return Task::perform(network::change(move |bus| network::forget(bus, path)), Message::Status),
            Message::Advanced => network::advanced(),
            Message::Bluetooth(on) => return Task::perform(system::set_bluetooth(on), Message::Status),
            Message::PowerMode(profile) => return Task::perform(system::set_power_mode(profile), Message::Status),
            Message::Mute(output, muted) => return Task::perform(system::set_muted(output, muted), Message::Status),
            Message::Ntp(on) => return Task::perform(system::set_ntp(on), Message::Status),
            Message::SetTime => {
                let now = chrono::Local::now();
                let typed = |key: &str, now: String| self.texts.get(key).cloned().unwrap_or(now);
                let (date, clock) = (typed(system::DATE, now.format("%Y-%m-%d").to_string()), typed(system::CLOCK, now.format("%H:%M").to_string()));
                match system::manual_time(&date, &clock) {
                    Some(usec) => {
                        self.texts.remove(system::DATE);
                        self.texts.remove(system::CLOCK);
                        return Task::perform(system::set_time(usec), Message::Status);
                    }
                    None => self.error = Some(format!("“{} {}” isn't a date and time like 2026-10-27 14:05", date.trim(), clock.trim())),
                }
            }
            Message::Device(path, on) => return Task::perform(system::connect_device(path, on), Message::Acted),
            Message::Forget(path) => return Task::perform(system::forget(path), Message::Acted),
            Message::Acted((status, error)) => {
                self.status = status;
                self.error = error;
            }
            Message::Rescan => return Task::perform(system::read(), Message::Status),
            Message::DefaultDevice(id) => return Task::perform(system::set_default(id), Message::Status),
            Message::Reload => {
                // A half-written or broken file keeps what is shown, with the error under it.
                let (config, error) = config::Config::load(config::path());
                if error.is_none() {
                    self.config = config;
                    self.retheme();
                }
                self.error = error;
            }
            Message::Displays => self.displays = read_displays(),
            Message::SelectDisplay(name) => self.selected = Some(name),
            Message::Output(name, changes) => {
                // What could leave a screen dark or unusable asks to be kept.
                let risky = changes.iter().any(|(key, _)| ["mode", "refresh", "rotation", "enabled"].contains(key));
                self.change_outputs(changes.into_iter().map(|(key, value)| (name.clone(), key, value)).collect(), risky);
            }
            Message::Arrange(places) => {
                let changes = places
                    .into_iter()
                    .map(|(name, x, y)| (name, "position", toml_edit::Value::Array([i64::from(x), i64::from(y)].into_iter().collect())))
                    .collect();
                self.change_outputs(changes, true);
            }
            Message::Primary(name) => {
                let changes = self.displays.iter().map(|d| (d.name.clone(), "primary", (d.name == name).into())).collect();
                self.change_outputs(changes, false);
            }
            Message::LoginItem(file, on) => {
                let done = autostart::set_on(&autostart::user_dir(), &autostart::system_dirs(), &file, on);
                self.login_changed(done, &file);
            }
            Message::AddLoginItem(app) => {
                let done = autostart::add(&autostart::user_dir(), &app.path);
                self.login_changed(done, &app.name);
            }
            Message::RemoveLoginItem(file) => {
                let done = autostart::remove(&autostart::user_dir(), &file);
                self.login_changed(done, &file);
            }
            Message::DefaultApp(kind, app) => {
                let (label, mimes) = defaults::KINDS[kind];
                match defaults::set(mimes, &defaults::id(&app)) {
                    Ok(()) => self.default_apps = defaults::load().0,
                    Err(err) => self.error = Some(format!("Couldn't set the default {}: {err}", label.to_lowercase())),
                }
            }
            Message::Terminal(app) => {
                if let Some((_, command)) = self.terminals.iter().find(|(t, _)| *t == app) {
                    self.save("terminal", command.clone());
                }
            }
            Message::Identify => return Task::perform(system::displays_call("Identify", ()), Message::Called),
            Message::ResetArrangement => {
                if self.ask_keep() {
                    self.error = self.config.clear_outputs("position").err().map(|err| format!("Couldn't save the display settings: {err}"));
                }
            }
            Message::Called(error) => self.error = error,
            Message::Keep => self.keep(),
            Message::Revert => self.revert(),
            // Ran out: the desktop is putting the settings back; the sheet just goes.
            Message::Tick => {
                if self.keep.as_ref().is_some_and(|k| k.expired(Instant::now())) {
                    self.keep = None;
                }
            }
            Message::Shortcuts(msg) => return shortcuts::update(self, msg),
            // The shortcuts sheet takes the keys.
            Message::TypeAhead(_) if self.shortcuts.is_some() => {}
            Message::TypeAhead(text) => {
                self.query.push_str(&text);
                use iced::widget::operation::{focus, move_cursor_to_end};
                return Task::batch([focus("search"), move_cursor_to_end("search")]);
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let t = self.tokens;
        let content: Element<'_, Message> = if self.query.trim().is_empty() { pages::page(self, self.page) } else { pages::search(self, &self.query) };
        let mut body = column![content].spacing(16).padding(iced::Padding { top: 20.0, right: 24.0, bottom: 24.0, left: 24.0 }).max_width(640);
        if let Some(error) = &self.error {
            body = body.push(text(error).size(11).color(style::rgb(0xFF3B30)));
        }
        let content = scrollable(container(body).center_x(Length::Fill)).direction(style::thin()).style(t.scroll()).height(Length::Fill);
        let window = row![self.sidebar(), content].height(Length::Fill);
        match (&self.keep, &self.shortcuts) {
            (Some(keep), _) => stack![window, pages::keep_sheet(t, keep.left(Instant::now()))].into(),
            (None, Some(editor)) => stack![window, shortcuts::view(self, editor)].into(),
            (None, None) => window.into(),
        }
    }

    /// Joins a network in range; a new secured one without `password` opens its password field (or closes it).
    fn join(&mut self, ssid: &str, password: Option<String>) -> Task<Message> {
        let found = self.status.wifi.as_ref().and_then(|w| w.network.networks.iter().find(|n| n.ssid == ssid));
        let Some(ap) = found.cloned() else { return Task::none() };
        let key = format!("{}{ssid}", network::PASSWORD);
        if password.is_none() && ap.secure && ap.saved.is_none() {
            if self.texts.remove(&key).is_none() {
                self.texts.insert(key, String::new());
            }
            return Task::none();
        }
        self.texts.remove(&key);
        Task::perform(network::change(move |bus| network::join(bus, ap, password)), Message::Status)
    }

    /// The display the Displays page shows: the chosen one, else the primary, else the first.
    fn selected_display(&self) -> Option<&display_info::Display> {
        let chosen = self.selected.as_ref().and_then(|name| self.displays.iter().find(|d| &d.name == name));
        chosen.or_else(|| self.displays.iter().find(|d| d.primary)).or(self.displays.first())
    }

    /// Writes display settings; `ask`: then "Keep these display settings?" counts down, and the desktop puts
    /// `[display]` back as it was before the first unkept change (from `display.previous`, set aside here first so
    /// it does so even if we are gone by then).
    fn change_outputs(&mut self, changes: Vec<(String, &'static str, toml_edit::Value)>, ask: bool) {
        if ask && !self.ask_keep() {
            return;
        }
        match self.config.set_outputs(changes) {
            Ok(()) => self.error = None,
            Err(err) => self.error = Some(format!("Couldn't save the display settings: {err}")),
        }
    }

    /// Sets `[display]` aside and starts "Keep these display settings?"; `false` (and the error) if it can't.
    fn ask_keep(&mut self) -> bool {
        let previous = self.keep.take().map_or_else(|| self.config.display_text(), |k| k.previous);
        let set_aside = display_info::previous_path().ok_or_else(|| std::io::Error::other("no XDG_RUNTIME_DIR")).and_then(|p| std::fs::write(p, &previous));
        if let Err(err) = set_aside {
            self.error = Some(format!("Couldn't set the display settings aside: {err}"));
            return false;
        }
        self.keep = Some(arrange::Keep { previous, since: Instant::now() });
        true
    }

    /// After a Login Items change: the error if it failed, and the list read again.
    fn login_changed(&mut self, done: std::io::Result<()>, what: &str) {
        self.error = done.err().map(|err| format!("Couldn't change the login item {what}: {err}"));
        self.login_items = autostart::load().0;
    }

    /// "Keep Changes": the desktop has nothing to put back.
    fn keep(&mut self) {
        self.keep = None;
        let _ = display_info::previous_path().map(std::fs::remove_file);
    }

    /// "Revert": the display settings from before go back now (and the desktop's countdown has nothing left to do).
    fn revert(&mut self) {
        let Some(keep) = self.keep.take() else { return };
        self.error = self.config.restore_display(&keep.previous).err().map(|err| format!("Couldn't put the display settings back: {err}"));
        let _ = display_info::previous_path().map(std::fs::remove_file);
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let t = self.tokens;
        let search = container(
            row![
                svg(pages::icon("magnifying-glass")).width(13).height(13).style(Tokens::tint(t.secondary())),
                text_input("Search", &self.query).id("search").on_input(Message::Search).size(13).padding(0).style(t.input(true)),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .padding([4, 8])
        .height(26)
        .align_y(Alignment::Center)
        .style(t.field());
        let user = button(
            row![
                svg(pages::icon("user-circle")).width(32).height(32).style(Tokens::tint(t.secondary())),
                column![text(&self.user.1).size(13).font(font(Weight::Semibold)), text("Noobsbot Desktop account").size(11).color(t.secondary())],
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding([4, 6])
        .width(Length::Fill)
        .style(t.sidebar_row(false));
        let mut list = column![search, user, container(space()).height(1).width(Length::Fill).style(t.rule())].spacing(8);
        let mut rows = column![].spacing(1);
        for page in Page::ALL {
            if page.gap_before() {
                rows = rows.push(space().height(8));
            }
            let selected = self.page == page && self.query.trim().is_empty();
            let item = row![pages::squircle(page, 20.0), text(page.name()).size(13)].spacing(8).align_y(Alignment::Center);
            rows = rows.push(button(item).padding([4, 6]).height(28).width(Length::Fill).style(t.sidebar_row(selected)).on_press(Message::Page(page)));
        }
        list = list.push(scrollable(rows).direction(style::thin()).style(t.scroll()).height(Length::Fill));
        container(container(list).padding(10).width(220).height(Length::Fill).style(t.sidebar())).padding(8).into()
    }
}
