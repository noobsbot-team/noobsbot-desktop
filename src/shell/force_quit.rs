//! The Force Quit window and "not responding" detection (E4.7).
//!
//! Detection is event-driven: an app is pinged (`xdg_wm_base.ping`) when it gets a key or click, when its window is
//! closed, and when the Force Quit window opens; at most one ping per app is out. No pong within 5 s marks the app
//! not responding (its title bar dims, closing it asks Wait / Force Quit); its late pong clears that. Nothing runs
//! while the desktop is idle.

use std::{
    cell::RefCell,
    time::{Duration, Instant},
};

use smithay::{
    desktop::Window,
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_server::{Resource, backend::ClientId},
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Size},
    wayland::shell::xdg::ShellClient,
};

use crate::{
    decorations::{FIRST_LIGHT, LIGHT, LIGHT_STEP},
    shell::{alert, switcher},
    state::State,
    wm::Rect,
};

/// No pong for this long: not responding.
pub const TIMEOUT: Duration = Duration::from_secs(5);
/// An app that answered this recently isn't pinged again, so typing doesn't ping on every key.
const QUIET: Duration = Duration::from_secs(1);
/// After SIGTERM, SIGKILL if the app is still connected.
const KILL_AFTER: Duration = Duration::from_secs(3);

/// The ping state of one app.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Ping {
    /// The unanswered ping went out then.
    sent: Option<Instant>,
    /// The last pong came then.
    answered: Option<Instant>,
    pub frozen: bool,
}

impl Ping {
    /// `true`: send one now (none is waiting for its pong, and the last pong isn't fresh).
    pub fn send(&mut self, now: Instant) -> bool {
        if self.sent.is_some() || self.answered.is_some_and(|t| now.duration_since(t) < QUIET) {
            return false;
        }
        self.sent = Some(now);
        true
    }

    /// The pong came; `true` if the app had been marked not responding.
    pub fn pong(&mut self, now: Instant) -> bool {
        self.sent = None;
        self.answered = Some(now);
        std::mem::take(&mut self.frozen)
    }

    /// A ping's deadline passed; `true` if the app just became not responding.
    pub fn expire(&mut self, now: Instant) -> bool {
        let late = self.sent.is_some_and(|sent| now.duration_since(sent) >= TIMEOUT);
        let became = late && !self.frozen;
        self.frozen |= late;
        became
    }
}

/// Runs `f` on the app's ping state, kept with its xdg_wm_base.
fn with_ping<T>(client: &ShellClient, f: impl FnOnce(&mut Ping) -> T) -> Option<T> {
    client
        .with_data(|data| {
            data.insert_if_missing(RefCell::<Ping>::default);
            data.get::<RefCell<Ping>>().map(|p| f(&mut p.borrow_mut()))
        })
        .ok()
        .flatten()
}

/// The app behind `window` isn't answering pings.
pub fn frozen(window: &Window) -> bool {
    window.toplevel().and_then(|t| with_ping(&t.client(), |p| p.frozen)).unwrap_or(false)
}

/// One row: an app (a client) with a window.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    /// Its theme icon, when the app is installed.
    pub icon: Option<String>,
    pub pid: i32,
    pub client: ClientId,
    pub frozen: bool,
}

/// The window (E4.7): 360 × 320, `sheet`, radius 12, centred.
pub mod layout {
    pub const WIDTH: i32 = 360;
    pub const HEIGHT: i32 = 320;
    pub const RADIUS: f64 = 12.0;
    pub const BAR: i32 = 28;
    pub const PAD: i32 = 16;
    pub const LINE: i32 = 15;
    pub const ROW: i32 = 28;
    pub const ICON: i32 = 20;
    pub const BUTTON_W: i32 = 96;
    pub const BUTTON_H: i32 = 28;
}

/// Where each part of the window is, output-relative.
#[derive(Debug, PartialEq)]
pub struct Layout {
    pub rect: Rect,
    /// Close, minimise, zoom.
    pub lights: [Rect; 3],
    pub title: Rect,
    /// The two lines of explanation.
    pub lines: [Rect; 2],
    pub list: Rect,
    /// One per row that fits.
    pub rows: Vec<Rect>,
    pub button: Rect,
}

pub fn layout(output: Size<i32, Logical>, count: usize) -> Layout {
    use layout::*;
    let (x, y) = (((output.w - WIDTH) / 2).max(0), ((output.h - HEIGHT) / 2).max(0));
    let at = |dx: i32, dy: i32, w: i32, h: i32| Rectangle::new((x + dx, y + dy).into(), (w, h).into());
    let light = |i: i32| {
        let c = FIRST_LIGHT + f64::from(i) * LIGHT_STEP - LIGHT / 2.0;
        at(c.round() as i32, ((f64::from(BAR) - LIGHT) / 2.0).round() as i32, LIGHT as i32, LIGHT as i32)
    };
    let list_y = BAR + 8 + 2 * LINE + 10;
    let button_y = HEIGHT - PAD - BUTTON_H;
    let list = at(PAD, list_y, WIDTH - 2 * PAD, button_y - PAD - list_y);
    let fits = ((list.size.h - 8) / ROW) as usize;
    Layout {
        rect: at(0, 0, WIDTH, HEIGHT),
        lights: [light(0), light(1), light(2)],
        title: at(0, 0, WIDTH, BAR),
        lines: [at(PAD + 4, BAR + 8, WIDTH - 2 * PAD - 4, LINE), at(PAD + 4, BAR + 8 + LINE, WIDTH - 2 * PAD - 4, LINE)],
        rows: (0..count.min(fits) as i32).map(|i| at(PAD + 4, list_y + 4 + i * ROW, WIDTH - 2 * PAD - 8, ROW)).collect(),
        list,
        button: at(WIDTH - PAD - BUTTON_W, button_y, BUTTON_W, BUTTON_H),
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ForceQuit {
    /// The selected app's process.
    pub selected: Option<i32>,
}

impl ForceQuit {
    /// The selected row's process: the chosen one while it's listed, else the first frozen app, else the first.
    pub fn selection(&self, entries: &[Entry]) -> Option<i32> {
        let first = entries.iter().find(|e| e.frozen).or(entries.first()).map(|e| e.pid);
        self.selected.filter(|pid| entries.iter().any(|e| e.pid == *pid)).or(first)
    }
}

impl State {
    /// Pings the app behind `window`, unless a ping is already out; 5 s later, no pong marks it not responding.
    pub fn ping(&mut self, window: &Window) {
        let Some(toplevel) = window.toplevel().cloned() else { return };
        let client = toplevel.client();
        if with_ping(&client, |p| p.send(Instant::now())) != Some(true) {
            return;
        }
        if client.send_ping(SERIAL_COUNTER.next_serial()).is_err() {
            with_ping(&client, |p| p.sent = None);
            return;
        }
        let deadline = self.loop_handle.insert_source(Timer::from_duration(TIMEOUT), move |_, _, state| {
            if with_ping(&toplevel.client(), |p| p.expire(Instant::now())) == Some(true) {
                tracing::info!(app = ?state.window_for(toplevel.wl_surface()).and_then(|w| state.app_id(w)), "not responding");
                state.schedule_redraw();
            }
            TimeoutAction::Drop
        });
        if let Err(err) = deadline {
            tracing::warn!("ping timer: {err}");
        }
    }

    /// An app answered; it shows as responding again.
    pub fn pong(&mut self, client: &ShellClient) {
        if with_ping(client, |p| p.pong(Instant::now())) == Some(true) {
            tracing::info!("responding again");
            self.schedule_redraw();
        }
    }

    /// The close button or Super+W: a frozen app gets the Wait / Force Quit alert instead.
    pub fn close_window(&mut self, window: &Window) {
        self.ping(window);
        if frozen(window)
            && let (Some(pid), Some(client)) = (self.pid(window), window.toplevel().and_then(|t| t.wl_surface().client()))
        {
            let app = self.app_name(window);
            self.open_ask(alert::Kind::NotResponding { app, pid, client: client.id() });
        } else if let Some(toplevel) = window.toplevel() {
            toplevel.send_close();
        }
    }

    fn app_name(&mut self, window: &Window) -> String {
        let id = self.app_id(window).unwrap_or_default();
        self.backend.as_mut().map_or_else(|| id.clone(), |b| b.menu_bar.name(&id))
    }

    /// One row per app process, most recently used first.
    pub fn force_quit_entries(&mut self) -> Vec<Entry> {
        let windows: Vec<Window> = self.switcher_apps().into_iter().flat_map(|a| a.windows).collect();
        let keyed: Vec<_> = windows.into_iter().filter_map(|w| Some((w.toplevel()?.wl_surface().client()?.id(), w))).collect();
        switcher::group(keyed)
            .into_iter()
            .filter_map(|(client, windows)| {
                let window = windows.first()?;
                Some(Entry { name: self.app_name(window), icon: self.app_icon(window), pid: self.pid(window)?, client, frozen: windows.iter().any(frozen) })
            })
            .collect()
    }

    /// Logo menu ▸ Force Quit… or Super+Alt+Esc. Every app is pinged, so frozen ones show as such.
    pub fn open_force_quit(&mut self) {
        self.popup = None;
        let windows: Vec<Window> = self.space.elements().cloned().chain(self.hidden.iter().map(|(w, _)| w.clone())).collect();
        windows.iter().for_each(|w| self.ping(w));
        self.force_quit = Some(ForceQuit::default());
        self.schedule_redraw();
    }

    pub fn close_force_quit(&mut self) -> bool {
        let open = self.force_quit.take().is_some();
        if open {
            self.schedule_redraw();
        }
        open
    }

    /// The layout for this frame's entries.
    pub fn force_quit_layout(&self, count: usize) -> Option<Layout> {
        let size = self.space.outputs().next().and_then(|o| self.space.output_geometry(o))?.size;
        Some(layout(size, count))
    }

    /// A press at `location` (output-relative): `true` if it hit the window.
    pub fn force_quit_press(&mut self, location: Point<f64, Logical>) -> bool {
        if self.force_quit.is_none() {
            return false;
        }
        let entries = self.force_quit_entries();
        let Some(layout) = self.force_quit_layout(entries.len()) else { return false };
        let hit = |r: Rect| r.to_f64().contains(location);
        if !hit(layout.rect) {
            return false;
        }
        if hit(layout.lights[0]) {
            self.close_force_quit();
        } else if hit(layout.button) {
            self.force_quit_selected(&entries);
        } else if let Some(i) = layout.rows.iter().position(|r| hit(*r)) {
            self.force_quit = Some(ForceQuit { selected: Some(entries[i].pid) });
            self.schedule_redraw();
        }
        true
    }

    /// Esc closes, Return force-quits, Up/Down select. `true` if the key was taken.
    pub fn force_quit_key(&mut self, sym: smithay::input::keyboard::Keysym) -> bool {
        use smithay::input::keyboard::Keysym;
        let Some(open) = self.force_quit.clone() else { return false };
        match sym {
            Keysym::Escape => self.close_force_quit(),
            Keysym::Return | Keysym::KP_Enter => {
                let entries = self.force_quit_entries();
                self.force_quit_selected(&entries);
                true
            }
            Keysym::Up | Keysym::Down => {
                let entries = self.force_quit_entries();
                let at = entries.iter().position(|e| Some(e.pid) == open.selection(&entries));
                let next = match (at, sym == Keysym::Up) {
                    (None, _) => 0,
                    (Some(i), true) => i.saturating_sub(1),
                    (Some(i), false) => (i + 1).min(entries.len().saturating_sub(1)),
                };
                self.force_quit = Some(ForceQuit { selected: entries.get(next).map(|e| e.pid) });
                self.schedule_redraw();
                true
            }
            _ => false,
        }
    }

    fn force_quit_selected(&mut self, entries: &[Entry]) {
        let Some(entry) = self.force_quit.as_ref().and_then(|f| entries.iter().find(|e| Some(e.pid) == f.selection(entries))) else { return };
        self.kill(entry.pid, entry.client.clone());
        // macOS keeps the window open on the next app.
        let next = entries.iter().find(|e| e.pid != entry.pid).map(|e| e.pid);
        self.force_quit = Some(ForceQuit { selected: next });
        self.schedule_redraw();
    }

    /// SIGTERM, then SIGKILL after 3 s if the app is still connected.
    pub fn kill(&mut self, pid: i32, client: ClientId) {
        // Never ourselves, init, or a process group.
        if pid <= 1 || pid == std::process::id() as i32 {
            tracing::warn!(pid, "force quit: refusing to signal this process");
            return;
        }
        tracing::info!(pid, "force quit");
        // SAFETY: plain syscall on a pid we got from the client's socket credentials.
        unsafe { libc::kill(pid, libc::SIGTERM) };
        let killed = self.loop_handle.insert_source(Timer::from_duration(KILL_AFTER), move |_, _, state| {
            if state.display_handle.backend_handle().get_client_data(client.clone()).is_ok() {
                tracing::info!(pid, "force quit: SIGKILL");
                // SAFETY: as above; the client is still connected, so the pid is still that app's.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
            TimeoutAction::Drop
        });
        if let Err(err) = killed {
            tracing::warn!("force quit timer: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_times_out_and_recovers() {
        let t0 = Instant::now();
        let mut ping = Ping::default();
        assert!(ping.send(t0));
        assert!(!ping.send(t0 + Duration::from_secs(1)), "one ping out at a time");
        assert!(!ping.expire(t0 + Duration::from_millis(4999)));
        assert!(!ping.frozen);
        assert!(ping.expire(t0 + TIMEOUT), "5 s without a pong");
        assert!(ping.frozen);
        assert!(!ping.expire(t0 + TIMEOUT * 2), "only reported once");
        assert!(!ping.send(t0 + TIMEOUT * 2), "the old ping is still out");
        assert!(ping.pong(t0 + TIMEOUT * 3), "a late pong clears it");
        assert!(!ping.frozen);
        assert!(!ping.send(t0 + TIMEOUT * 3 + QUIET / 2), "just answered");
        assert!(ping.send(t0 + TIMEOUT * 3 + QUIET));
    }

    #[test]
    fn answered_ping_never_freezes() {
        let t0 = Instant::now();
        let mut ping = Ping::default();
        ping.send(t0);
        assert!(!ping.pong(t0));
        assert!(!ping.expire(t0 + TIMEOUT), "the old deadline finds nothing out");
        assert!(!ping.frozen);
    }

    #[test]
    fn window_layout_fits() {
        let l = layout((1280, 800).into(), 3);
        assert_eq!(l.rect, Rectangle::new((460, 240).into(), (360, 320).into()));
        let parts = l.lights.iter().chain(&l.lines).chain(&l.rows).chain([&l.list, &l.button, &l.title]);
        assert!(parts.into_iter().all(|r| l.rect.contains_rect(*r)));
        assert!(l.rows.iter().all(|r| l.list.contains_rect(*r)));
        assert!(!l.list.overlaps(l.button));
        assert_eq!(l.rows.len(), 3);
        // More apps than rows: only what fits.
        let many = layout((1280, 800).into(), 50);
        assert_eq!(many.rows.len(), 6);
    }
}
