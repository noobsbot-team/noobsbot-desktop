//! Desktop shortcuts (E1.6). "Super" in a binding means the desktop key: Super in a real session, Alt when nested
//! (the host desktop keeps Super for itself). Super+C/V/X/Z/S/F/T/R/L are never bound, so they reach the app; Super+A
//! opens the app drawer (Mac keys mode, when it comes, frees it for select-all).

use smithay::{
    desktop::Window,
    input::keyboard::{Keysym, ModifiersState, xkb},
    reexports::wayland_server::Resource,
    utils::SERIAL_COUNTER,
};

use crate::{
    config::Config,
    services::{Command, media::Action as MediaAction},
    shell::{Osd, alert::Power},
    state::State,
    symbols::Symbol,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Spawn(String),
    /// Opens the default app for a MIME type (`x-scheme-handler/https`), or the first app in a category (`Calculator`).
    DefaultApp(String),
    /// Close every window of the focused app (Super+Q).
    QuitApp,
    CloseWindow,
    Minimise,
    /// Fill the work area, or back (Window ▸ Zoom; Alt+green).
    Zoom,
    /// Full screen in its own Space, or back (green light; Super+Ctrl+F).
    Fullscreen,
    /// Hide every window of the focused app (Super+H).
    Hide,
    /// Hide every other app's windows (Super+Alt+H).
    HideOthers,
    /// Bring back every hidden window.
    ShowAll,
    /// Lock the screen (Super+Ctrl+Q, logo menu ▸ Lock Screen; E6.4).
    Lock,
    /// Lock, then suspend the computer (logo menu ▸ Sleep).
    Sleep,
    /// Ask, then restart, shut down or log out (logo menu).
    Power(crate::shell::alert::Power),
    /// The app switcher (E4.5); `back`: Shift was held. `held` is the binding, whose modifiers keep it open.
    Switcher {
        back: bool,
        held: crate::shell::switcher::Held,
    },
    /// The Force Quit window (E4.7).
    ForceQuit,
    /// Shows or hides the quake terminal, starting `noobsbot --quake` the first time (E2.8).
    Quake,
    /// The default output's volume by one step; `true`: up (E2.7).
    Volume(bool),
    /// A quarter step (Shift+Alt+volume keys).
    VolumeFine(bool),
    /// Mutes or unmutes the default output.
    Mute,
    /// Mutes or unmutes the default microphone.
    MicMute,
    /// The display backlight by one step; `true`: up.
    Brightness(bool),
    /// The keyboard backlight one step up or down, or (`None`) off and on.
    KbdBacklight(Option<bool>),
    /// The trackpad on (`true`), off, or (`None`) the other way; until the config is next reloaded.
    Touchpad(Option<bool>),
    /// To the Now Playing player.
    Media(MediaAction),
    /// Ejects the last mounted removable drive.
    Eject,
    /// A row of the open tray menu, by dbusmenu id.
    TrayMenu(i32),
    /// Opens or closes Notification Center (Super+N).
    NotificationCenter,
    /// Opens or closes Spotlight (Super+Space, E4.3).
    Spotlight,
    /// Opens or closes the emoji picker (Super+Ctrl+E, E7.2).
    Emoji,
    /// Shows or hides the on-screen keyboard (E8.6).
    OnScreenKeyboard,
    /// A row of a dock icon's right-click menu (E3.1).
    Dock(crate::shell::dock::Command),
    /// The n-th dock icon, from 0 (Super+1…9, E8.3): focus, cycle or open it; `new`: a new window (Super+Shift+n).
    DockApp {
        n: usize,
        new: bool,
    },
    /// A row of a desktop icon's or the desktop's right-click menu (E7.2).
    Desktop(crate::shell::desktop::Command),
    /// Opens or closes Launchpad (E4.4; F4, off by default).
    Launchpad,
    /// Opens or closes the app drawer (Super+A, or a lone Super tap).
    AppDrawer,
    /// A row of an app's right-click menu in the app drawer.
    Drawer(crate::shell::drawer::Command),
    /// Installs the downloaded update, then logs out (logo menu / notification ▸ Restart to Update, E6.13).
    InstallUpdate,
    /// Orca on or off (Super+Alt+F5).
    ScreenReader,
    /// The Space to the left (-1) or right (+1) (E5.1).
    SwitchSpace(i32),
    /// Space n, from 0.
    ShowSpace(usize),
    /// The focused window comes along to the Space left or right.
    MoveToSpace(i32),
    /// Space n, from 0: the focused window goes there, the shown Space stays.
    SendToSpace(usize),
    /// The focused window goes to a new Space (dynamic Spaces).
    SendToNewSpace,
    /// The focused window to the next (+1) or previous (-1) display.
    MoveToDisplay(i32),
    /// Tiles the focused window against these work-area edges; none: Fill (E8.5, Super+Ctrl+arrows).
    Tile(crate::wm::Edges),
    /// Centres the focused window.
    Center,
    /// Keeps the focused window above the others, or not.
    KeepOnTop,
    /// The focused app's next window on this Space (Super+`, E4.5).
    CycleAppWindows,
    /// Opens or closes Mission Control (E5.2).
    MissionControl,
    /// Mission Control with only the focused app's windows.
    AppExpose,
    /// Windows slide to the screen edges, or back (E5.3; no default key).
    ShowDesktop,
    /// Keyboard layout n, from 0, or the next one (Super+Ctrl+Space, E6.12).
    Layout(Option<usize>),
    /// An fcitx5 input method, by unique name (the input menu).
    InputMethod(String),
    /// A row of the window menu (E8.5).
    WindowMenu(crate::decorations::Command),
    /// Screen Zoom on or off (None, Super+Alt+8), in (true, Super+Alt+=) or out (Super+Alt+-) (E8.6).
    ScreenZoom(Option<bool>),
    /// Starts or stops recording the screen (Super+Alt+5).
    ScreenRecord,
    /// Super+Shift+3 / 4 / 5 (E6.8).
    Screenshot(crate::shell::screenshot::Start),
}

/// A volume or brightness key press, in percent (about 1/16 steps).
const STEP: u8 = 6;
/// About a quarter of `STEP`.
const FINE_STEP: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// Needs the desktop key (written "Super"); F12-style bindings don't.
    pub desktop: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// The key without modifiers, lower case.
    pub key: Keysym,
}

/// Parses "Super+Shift+Return" or "F12": modifiers, then one key name (xkb names; Enter and Esc are accepted too).
/// Returns None for anything else, so a typo in config disables one binding, nothing more.
pub fn parse(text: &str) -> Option<Binding> {
    let mut parts: Vec<&str> = text.split('+').map(str::trim).collect();
    let key = parts.pop()?;
    let mut binding = Binding { desktop: false, ctrl: false, alt: false, shift: false, key: Keysym::NoSymbol };
    for part in parts {
        match part.to_ascii_lowercase().as_str() {
            "super" => binding.desktop = true,
            "ctrl" | "control" => binding.ctrl = true,
            "alt" => binding.alt = true,
            "shift" => binding.shift = true,
            _ => return None,
        }
    }
    let name = match key.to_ascii_lowercase().as_str() {
        "enter" => "Return".to_string(),
        "esc" => "Escape".to_string(),
        k if k.chars().count() == 1 => k.to_string(),
        _ => key.to_string(),
    };
    binding.key = xkb::keysym_from_name(&name, xkb::KEYSYM_CASE_INSENSITIVE);
    (binding.key != Keysym::NoSymbol).then_some(binding)
}

/// An action name from the config (`"close"`, `"exec helium-browser"`, `"mission-control"`, …); None for anything else.
pub fn action(config: &Config, text: &str) -> Option<Action> {
    match text.split_once(' ').map_or((text, ""), |(a, rest)| (a, rest.trim())) {
        ("terminal", "") => Some(Action::Spawn(config.terminal.clone())),
        ("exec", command) if !command.is_empty() => Some(Action::Spawn(command.into())),
        ("default-app", kind) if !kind.is_empty() => Some(Action::DefaultApp(kind.into())),
        ("quit", "") => Some(Action::QuitApp),
        ("close", "") => Some(Action::CloseWindow),
        ("minimise" | "minimize", "") => Some(Action::Minimise),
        ("zoom", "") => Some(Action::Zoom),
        ("fullscreen", "") => Some(Action::Fullscreen),
        ("hide", "") => Some(Action::Hide),
        ("hide-others", "") => Some(Action::HideOthers),
        ("show-all", "") => Some(Action::ShowAll),
        ("lock", "") => Some(Action::Lock),
        ("sleep", "") => Some(Action::Sleep),
        ("restart", "") => Some(Action::Power(Power::Restart)),
        ("shut-down", "") => Some(Action::Power(Power::ShutDown)),
        ("log-out", "") => Some(Action::Power(Power::LogOut)),
        // "cycle" is the M1 name.
        ("switcher" | "cycle", "") => Some(Action::Switcher { back: false, held: Default::default() }),
        ("force-quit", "") => Some(Action::ForceQuit),
        ("quake", "") => Some(Action::Quake),
        ("volume", "up") => Some(Action::Volume(true)),
        ("volume", "down") => Some(Action::Volume(false)),
        ("volume", "up-fine") => Some(Action::VolumeFine(true)),
        ("volume", "down-fine") => Some(Action::VolumeFine(false)),
        ("mute", "") => Some(Action::Mute),
        ("mic-mute", "") => Some(Action::MicMute),
        ("brightness", "up") => Some(Action::Brightness(true)),
        ("brightness", "down") => Some(Action::Brightness(false)),
        ("keyboard-backlight", "up") => Some(Action::KbdBacklight(Some(true))),
        ("keyboard-backlight", "down") => Some(Action::KbdBacklight(Some(false))),
        ("keyboard-backlight", "toggle") => Some(Action::KbdBacklight(None)),
        ("touchpad", "toggle") => Some(Action::Touchpad(None)),
        ("touchpad", "on") => Some(Action::Touchpad(Some(true))),
        ("touchpad", "off") => Some(Action::Touchpad(Some(false))),
        ("media", "play-pause") => Some(Action::Media(MediaAction::PlayPause)),
        ("media", "next") => Some(Action::Media(MediaAction::Next)),
        ("media", "previous") => Some(Action::Media(MediaAction::Previous)),
        ("eject", "") => Some(Action::Eject),
        ("media", "stop") => Some(Action::Media(MediaAction::Stop)),
        ("media", "forward") => Some(Action::Media(MediaAction::Seek(true))),
        ("media", "rewind") => Some(Action::Media(MediaAction::Seek(false))),
        ("notification-center", "") => Some(Action::NotificationCenter),
        ("spotlight", "") => Some(Action::Spotlight),
        ("emoji", "") => Some(Action::Emoji),
        ("on-screen-keyboard", "") => Some(Action::OnScreenKeyboard),
        ("launchpad", "") => Some(Action::Launchpad),
        ("app-drawer", "") => Some(Action::AppDrawer),
        ("space", "left") => Some(Action::SwitchSpace(-1)),
        ("space", "right") => Some(Action::SwitchSpace(1)),
        ("space", n) => n.parse::<usize>().ok().filter(|n| (1..=16).contains(n)).map(|n| Action::ShowSpace(n - 1)),
        ("dock-app", n) => dock_n(n).map(|n| Action::DockApp { n, new: false }),
        ("dock-app-new", n) => dock_n(n).map(|n| Action::DockApp { n, new: true }),
        ("move-to-space", "left") => Some(Action::MoveToSpace(-1)),
        ("move-to-space", "right") => Some(Action::MoveToSpace(1)),
        ("move-to-space", n) => n.parse::<usize>().ok().filter(|n| (1..=16).contains(n)).map(|n| Action::SendToSpace(n - 1)),
        ("move-to-new-space", "") => Some(Action::SendToNewSpace),
        ("move-to-display", "next") => Some(Action::MoveToDisplay(1)),
        ("move-to-display", "previous") => Some(Action::MoveToDisplay(-1)),
        ("tile", side) => tile_edges(side).map(Action::Tile),
        ("fill", "") => Some(Action::Tile(Default::default())),
        ("center" | "centre", "") => Some(Action::Center),
        ("keep-on-top", "") => Some(Action::KeepOnTop),
        ("cycle-app-windows", "") => Some(Action::CycleAppWindows),
        ("mission-control", "") => Some(Action::MissionControl),
        ("app-expose", "") => Some(Action::AppExpose),
        ("show-desktop", "") => Some(Action::ShowDesktop),
        ("input-source", "") => Some(Action::Layout(None)),
        ("screen-zoom", "") => Some(Action::ScreenZoom(None)),
        ("screen-zoom", "in") => Some(Action::ScreenZoom(Some(true))),
        ("screen-zoom", "out") => Some(Action::ScreenZoom(Some(false))),
        ("screen-reader", "") => Some(Action::ScreenReader),
        ("screenshot", "") => Some(Action::Screenshot(crate::shell::screenshot::Start::Screen)),
        ("screen-record", "") => Some(Action::ScreenRecord),
        ("screenshot-window", "") => Some(Action::Screenshot(crate::shell::screenshot::Start::Window)),
        ("screenshot-region", "") => Some(Action::Screenshot(crate::shell::screenshot::Start::Region)),
        ("screenshot-toolbar", "") => Some(Action::Screenshot(crate::shell::screenshot::Start::Toolbar)),
        _ => None,
    }
}

/// "1"…"9": a dock icon, from 0.
fn dock_n(n: &str) -> Option<usize> {
    n.parse::<usize>().ok().filter(|n| (1..=9).contains(n)).map(|n| n - 1)
}

/// "left", "top-right", …: the work-area edges a tile is against; a half or a quarter, nothing else.
fn tile_edges(side: &str) -> Option<crate::wm::Edges> {
    let mut e = crate::wm::Edges::default();
    for part in side.split('-') {
        let edge = match part {
            "left" => &mut e.left,
            "right" => &mut e.right,
            "top" => &mut e.top,
            "bottom" => &mut e.bottom,
            _ => return None,
        };
        *edge = true;
    }
    (!side.is_empty() && !(e.left && e.right) && !(e.top && e.bottom)).then_some(e)
}

/// The shortcut table from the config's `[bindings]`; entries that don't parse are logged and skipped.
pub fn from_config(config: &Config) -> Vec<(Binding, Action)> {
    let mut table = Vec::new();
    for (combo, text) in &config.bindings {
        match (parse(combo), action(config, text)) {
            (Some(binding), Some(Action::Switcher { .. })) => {
                // The same keys with Shift step back.
                let held = crate::shell::switcher::Held { desktop: binding.desktop, ctrl: binding.ctrl, alt: binding.alt, key: binding.key };
                table.push((binding, Action::Switcher { back: false, held }));
                table.push((Binding { shift: true, ..binding }, Action::Switcher { back: true, held }));
            }
            (Some(binding), Some(action)) => table.push((binding, action)),
            _ => tracing::warn!("config: shortcut `{combo}` = `{text}` not understood, skipped"),
        }
    }
    table
}

/// The action for a key. `key` is the raw (unshifted) keysym; `desktop_is_alt`: nested mode, Alt is the desktop key.
pub fn lookup<'a>(bindings: &'a [(Binding, Action)], mods: &ModifiersState, key: Keysym, desktop_is_alt: bool) -> Option<&'a Action> {
    let desktop = if desktop_is_alt { mods.alt } else { mods.logo };
    let alt = !desktop_is_alt && mods.alt;
    bindings.iter().find(|(b, _)| b.key == key && b.desktop == desktop && b.ctrl == mods.ctrl && b.shift == mods.shift && b.alt == alt).map(|(_, a)| a)
}

/// How long a `Record(true)` lasts without a key from Settings.
const RECORD_FOR: std::time::Duration = std::time::Duration::from_secs(15);

/// Settings recording a shortcut (`org.noobsbot.Desktop.Shortcuts.Record`): while on, no binding matches and every
/// key goes to the focused window. Ends when that window loses focus or after `RECORD_FOR`.
#[derive(Debug)]
pub struct Recording<W>(Option<(Option<W>, std::time::Instant)>);

impl<W> Default for Recording<W> {
    fn default() -> Self {
        Self(None)
    }
}

impl<W: PartialEq> Recording<W> {
    /// `focus`: the window focused now, which the recording belongs to.
    pub fn set(&mut self, on: bool, focus: Option<W>, now: std::time::Instant) {
        self.0 = on.then_some((focus, now));
    }

    /// Whether it is still on with `focus` focused at `now`; turns itself off otherwise.
    pub fn on(&mut self, focus: Option<&W>, now: std::time::Instant) -> bool {
        if self.0.as_ref().is_some_and(|(w, since)| w.as_ref() != focus || now.duration_since(*since) >= RECORD_FOR) {
            self.0 = None;
        }
        self.0.is_some()
    }
}

/// One key step of `by` from `percent`, within 0–100; a level already above 100 (PipeWire allows 150) is only turned down.
fn step(percent: u8, up: bool, by: u8) -> u8 {
    if up { percent.saturating_add(by).min(percent.max(100)) } else { percent.saturating_sub(by) }
}

impl State {
    pub fn run_action(&mut self, action: Action) {
        tracing::debug!(?action, "shortcut");
        let focused = self.focused_window();
        match action.clone() {
            Action::Spawn(command) => {
                self.spawn(&command.split_whitespace().map(String::from).collect::<Vec<_>>());
            }
            Action::Quake => self.toggle_quake(),
            Action::DefaultApp(kind) => {
                let app = if kind.contains('/') {
                    // ponytail: xdg-mime runs on the frame loop (tens of ms, once per key press); a worker thread if it shows.
                    let id = std::process::Command::new("xdg-mime").args(["query", "default", &kind]).output();
                    let id = id.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).unwrap_or_default();
                    (!id.is_empty()).then(|| self.apps.find(&id).cloned()).flatten()
                } else {
                    self.apps.list().iter().find(|a| !a.no_display && a.categories.contains(&kind)).cloned()
                };
                if !self.launch(app.as_ref()) {
                    tracing::warn!("no app for {kind}");
                }
            }
            Action::CloseWindow => {
                if let Some(window) = focused {
                    self.close_window(&window);
                }
            }
            // ponytail: closes the app's windows and lets the app decide to exit; a real quit comes with app tracking (M3).
            Action::QuitApp => self.app_windows(focused.as_ref()).iter().filter_map(|w| w.toplevel()).for_each(|t| t.send_close()),
            Action::Minimise => self.minimise(focused.into_iter().collect()),
            Action::Zoom => {
                if let Some(window) = focused {
                    self.toggle_zoom(&window);
                }
            }
            Action::Fullscreen => {
                if let Some(window) = focused {
                    self.toggle_fullscreen(&window);
                }
            }
            Action::Hide => {
                let windows = self.app_windows(focused.as_ref());
                self.hide_windows(windows);
            }
            Action::HideOthers => {
                let mine = self.app_windows(focused.as_ref());
                let others = self.space.elements().filter(|w| !mine.contains(w)).cloned().collect();
                // hide_windows then focuses the top window left, which is the app's own.
                self.hide_windows(others);
            }
            Action::ShowAll => {
                for (window, loc) in std::mem::take(&mut self.hidden) {
                    self.set_visible_loc(&window, loc);
                }
                self.schedule_redraw();
            }
            Action::Lock => self.lock_screen(),
            Action::ScreenZoom(step) => {
                match step {
                    None => self.zoom.toggle(),
                    Some(zoom_in) => self.zoom.step(zoom_in),
                }
                self.schedule_redraw();
            }
            Action::Sleep => self.sleep(),
            Action::TrayMenu(id) => {
                if let Some((service, menu, _)) = &self.tray_menu {
                    self.services.send(Command::TrayClicked { service: service.clone(), menu: menu.clone(), id });
                }
            }
            Action::Power(power) => self.open_alert(power),
            Action::Switcher { back, held } => self.switcher_start(back, held),
            Action::ForceQuit => self.open_force_quit(),
            Action::Spotlight => {
                self.close_drawer();
                self.toggle_spotlight();
            }
            Action::Emoji => self.toggle_emoji(),
            Action::OnScreenKeyboard => self.toggle_osk(),
            Action::Layout(index) => self.switch_layout(index),
            Action::InputMethod(name) => self.services.send(crate::services::Command::InputMethod(name)),
            // ponytail: one step per press, no repeat while held.
            // Each changes the local copy at once, so presses faster than the service's echo still add up.
            Action::Volume(up) | Action::VolumeFine(up) => {
                let Some(output) = self.sound.as_mut().map(|s| &mut s.output) else { return };
                if up && output.muted {
                    // Turning it up unmutes.
                    output.muted = false;
                    self.services.send(Command::Mute { output: true, muted: false });
                }
                output.volume = step(output.volume, up, if matches!(action, Action::VolumeFine(_)) { FINE_STEP } else { STEP });
                self.services.send(Command::Volume { output: true, percent: output.volume });
                self.show_volume();
            }
            Action::Mute | Action::MicMute => {
                let output = action == Action::Mute;
                let sound = self.sound.as_mut();
                let Some(device) = sound.and_then(|s| if output { Some(&mut s.output) } else { s.input.as_mut() }) else { return };
                device.muted = !device.muted;
                let muted = device.muted;
                self.services.send(Command::Mute { output, muted });
                if output {
                    self.show_volume();
                } else {
                    let (symbol, text) = if muted { (Symbol::MicrophoneSlash, "Microphone Off") } else { (Symbol::Microphone, "Microphone On") };
                    self.show_osd(Osd { symbol, level: None, text: Some(text.into()) });
                }
            }
            Action::Brightness(up) => {
                let Some(percent) = self.brightness.as_mut() else { return };
                // Never to 0: a black screen leaves no way to see what happened.
                *percent = step(*percent, up, STEP).max(1);
                let percent = *percent;
                self.services.send(Command::Brightness(percent));
                self.show_osd(Osd { symbol: Symbol::Sun, level: Some(percent), text: None });
            }
            Action::Media(media) => {
                let Some(now) = self.media.as_ref() else { return };
                // What the key does, shown before the player's echo arrives.
                let symbol = match media {
                    MediaAction::PlayPause if now.playing => Symbol::Pause,
                    MediaAction::PlayPause => Symbol::Play,
                    MediaAction::Next | MediaAction::Seek(true) => Symbol::SkipForward,
                    MediaAction::Previous | MediaAction::Seek(false) => Symbol::SkipBack,
                    MediaAction::Stop => Symbol::Stop,
                };
                let osd = Osd { symbol, level: None, text: Some(now.title.clone()).filter(|t| !t.is_empty()) };
                self.services.send(Command::Media { player: now.player.clone(), action: media });
                self.show_osd(osd);
            }
            Action::Eject => self.eject_last(),
            Action::Touchpad(on) => {
                let touchpad = &mut self.config.touchpad;
                touchpad.enabled = on.unwrap_or(!touchpad.enabled);
                let text = if touchpad.enabled { "Trackpad On" } else { "Trackpad Off" };
                if let Some(crate::backend::Device::Udev(udev)) = self.device.as_mut() {
                    udev.configure_inputs(&self.config.touchpad, &self.config.mouse);
                }
                self.show_osd(Osd { symbol: Symbol::Toggle, level: None, text: Some(text.into()) });
            }
            // The OSD comes with UPower's answer (`Update::KbdBacklight`).
            Action::KbdBacklight(change) => self.services.send(Command::KbdBacklight(change)),
            Action::SwitchSpace(delta) => self.switch_space(delta),
            Action::ShowSpace(n) => self.show_space(n),
            Action::MoveToSpace(delta) => self.move_to_space(delta),
            Action::SendToSpace(n) => {
                if let Some(window) = focused {
                    self.move_window_to_space(&window, n);
                }
            }
            Action::SendToNewSpace => {
                if let Some(window) = focused {
                    self.move_to_new_space(&window);
                }
            }
            Action::MoveToDisplay(delta) => {
                if let Some(window) = focused {
                    self.move_to_display(&window, delta);
                }
            }
            Action::Tile(edges) => {
                if let Some(window) = focused {
                    self.tile_to(&window, edges);
                }
            }
            Action::Center => {
                if let Some(window) = focused {
                    self.center(&window);
                }
            }
            Action::KeepOnTop => {
                if let Some(window) = focused {
                    self.keep_on_top(&window);
                }
            }
            Action::CycleAppWindows => {
                let windows = self.app_windows(focused.as_ref()).into_iter().filter(|w| self.on_current(w)).collect();
                self.dock_cycle(windows, true);
            }
            Action::MissionControl => {
                self.close_drawer();
                self.toggle_mission_control();
            }
            Action::AppExpose => self.app_expose(),
            Action::ShowDesktop => self.toggle_desktop(),
            Action::NotificationCenter => {
                let (year, month) = crate::shell::this_month();
                let open = matches!(self.popup, Some(crate::shell::Popup::Notifications { .. }));
                self.popup = (!open).then_some(crate::shell::Popup::Notifications { year, month });
                self.schedule_redraw();
            }
            Action::Dock(command) => self.dock_command(command),
            Action::DockApp { n, new } => self.dock_app(n, new),
            Action::Desktop(command) => self.desktop_command(command),
            Action::WindowMenu(command) => self.window_menu_command(command),
            Action::Launchpad => {
                self.close_drawer();
                self.toggle_launchpad();
            }
            Action::AppDrawer => self.toggle_drawer(crate::shell::drawer::Anchor::Corner),
            Action::Drawer(command) => self.drawer_command(command),
            Action::Screenshot(start) => self.screenshot(start),
            Action::ScreenRecord => {
                let was = self.screenshots.recording.is_some();
                self.toggle_recording();
                // Nothing in the menu bar shows it yet, so say so.
                let (symbol, text) = match (was, self.screenshots.recording.is_some()) {
                    (_, true) => (Symbol::Record, "Recording"),
                    (true, false) => (Symbol::Stop, "Recording Stopped"),
                    (false, false) => (Symbol::Record, "Recording needs wf-recorder"),
                };
                self.show_osd(Osd { symbol, level: None, text: Some(text.into()) });
            }
            Action::InstallUpdate => self.install_update(),
            Action::ScreenReader => {
                let on = !self.config.accessibility.screen_reader;
                self.config.accessibility.screen_reader = on;
                crate::config::save_key("accessibility", "screen_reader", if on { "true" } else { "false" });
                self.apply_screen_reader();
            }
        }
    }

    /// The volume OSD for the default output: muted shows the crossed-out speaker at 0.
    fn show_volume(&mut self) {
        let Some(sound) = self.sound.as_ref() else { return };
        let level = if sound.output.muted { 0 } else { sound.output.volume };
        self.show_osd(Osd { symbol: crate::render::menubar::volume(sound), level: Some(level), text: None });
    }

    /// Starts a program on this display; a thread reaps it so no zombie is left. The user launched it, so it gets an
    /// activation token: its first window may take focus (see `request_activation`).
    pub fn spawn(&mut self, command: &[String]) -> Option<u32> {
        let (program, args) = command.split_first()?;
        self.activation_state.retain_tokens(|_, data| data.timestamp.elapsed() < crate::wm::ACTIVATION_TIMEOUT);
        let token = self.activation_state.create_external_token(None).0.to_string();
        // ponytail: fire and forget; supervised launching lives in apps.rs (M3).
        let mut command = std::process::Command::new(program);
        command.args(args).env("WAYLAND_DISPLAY", &self.socket_name).env("XDG_ACTIVATION_TOKEN", &token).env("DESKTOP_STARTUP_ID", &token);
        command.env("XCURSOR_SIZE", crate::backend::cursor::size(self.config.appearance.pointer_size).to_string());
        if let Some(display) = &self.x_display {
            command.env("DISPLAY", display);
        }
        match command.spawn() {
            Ok(mut child) => {
                let pid = child.id();
                std::thread::spawn(move || child.wait());
                Some(pid)
            }
            Err(err) => {
                tracing::warn!("could not start {program}: {err}");
                None
            }
        }
    }

    /// Starts or stops Orca as `[accessibility] screen_reader` says, and tells apps whether one runs.
    pub fn apply_screen_reader(&mut self) {
        let on = self.config.accessibility.screen_reader;
        // Still ours: a pid of a dead Orca may have been reused.
        let running = self.screen_reader.filter(|pid| std::fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c.trim() == "orca"));
        match (on, running) {
            (true, None) => self.screen_reader = self.spawn(&["orca".into(), "--replace".into()]),
            (false, Some(pid)) => {
                // SAFETY: kill(2) on a pid just checked to be Orca.
                unsafe { libc::kill(pid as i32, libc::SIGTERM) };
                self.screen_reader = None;
            }
            _ => {}
        }
        self.services.send(Command::ScreenReader(on));
        self.access_enable(on);
    }

    /// Is `window` the quake terminal's (opened by the process the `quake` action started)?
    pub fn is_quake(&self, window: &Window) -> bool {
        let pid = window.toplevel().and_then(|t| t.wl_surface().client()).and_then(|c| c.get_credentials(&self.display_handle).ok()).map(|c| c.pid as u32);
        self.quake.is_some() && pid == self.quake
    }

    /// The quake action (E2.8). On Wayland noobsbot can't hide its own window (winit has no unmap), so the desktop does
    /// it: a focused quake window hides, one behind others comes forward, a hidden one comes back; none starts it.
    /// ponytail: shows and hides at once; the `smooth` slide down comes with the animation module.
    fn toggle_quake(&mut self) {
        let serial = SERIAL_COUNTER.next_serial();
        let shown = self.space.elements().find(|w| self.is_quake(w)).cloned();
        if let Some(window) = shown {
            if self.focused_window().as_ref() == Some(&window) {
                self.hide_windows(vec![window]);
            } else {
                self.focus_window(Some(window), serial);
            }
        } else if let Some(i) = self.hidden.iter().position(|(w, _)| self.is_quake(w)) {
            let (window, loc) = self.hidden.remove(i);
            self.set_visible_loc(&window, loc);
            self.focus_window(Some(window), serial);
        } else if !self.quake.is_some_and(|pid| std::path::Path::new(&format!("/proc/{pid}")).exists()) {
            // Nested, DISPLAY is the host's: noobsbot would grab its hotkey (F12) there and steal it from the host
            // desktop. `env` execs, so the pid stays the terminal's.
            let command: &[&str] = if self.nested { &["env", "-u", "DISPLAY", "noobsbot", "--quake"] } else { &["noobsbot", "--quake"] };
            self.quake = self.spawn(&command.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        }
    }

    /// Settings is still recording a shortcut (see `Recording`).
    pub fn recording_on(&mut self) -> bool {
        let focus = self.focused_window();
        self.recording.on(focus.as_ref(), std::time::Instant::now())
    }

    pub fn focused_window(&self) -> Option<Window> {
        let surface = self.seat.get_keyboard()?.current_focus()?;
        self.space.elements().find(|w| w.toplevel().is_some_and(|t| *t.wl_surface() == surface)).cloned()
    }

    /// Every mapped window of the client that owns `window`.
    fn app_windows(&self, window: Option<&Window>) -> Vec<Window> {
        let Some(client) = window.and_then(|w| w.toplevel()).and_then(|t| t.wl_surface().client()) else { return Vec::new() };
        self.space.elements().filter(|w| w.toplevel().and_then(|t| t.wl_surface().client()).is_some_and(|c| c.id() == client.id())).cloned().collect()
    }

    pub fn hide_windows(&mut self, windows: Vec<Window>) {
        if windows.is_empty() {
            return;
        }
        for window in windows {
            let Some(loc) = self.frame(&window).map(|f| f.loc) else { continue };
            window.set_activated(false);
            self.window_minimising(&window);
            self.space.unmap_elem(&window);
            self.hidden.push((window, loc));
        }
        let top = self.top_window();
        self.focus_window(top, SERIAL_COUNTER.next_serial());
        self.schedule_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(logo: bool, alt: bool, shift: bool) -> ModifiersState {
        ModifiersState { logo, alt, shift, ..Default::default() }
    }

    #[test]
    fn parses_bindings() {
        assert_eq!(parse("Super+Enter").map(|b| b.key), Some(Keysym::Return));
        assert_eq!(parse("Super+b").map(|b| b.key), Some(Keysym::b));
        assert_eq!(parse("Super+B").map(|b| b.key), Some(Keysym::b), "letters are case-insensitive");
        assert!(parse("Super+Alt+Esc").is_some_and(|b| b.alt && b.key == Keysym::Escape));
        assert!(parse("F12").is_some_and(|b| !b.desktop && b.key == Keysym::F12));
        assert_eq!(parse("Super+Ctrl+X"), parse("ctrl+super+x"), "any order, any case");
        assert_ne!(parse("Super+X"), parse("Super+Shift+X"));
        assert_eq!(parse("Super+Hyper+B"), None);
        assert_eq!(parse("Super+NoSuchKey"), None);
    }

    #[test]
    fn parses_window_actions() {
        let a = |text| action(&Config::default(), text);
        let e = crate::wm::Edges { right: true, top: true, ..Default::default() };
        assert_eq!(a("tile top-right"), Some(Action::Tile(e)));
        assert_eq!(a("tile right-top"), Some(Action::Tile(e)));
        assert_eq!(a("fill"), Some(Action::Tile(Default::default())));
        assert_eq!(a("tile left-right"), None);
        assert_eq!(a("tile"), None);
        assert_eq!(a("tile middle"), None);
        assert_eq!(a("screen-zoom out"), Some(Action::ScreenZoom(Some(false))));
        assert!(parse("Super+Alt+equal").is_some_and(|b| b.alt && b.key == Keysym::equal));
        assert_eq!(a("move-to-space 16"), Some(Action::SendToSpace(15)));
        assert_eq!(a("move-to-space 17"), None);
        assert_eq!(a("move-to-space left"), Some(Action::MoveToSpace(-1)));
        assert_eq!(a("move-to-display previous"), Some(Action::MoveToDisplay(-1)));
        assert_eq!(a("keep-on-top"), Some(Action::KeepOnTop));
    }

    #[test]
    fn recording_ends_on_focus_change_or_timeout() {
        let t = std::time::Instant::now();
        let mut r = Recording::default();
        assert!(!r.on(Some(&1), t));
        r.set(true, Some(1), t);
        assert!(r.on(Some(&1), t + RECORD_FOR / 2));
        r.set(false, Some(1), t);
        assert!(!r.on(Some(&1), t), "Record(false)");
        r.set(true, Some(1), t);
        assert!(!r.on(Some(&1), t + RECORD_FOR), "expired");
        r.set(true, Some(1), t);
        assert!(!r.on(Some(&2), t), "another window took focus");
        assert!(!r.on(Some(&1), t), "and it stays off when focus comes back");
        r.set(true, Some(1), t);
        assert!(!r.on(None, t), "focus went nowhere");
    }

    #[test]
    fn steps() {
        assert_eq!(step(50, true, STEP), 56);
        assert_eq!(step(98, true, STEP), 100);
        assert_eq!(step(3, false, STEP), 0);
        assert_eq!(step(130, true, STEP), 130, "above 100 only goes down");
        assert_eq!(step(130, false, STEP), 124);
        assert_eq!(step(50, true, FINE_STEP), 52);
    }

    #[test]
    fn media_keys_are_bound() {
        let table = from_config(&Config::default());
        let none = mods(false, false, false);
        assert_eq!(lookup(&table, &none, Keysym::XF86_AudioRaiseVolume, false), Some(&Action::Volume(true)));
        assert_eq!(lookup(&table, &none, Keysym::XF86_AudioMicMute, false), Some(&Action::MicMute));
        assert_eq!(lookup(&table, &none, Keysym::XF86_MonBrightnessDown, false), Some(&Action::Brightness(false)));
        assert_eq!(lookup(&table, &none, Keysym::XF86_AudioPlay, false), Some(&Action::Media(MediaAction::PlayPause)));
        assert_eq!(lookup(&table, &none, Keysym::XF86_AudioStop, false), Some(&Action::Media(MediaAction::Stop)));
        assert_eq!(lookup(&table, &none, Keysym::XF86_AudioRewind, false), Some(&Action::Media(MediaAction::Seek(false))));
        assert_eq!(lookup(&table, &none, Keysym::XF86_Eject, false), Some(&Action::Eject));
        let shift_alt = ModifiersState { alt: true, shift: true, ..Default::default() };
        assert_eq!(lookup(&table, &shift_alt, Keysym::XF86_AudioLowerVolume, false), Some(&Action::VolumeFine(false)));
        assert_eq!(lookup(&table, &none, Keysym::XF86_KbdBrightnessUp, false), Some(&Action::KbdBacklight(Some(true))));
        assert_eq!(lookup(&table, &none, Keysym::XF86_KbdLightOnOff, false), Some(&Action::KbdBacklight(None)));
        assert_eq!(lookup(&table, &none, Keysym::XF86_TouchpadToggle, false), Some(&Action::Touchpad(None)));
        assert_eq!(lookup(&table, &none, Keysym::XF86_TouchpadOff, false), Some(&Action::Touchpad(Some(false))));
        assert_eq!(lookup(&table, &none, Keysym::XF86_Display, false), Some(&Action::Spawn("noobsbot-desktop-settings --page displays".into())));
        assert_eq!(lookup(&table, &none, Keysym::XF86_WWW, false), Some(&Action::DefaultApp("x-scheme-handler/https".into())));
        assert_eq!(lookup(&table, &none, Keysym::XF86_Mail, false), Some(&Action::DefaultApp("x-scheme-handler/mailto".into())));
        assert_eq!(lookup(&table, &none, Keysym::XF86_Explorer, false), Some(&Action::DefaultApp("inode/directory".into())));
        assert_eq!(lookup(&table, &none, Keysym::XF86_Calculator, false), Some(&Action::DefaultApp("Calculator".into())));
        assert_eq!(lookup(&table, &none, Keysym::XF86_Search, false), Some(&Action::Spotlight));
        let alt = ModifiersState { alt: true, ..Default::default() };
        assert_eq!(lookup(&table, &alt, Keysym::Print, false), Some(&Action::Screenshot(crate::shell::screenshot::Start::Window)));
        assert_eq!(lookup(&table, &ModifiersState { logo: true, alt: true, ..Default::default() }, Keysym::_5, false), Some(&Action::ScreenRecord));
        assert_eq!(lookup(&table, &none, Keysym::XF86_Sleep, false), Some(&Action::Sleep));
        assert_eq!(lookup(&table, &mods(true, false, true), Keysym::q, false), Some(&Action::Power(Power::LogOut)));
    }

    #[test]
    fn looks_up_with_exact_modifiers() {
        let table = from_config(&Config::default());
        assert_eq!(lookup(&table, &mods(true, false, false), Keysym::b, false), Some(&Action::Spawn("helium-browser".into())));
        assert_eq!(lookup(&table, &mods(true, false, true), Keysym::b, false), None, "Shift is not part of Super+B");
        assert_eq!(lookup(&table, &mods(false, false, false), Keysym::b, false), None, "plain b goes to the app");
        assert_eq!(lookup(&table, &mods(true, false, false), Keysym::c, false), None, "Super+C passes through");
        assert_eq!(lookup(&table, &mods(true, false, false), Keysym::a, false), Some(&Action::AppDrawer));
        assert_eq!(lookup(&table, &mods(false, false, false), Keysym::F12, false), Some(&Action::Quake));
        assert_eq!(lookup(&table, &mods(true, false, false), Keysym::F12, false), None, "Super+F12 is not F12");
        assert_eq!(lookup(&table, &mods(false, true, false), Keysym::b, true), Some(&Action::Spawn("helium-browser".into())), "nested: Alt is the desktop key");
        assert!(matches!(lookup(&table, &mods(true, false, false), Keysym::Tab, false), Some(Action::Switcher { back: false, held }) if held.desktop));
        assert!(matches!(lookup(&table, &mods(true, false, true), Keysym::Tab, false), Some(Action::Switcher { back: true, .. })), "Shift steps back");
        assert_eq!(lookup(&table, &ModifiersState { logo: true, alt: true, ..Default::default() }, Keysym::Escape, false), Some(&Action::ForceQuit));
        assert_eq!(lookup(&table, &mods(true, false, false), Keysym::Right, false), Some(&Action::SwitchSpace(1)));
        assert_eq!(lookup(&table, &mods(true, false, true), Keysym::Left, false), Some(&Action::MoveToSpace(-1)));
        assert_eq!(lookup(&table, &mods(true, false, false), Keysym::_3, false), Some(&Action::DockApp { n: 2, new: false }));
        assert_eq!(lookup(&table, &mods(true, false, true), Keysym::_2, false), Some(&Action::DockApp { n: 1, new: true }));
        assert!(matches!(lookup(&table, &mods(true, false, true), Keysym::_3, false), Some(Action::Screenshot(_))), "Super+Shift+3 stays a screenshot");
        let super_ctrl = ModifiersState { logo: true, ctrl: true, ..Default::default() };
        assert_eq!(lookup(&table, &super_ctrl, Keysym::_3, false), Some(&Action::ShowSpace(2)));
        assert_eq!(lookup(&table, &ModifiersState { ctrl: true, ..Default::default() }, Keysym::_3, false), None, "Ctrl+3 goes to the app");
        assert_eq!(action(&Config::default(), "dock-app 10"), None);
        let tile = |left, right, top, bottom| Some(Action::Tile(crate::wm::Edges { left, right, top, bottom }));
        assert_eq!(lookup(&table, &super_ctrl, Keysym::Left, false).cloned(), tile(true, false, false, false));
        assert_eq!(lookup(&table, &super_ctrl, Keysym::Down, false).cloned(), tile(false, false, false, true));
        assert_eq!(lookup(&table, &mods(true, false, false), Keysym::grave, false), Some(&Action::CycleAppWindows));
        assert_eq!(lookup(&table, &mods(true, false, false), Keysym::Up, false), Some(&Action::MissionControl));
        assert_eq!(lookup(&table, &mods(false, false, false), Keysym::Left, false), None, "plain arrows go to the app");
    }
}
