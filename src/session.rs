//! Reopen windows at login (E8.8). Log Out, Restart and Shut Down with the alert's box ticked save every app's windows
//! to `$XDG_STATE_HOME/noobsbot-desktop/session.toml`; the next login starts each app once and puts its windows back
//! as they map, the app's first window on its first entry. Entries still waiting after `WAIT` are dropped.

use std::time::Duration;

use serde::Deserialize;
use smithay::{
    desktop::Window,
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_protocols::xdg::shell::server::xdg_toplevel,
    },
};

use crate::{spaces, state::State, wm::Rect};

/// Saved windows wait this long for their app to map them.
const WAIT: Duration = Duration::from_secs(20);
/// Apps start this long after login, once the autostart apps have had their turn.
// ponytail: a fixed delay, not the end of the session target's start job; an app autostart already opened isn't started twice.
const AFTER_AUTOSTART: Duration = Duration::from_secs(3);

/// A window placed from a saved entry, to put on its Space (and zoom) once placed.
struct Reopened(Entry);

/// One saved window.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Entry {
    /// The window's app id.
    pub app: String,
    /// Its desktop-file id, started from the app index; else `command` is run.
    #[serde(default)]
    pub desktop: Option<String>,
    #[serde(default)]
    pub command: Vec<String>,
    /// The n-th Space that isn't a fullscreen app's (from 0), among its display's Spaces.
    #[serde(default)]
    pub space: usize,
    pub display: String,
    /// x, y, w, h of the frame (the untiled one for a zoomed or full-screen window), on Space row 0.
    pub frame: [i32; 4],
    #[serde(default)]
    pub maximized: bool,
    #[serde(default)]
    pub fullscreen: bool,
}

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    window: Vec<Entry>,
}

pub fn path() -> std::path::PathBuf {
    crate::config::xdg_dir("XDG_STATE_HOME", ".local/state").join("noobsbot-desktop/session.toml")
}

/// A TOML basic string: JSON's escapes are a subset of TOML's.
fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

pub fn format(entries: &[Entry]) -> String {
    let mut out = String::new();
    for e in entries {
        let command: Vec<String> = e.command.iter().map(|c| quote(c)).collect();
        out += &format!("[[window]]\napp = {}\n", quote(&e.app));
        if let Some(desktop) = &e.desktop {
            out += &format!("desktop = {}\n", quote(desktop));
        }
        let [x, y, w, h] = e.frame;
        out += &format!(
            "command = [{}]\nspace = {}\ndisplay = {}\nframe = [{x}, {y}, {w}, {h}]\nmaximized = {}\nfullscreen = {}\n\n",
            command.join(", "),
            e.space,
            quote(&e.display),
            e.maximized,
            e.fullscreen
        );
    }
    out
}

pub fn parse(text: &str) -> Vec<Entry> {
    toml::from_str::<File>(text).map(|f| f.window).inspect_err(|err| tracing::warn!("session.toml: {err}")).unwrap_or_default()
}

/// The first entry still waiting for a window of `app`, taken out.
pub fn take(pending: &mut Vec<Entry>, app: &str) -> Option<Entry> {
    let at = pending.iter().position(|e| e.app == app)?;
    Some(pending.remove(at))
}

/// How many plain (not fullscreen) Spaces come before Space `desk`.
pub fn plain_index<T>(list: &[Option<T>], desk: usize) -> usize {
    list.iter().take(desk).filter(|s| s.is_none()).count()
}

/// The index of the `n`-th plain Space, if there are that many.
pub fn plain_desk<T>(list: &[Option<T>], n: usize) -> Option<usize> {
    list.iter().enumerate().filter(|(_, s)| s.is_none()).nth(n).map(|(i, _)| i)
}

impl State {
    /// The power alert was confirmed: save the windows (`keep`) or forget any saved ones.
    pub fn session_save(&mut self, keep: bool) {
        let path = path();
        let saved = if keep {
            let text = format(&self.session_entries());
            path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| crate::config::save_atomic(&path, &text))
        } else {
            std::fs::remove_file(&path).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) })
        };
        if let Err(err) = saved {
            tracing::warn!("{}: {err}", path.display());
        }
    }

    /// Every app window, bottom to top (minimised ones too), but not dialogs or the quake terminal.
    fn session_entries(&mut self) -> Vec<Entry> {
        let top = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map_or(0, |g| g.loc.y);
        let shown = self.space.elements().map(|w| (w.clone(), None));
        let windows: Vec<(Window, Option<smithay::utils::Point<i32, smithay::utils::Logical>>)> =
            shown.chain(self.hidden.iter().map(|(w, loc)| (w.clone(), Some(*loc)))).collect();
        let mut entries = Vec::new();
        for (window, hidden) in windows {
            let Some(toplevel) = window.toplevel() else { continue };
            if toplevel.parent().is_some() || self.is_quake(&window) {
                continue;
            }
            let Some(app) = self.app_id(&window) else { continue };
            let now = match hidden {
                Some(loc) => Some(Rect::new(loc, window.geometry().size + (0, crate::decorations::bar_height(&window)).into())),
                None => self.frame(&window),
            };
            let (Some(now), (maximized, fullscreen)) =
                (now, toplevel.with_pending_state(|s| (s.states.contains(xdg_toplevel::State::Maximized), s.states.contains(xdg_toplevel::State::Fullscreen))))
            else {
                continue;
            };
            let mut frame = self.restore_frame(&window).unwrap_or(now);
            frame.loc.y -= spaces::row_at(frame.loc.y, top) * spaces::STRIDE;
            let outputs = self.space.outputs().filter_map(|o| Some((self.space.output_geometry(o)?, o.clone())));
            let Some(output) = crate::displays::mostly_on(frame, outputs) else { continue };
            // Its index among its display's Spaces.
            let key = self.output_key(&output);
            let desks = self.desks_at(&key);
            let space = plain_index(&desks.list, spaces::desk_of(spaces::row_at(now.loc.y, top), desks.current));
            let desktop = self.apps.find(&app).map(|a| a.id.clone());
            let command = match desktop {
                Some(_) => Vec::new(),
                None => self
                    .pid(&window)
                    .and_then(|pid| std::fs::read(format!("/proc/{pid}/cmdline")).ok())
                    .map_or_else(Vec::new, |c| c.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect()),
            };
            if desktop.is_none() && command.is_empty() {
                continue;
            }
            let frame = [frame.loc.x, frame.loc.y, frame.size.w, frame.size.h];
            entries.push(Entry { app, desktop, command, space, display: output.name(), frame, maximized, fullscreen });
        }
        entries
    }

    /// At login: read (and remove) the saved windows, then start their apps once autostart has had its turn.
    pub fn session_restore(&mut self) {
        let path = path();
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let _ = std::fs::remove_file(&path);
        self.reopen = parse(&text);
        if self.reopen.is_empty() {
            return;
        }
        let start = |_, _: &mut (), state: &mut State| {
            // The app index loads on a worker thread.
            if state.apps.list().is_empty() {
                return TimeoutAction::ToDuration(Duration::from_millis(500));
            }
            state.session_launch();
            TimeoutAction::Drop
        };
        let forget = |_, _: &mut (), state: &mut State| {
            state.reopen.clear();
            TimeoutAction::Drop
        };
        let started = self.loop_handle.insert_source(Timer::from_duration(AFTER_AUTOSTART), start).map(drop);
        let dropped = self.loop_handle.insert_source(Timer::from_duration(AFTER_AUTOSTART + WAIT), forget).map(drop);
        if let Err(err) = started.and(dropped) {
            tracing::warn!("session restore: {err}");
        }
    }

    /// Starts each saved app once, unless it already has a window (autostart opened it).
    fn session_launch(&mut self) {
        let mut started: Vec<String> = Vec::new();
        for entry in self.reopen.clone() {
            let open = self.space.elements().chain(self.hidden.iter().map(|(w, _)| w)).any(|w| self.app_id(w).as_deref() == Some(&entry.app));
            if open || started.contains(&entry.app) {
                continue;
            }
            started.push(entry.app.clone());
            match entry.desktop.as_deref().and_then(|d| self.apps.find(d)).cloned() {
                Some(app) => {
                    self.launch(Some(&app));
                }
                None if !entry.command.is_empty() => {
                    self.spawn(&entry.command);
                }
                None => {}
            }
        }
    }

    /// Before an app's window's first configure: if a saved entry waits for its app, size it to the saved frame.
    /// `true`: claimed (the remembered frame isn't used).
    pub fn session_claim(&mut self, window: &Window) -> bool {
        if self.reopen.is_empty() || window.toplevel().is_none_or(|t| t.parent().is_some()) || self.is_quake(window) {
            return false;
        }
        let Some(entry) = self.app_id(window).and_then(|app| take(&mut self.reopen, &app)) else { return false };
        if let Some(output) = self.space.outputs().find(|o| o.name() == entry.display).cloned() {
            let area = self.output_area(&output);
            let [x, y, w, h] = entry.frame;
            if let (Some(frame), Some(toplevel)) = (area.and_then(|a| crate::wm::recall(Rect::new((x, y).into(), (w, h).into()), a)), window.toplevel()) {
                let size = self.content_size(window, frame.size);
                toplevel.with_pending_state(|s| s.size = Some(size));
                window.user_data().insert_if_missing(|| crate::sheets::Recalled(frame));
            }
        }
        window.user_data().insert_if_missing(|| Reopened(entry));
        true
    }

    /// Once placed: a claimed window goes to its Space and is zoomed or made full screen again.
    pub fn session_place(&mut self, window: &Window) {
        let Some(entry) = window.user_data().get::<Reopened>().map(|r| r.0.clone()) else { return };
        let Some(toplevel) = window.toplevel().cloned() else { return };
        if entry.fullscreen {
            // Its own Space, after the shown one.
            return self.fill(&toplevel, xdg_toplevel::State::Fullscreen);
        }
        if entry.maximized {
            self.fill(&toplevel, xdg_toplevel::State::Maximized);
        }
        if entry.space == 0 {
            return;
        }
        // The Spaces of the display it went back to.
        let dynamic = self.config.windows.spaces == 0;
        let key = self.window_key(window);
        let desks = self.desks_at(&key);
        while dynamic && plain_desk(&desks.list, entry.space).is_none() && desks.list.len() < 16 {
            desks.list.push(None);
        }
        if let Some(desk) = plain_desk(&desks.list, entry.space) {
            self.send_to_space(window, desk);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(app: &str, space: usize) -> Entry {
        Entry { app: app.into(), desktop: None, command: vec![], space, display: "DP-1".into(), frame: [0, 0, 1, 1], maximized: false, fullscreen: false }
    }

    #[test]
    fn round_trips() {
        let entries = vec![
            Entry { desktop: Some("org.kde.kate".into()), frame: [-10, 40, 640, 480], maximized: true, ..entry("org.kde.kate", 2) },
            Entry { command: vec!["my app".into(), "--x=\"q\"\\\n\u{1}".into()], fullscreen: true, ..entry("tool", 0) },
        ];
        assert_eq!(parse(&format(&entries)), entries);
        assert!(parse("").is_empty() && parse("junk [").is_empty());
    }

    #[test]
    fn windows_take_their_apps_entries_in_order() {
        let mut pending = vec![entry("a", 0), entry("b", 1), entry("a", 2)];
        assert_eq!(take(&mut pending, "a").map(|e| e.space), Some(0));
        assert_eq!(take(&mut pending, "c"), None);
        assert_eq!(take(&mut pending, "a").map(|e| e.space), Some(2));
        assert_eq!(take(&mut pending, "a"), None);
        assert_eq!(pending, vec![entry("b", 1)]);
    }

    #[test]
    fn fullscreen_spaces_are_not_counted() {
        let list = [None, Some(()), None, None];
        assert_eq!((plain_index(&list, 0), plain_index(&list, 2), plain_index(&list, 3)), (0, 1, 2));
        assert_eq!((plain_desk(&list, 1), plain_desk(&list, 2), plain_desk(&list, 3)), (Some(2), Some(3), None));
    }
}
