//! Desktop shortcuts (E1.6). "Super" in a binding means the desktop key: Super in a real session, Alt when nested
//! (the host desktop keeps Super for itself). Super+C/V/X/Z/A/S/F/T/R/L are never bound, so they reach the app.

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
    /// Close every window of the focused app (Super+Q).
    QuitApp,
    CloseWindow,
    Minimise,
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
    /// Mutes or unmutes the default output.
    Mute,
    /// Mutes or unmutes the default microphone.
    MicMute,
    /// The display backlight by one step; `true`: up.
    Brightness(bool),
    /// To the Now Playing player.
    Media(MediaAction),
    /// A row of the open tray menu, by dbusmenu id.
    TrayMenu(i32),
    /// Opens or closes Notification Center (Super+N).
    NotificationCenter,
    /// Opens or closes Spotlight (Super+Space, E4.3).
    Spotlight,
    /// A row of a dock icon's right-click menu (E3.1).
    Dock(crate::shell::dock::Command),
    /// Opens or closes Launchpad (E4.4; F4, off by default).
    Launchpad,
    /// The Space to the left (-1) or right (+1) (E5.1).
    SwitchSpace(i32),
    /// Space n, from 0.
    ShowSpace(usize),
    /// The focused window comes along to the Space left or right.
    MoveToSpace(i32),
    /// Opens or closes Mission Control (E5.2).
    MissionControl,
    /// Mission Control with only the focused app's windows.
    AppExpose,
}

/// A volume or brightness key press, in percent (about macOS's 1/16 steps).
const STEP: u8 = 6;

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

/// The shortcut table from the config's `[bindings]`; entries that don't parse are logged and skipped.
pub fn from_config(config: &Config) -> Vec<(Binding, Action)> {
    let action = |text: &str| match text.split_once(' ').map_or((text, ""), |(a, rest)| (a, rest.trim())) {
        ("terminal", "") => Some(Action::Spawn(config.terminal.clone())),
        ("exec", command) if !command.is_empty() => Some(Action::Spawn(command.into())),
        ("quit", "") => Some(Action::QuitApp),
        ("close", "") => Some(Action::CloseWindow),
        ("minimise" | "minimize", "") => Some(Action::Minimise),
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
        ("mute", "") => Some(Action::Mute),
        ("mic-mute", "") => Some(Action::MicMute),
        ("brightness", "up") => Some(Action::Brightness(true)),
        ("brightness", "down") => Some(Action::Brightness(false)),
        ("media", "play-pause") => Some(Action::Media(MediaAction::PlayPause)),
        ("media", "next") => Some(Action::Media(MediaAction::Next)),
        ("media", "previous") => Some(Action::Media(MediaAction::Previous)),
        ("notification-center", "") => Some(Action::NotificationCenter),
        ("spotlight", "") => Some(Action::Spotlight),
        ("launchpad", "") => Some(Action::Launchpad),
        ("space", "left") => Some(Action::SwitchSpace(-1)),
        ("space", "right") => Some(Action::SwitchSpace(1)),
        ("space", n) => n.parse::<usize>().ok().filter(|n| (1..=16).contains(n)).map(|n| Action::ShowSpace(n - 1)),
        ("move-to-space", "left") => Some(Action::MoveToSpace(-1)),
        ("move-to-space", "right") => Some(Action::MoveToSpace(1)),
        ("mission-control", "") => Some(Action::MissionControl),
        ("app-expose", "") => Some(Action::AppExpose),
        _ => None,
    };
    let mut table = Vec::new();
    for (combo, text) in &config.bindings {
        match (parse(combo), action(text)) {
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

/// One key step from `percent`, within 0–100; a level already above 100 (PipeWire allows 150) is only turned down.
fn step(percent: u8, up: bool) -> u8 {
    if up { percent.saturating_add(STEP).min(percent.max(100)) } else { percent.saturating_sub(STEP) }
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
            Action::CloseWindow => {
                if let Some(window) = focused {
                    self.close_window(&window);
                }
            }
            // ponytail: closes the app's windows and lets the app decide to exit; a real quit comes with app tracking (M3).
            Action::QuitApp => self.app_windows(focused.as_ref()).iter().filter_map(|w| w.toplevel()).for_each(|t| t.send_close()),
            Action::Minimise => self.minimise(focused.into_iter().collect()),
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
            Action::Sleep => self.sleep(),
            Action::TrayMenu(id) => {
                if let Some((service, menu, _)) = &self.tray_menu {
                    self.services.send(Command::TrayClicked { service: service.clone(), menu: menu.clone(), id });
                }
            }
            Action::Power(power) => self.open_alert(power),
            Action::Switcher { back, held } => self.switcher_start(back, held),
            Action::ForceQuit => self.open_force_quit(),
            Action::Spotlight => self.toggle_spotlight(),
            // ponytail: one step per press, no repeat while held.
            // Each changes the local copy at once, so presses faster than the service's echo still add up.
            Action::Volume(up) => {
                let Some(output) = self.sound.as_mut().map(|s| &mut s.output) else { return };
                if up && output.muted {
                    // As on macOS: turning it up unmutes.
                    output.muted = false;
                    self.services.send(Command::Mute { output: true, muted: false });
                }
                output.volume = step(output.volume, up);
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
                *percent = step(*percent, up).max(1);
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
                    MediaAction::Next => Symbol::SkipForward,
                    MediaAction::Previous => Symbol::SkipBack,
                };
                let osd = Osd { symbol, level: None, text: Some(now.title.clone()).filter(|t| !t.is_empty()) };
                self.services.send(Command::Media { player: now.player.clone(), action: media });
                self.show_osd(osd);
            }
            Action::SwitchSpace(delta) => self.switch_space(delta),
            Action::ShowSpace(n) => self.show_space(n),
            Action::MoveToSpace(delta) => self.move_to_space(delta),
            Action::MissionControl => self.toggle_mission_control(),
            Action::AppExpose => self.app_expose(),
            Action::NotificationCenter => {
                let (year, month) = crate::shell::this_month();
                let open = matches!(self.popup, Some(crate::shell::Popup::Notifications { .. }));
                self.popup = (!open).then_some(crate::shell::Popup::Notifications { year, month });
                self.schedule_redraw();
            }
            Action::Dock(command) => self.dock_command(command),
            Action::Launchpad => self.toggle_launchpad(),
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
            crate::shell::dock_fx::not_minimised(&window);
            window.set_activated(false);
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
        assert_eq!(parse("Super+Hyper+B"), None);
        assert_eq!(parse("Super+NoSuchKey"), None);
    }

    #[test]
    fn steps() {
        assert_eq!(step(50, true), 56);
        assert_eq!(step(98, true), 100);
        assert_eq!(step(3, false), 0);
        assert_eq!(step(130, true), 130, "above 100 only goes down");
        assert_eq!(step(130, false), 124);
    }

    #[test]
    fn media_keys_are_bound() {
        let table = from_config(&Config::default());
        let none = mods(false, false, false);
        assert_eq!(lookup(&table, &none, Keysym::XF86_AudioRaiseVolume, false), Some(&Action::Volume(true)));
        assert_eq!(lookup(&table, &none, Keysym::XF86_AudioMicMute, false), Some(&Action::MicMute));
        assert_eq!(lookup(&table, &none, Keysym::XF86_MonBrightnessDown, false), Some(&Action::Brightness(false)));
        assert_eq!(lookup(&table, &none, Keysym::XF86_AudioPlay, false), Some(&Action::Media(MediaAction::PlayPause)));
    }

    #[test]
    fn looks_up_with_exact_modifiers() {
        let table = from_config(&Config::default());
        assert_eq!(lookup(&table, &mods(true, false, false), Keysym::b, false), Some(&Action::Spawn("helium-browser".into())));
        assert_eq!(lookup(&table, &mods(true, false, true), Keysym::b, false), None, "Shift is not part of Super+B");
        assert_eq!(lookup(&table, &mods(false, false, false), Keysym::b, false), None, "plain b goes to the app");
        assert_eq!(lookup(&table, &mods(true, false, false), Keysym::c, false), None, "Super+C passes through");
        assert_eq!(lookup(&table, &mods(false, false, false), Keysym::F12, false), Some(&Action::Quake));
        assert_eq!(lookup(&table, &mods(true, false, false), Keysym::F12, false), None, "Super+F12 is not F12");
        assert_eq!(lookup(&table, &mods(false, true, false), Keysym::b, true), Some(&Action::Spawn("helium-browser".into())), "nested: Alt is the desktop key");
        assert!(matches!(lookup(&table, &mods(true, false, false), Keysym::Tab, false), Some(Action::Switcher { back: false, held }) if held.desktop));
        assert!(matches!(lookup(&table, &mods(true, false, true), Keysym::Tab, false), Some(Action::Switcher { back: true, .. })), "Shift steps back");
        assert_eq!(lookup(&table, &ModifiersState { logo: true, alt: true, ..Default::default() }, Keysym::Escape, false), Some(&Action::ForceQuit));
        assert_eq!(lookup(&table, &mods(true, false, false), Keysym::Right, false), Some(&Action::SwitchSpace(1)));
        assert_eq!(lookup(&table, &mods(true, false, true), Keysym::Left, false), Some(&Action::MoveToSpace(-1)));
        assert_eq!(lookup(&table, &mods(true, false, false), Keysym::_3, false), Some(&Action::ShowSpace(2)));
        assert_eq!(lookup(&table, &mods(true, false, false), Keysym::Up, false), Some(&Action::MissionControl));
        assert_eq!(lookup(&table, &mods(false, false, false), Keysym::Left, false), None, "plain arrows go to the app");
    }
}
