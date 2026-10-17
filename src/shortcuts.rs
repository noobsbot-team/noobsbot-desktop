//! Desktop shortcuts (E1.6). "Super" in a binding means the desktop key: Super in a real session, Alt when nested
//! (the host desktop keeps Super for itself). Super+C/V/X/Z/A/S/F/T/R/L are never bound, so they reach the app.

use smithay::{
    desktop::Window,
    input::keyboard::{Keysym, ModifiersState, xkb},
    reexports::wayland_server::Resource,
    utils::SERIAL_COUNTER,
};

use crate::state::State;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Spawn(String),
    /// Close every window of the focused app (Super+Q).
    QuitApp,
    CloseWindow,
    Minimise,
    /// Hide every window of the focused app (Super+H).
    Hide,
    /// Basic window cycling; the full switcher comes in M4.
    CycleWindows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// The key without modifiers, lower case.
    pub key: Keysym,
}

/// Parses "Super+Shift+Return": the desktop key first, then modifiers, then one key name (xkb names; Enter and Esc
/// are accepted too). Returns None for anything else, so a typo in config disables one binding, nothing more.
pub fn parse(text: &str) -> Option<Binding> {
    let mut parts: Vec<&str> = text.split('+').map(str::trim).collect();
    let key = parts.pop()?;
    if !parts.first().is_some_and(|p| p.eq_ignore_ascii_case("super")) {
        return None;
    }
    let mut binding = Binding { ctrl: false, alt: false, shift: false, key: Keysym::NoSymbol };
    for part in &parts[1..] {
        match part.to_ascii_lowercase().as_str() {
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

/// The M1 bindings (plan E1.6). ponytail: fixed until config.rs lands, which makes every binding editable.
pub fn defaults() -> Vec<(Binding, Action)> {
    [
        ("Super+Return", Action::Spawn("noobsbot".into())),
        ("Super+B", Action::Spawn("brave".into())),
        ("Super+E", Action::Spawn("dolphin".into())),
        ("Super+Q", Action::QuitApp),
        ("Super+W", Action::CloseWindow),
        ("Super+M", Action::Minimise),
        ("Super+H", Action::Hide),
        ("Super+Tab", Action::CycleWindows),
    ]
    .into_iter()
    .filter_map(|(text, action)| Some((parse(text)?, action)))
    .collect()
}

/// The action for a key pressed while the desktop key is held. `key` is the raw (unshifted) keysym; `desktop_is_alt`:
/// nested mode, Alt is the desktop key.
pub fn lookup<'a>(bindings: &'a [(Binding, Action)], mods: &ModifiersState, key: Keysym, desktop_is_alt: bool) -> Option<&'a Action> {
    let desktop = if desktop_is_alt { mods.alt } else { mods.logo };
    if !desktop {
        return None;
    }
    let other_alt = !desktop_is_alt && mods.alt;
    bindings.iter().find(|(b, _)| b.key == key && b.ctrl == mods.ctrl && b.shift == mods.shift && b.alt == other_alt).map(|(_, a)| a)
}

impl State {
    pub fn run_action(&mut self, action: Action) {
        tracing::debug!(?action, "shortcut");
        let focused = self.focused_window();
        match action {
            Action::Spawn(command) => self.spawn(&command.split_whitespace().map(String::from).collect::<Vec<_>>()),
            Action::CloseWindow => {
                if let Some(toplevel) = focused.as_ref().and_then(|w| w.toplevel()) {
                    toplevel.send_close();
                }
            }
            // ponytail: closes the app's windows and lets the app decide to exit; a real quit comes with app tracking (M3).
            Action::QuitApp => self.app_windows(focused.as_ref()).iter().filter_map(|w| w.toplevel()).for_each(|t| t.send_close()),
            Action::Minimise => self.hide_windows(focused.into_iter().collect()),
            Action::Hide => {
                let windows = self.app_windows(focused.as_ref());
                self.hide_windows(windows);
            }
            // ponytail: brings back the oldest hidden window first, else rotates the stack; the real switcher is M4.
            Action::CycleWindows => {
                let next = if self.hidden.is_empty() {
                    self.space.elements().next().cloned()
                } else {
                    let (window, loc) = self.hidden.remove(0);
                    self.set_visible_loc(&window, loc);
                    Some(window)
                };
                if next.is_some() {
                    self.focus_window(next, SERIAL_COUNTER.next_serial());
                }
            }
        }
    }

    /// Starts a program on this display; a thread reaps it so no zombie is left.
    pub fn spawn(&self, command: &[String]) {
        let Some((program, args)) = command.split_first() else { return };
        // ponytail: fire and forget; supervised launching lives in apps.rs (M3).
        match std::process::Command::new(program).args(args).env("WAYLAND_DISPLAY", &self.socket_name).spawn() {
            Ok(mut child) => {
                std::thread::spawn(move || child.wait());
            }
            Err(err) => tracing::warn!("could not start {program}: {err}"),
        }
    }

    fn focused_window(&self) -> Option<Window> {
        let surface = self.seat.get_keyboard()?.current_focus()?;
        self.space.elements().find(|w| w.toplevel().is_some_and(|t| *t.wl_surface() == surface)).cloned()
    }

    /// Every mapped window of the client that owns `window`.
    fn app_windows(&self, window: Option<&Window>) -> Vec<Window> {
        let Some(client) = window.and_then(|w| w.toplevel()).and_then(|t| t.wl_surface().client()) else { return Vec::new() };
        self.space.elements().filter(|w| w.toplevel().and_then(|t| t.wl_surface().client()).is_some_and(|c| c.id() == client.id())).cloned().collect()
    }

    fn hide_windows(&mut self, windows: Vec<Window>) {
        if windows.is_empty() {
            return;
        }
        for window in windows {
            let Some(loc) = self.space.element_location(&window) else { continue };
            window.set_activated(false);
            self.space.unmap_elem(&window);
            self.hidden.push((window, loc));
        }
        let top = self.space.elements().last().cloned();
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
        assert_eq!(parse("Ctrl+B"), None, "must start with the desktop key");
        assert_eq!(parse("Super+Hyper+B"), None);
        assert_eq!(parse("Super+NoSuchKey"), None);
    }

    #[test]
    fn looks_up_with_exact_modifiers() {
        let table = defaults();
        assert_eq!(lookup(&table, &mods(true, false, false), Keysym::b, false), Some(&Action::Spawn("brave".into())));
        assert_eq!(lookup(&table, &mods(true, false, true), Keysym::b, false), None, "Shift is not part of Super+B");
        assert_eq!(lookup(&table, &mods(false, false, false), Keysym::b, false), None, "plain b goes to the app");
        assert_eq!(lookup(&table, &mods(true, false, false), Keysym::c, false), None, "Super+C passes through");
        assert_eq!(lookup(&table, &mods(false, true, false), Keysym::b, true), Some(&Action::Spawn("brave".into())), "nested: Alt is the desktop key");
    }
}
