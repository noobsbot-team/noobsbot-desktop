//! Title bars (E1.3): which windows get ours, the frame (title bar + app content) the window manager works with, and
//! what a click on a bar does. Every Rect in wm.rs and grabs.rs is a frame; only sizes sent to the app are content.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

use smithay::{
    delegate_kde_decoration, delegate_xdg_decoration,
    desktop::Window,
    input::pointer::GrabStartData,
    reexports::{
        wayland_protocols::xdg::{decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode, shell::server::xdg_toplevel},
        wayland_protocols_misc::server_decoration::server::{org_kde_kwin_server_decoration, org_kde_kwin_server_decoration_manager},
        wayland_server::{DisplayHandle, WEnum, protocol::wl_surface::WlSurface},
    },
    utils::{Logical, Point, Serial, Size},
    wayland::{
        compositor::with_states,
        shell::{
            kde::decoration::{KdeDecorationHandler, KdeDecorationState},
            xdg::{
                ToplevelSurface,
                decoration::{XdgDecorationHandler, XdgDecorationState},
            },
        },
    },
};

use crate::{
    state::State,
    wm::{self, Rect},
};

/// Title bar height.
pub const BAR: i32 = 28;
/// Traffic lights: diameter, centre-to-centre step, centre of the first from the bar's left edge.
pub const LIGHT: f64 = 12.0;
pub const LIGHT_STEP: f64 = 20.0;
pub const FIRST_LIGHT: f64 = 14.0;

/// A part of a title bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Close,
    Minimise,
    Zoom,
    Title,
}

/// The part of a title bar at `p`, relative to the bar's top-left corner.
pub fn part_at(p: Point<f64, Logical>) -> Part {
    let dy = p.y - f64::from(BAR) / 2.0;
    for (i, part) in [Part::Close, Part::Minimise, Part::Zoom].into_iter().enumerate() {
        let dx = p.x - (FIRST_LIGHT + i as f64 * LIGHT_STEP);
        // A little past the circle still counts, so the 12 px lights are easy to hit.
        if dx.hypot(dy) <= LIGHT / 2.0 + 2.0 {
            return part;
        }
    }
    Part::Title
}

pub struct Decorations {
    _xdg: XdgDecorationState,
    kde: KdeDecorationState,
}

impl Decorations {
    pub fn new(dh: &DisplayHandle) -> Self {
        Self { _xdg: XdgDecorationState::new::<State>(dh), kde: KdeDecorationState::new::<State>(dh, org_kde_kwin_server_decoration_manager::Mode::Server) }
    }
}

/// Set on a window while the pointer is over its traffic lights: all three show their glyphs (macOS).
#[derive(Default)]
struct LightsHover(AtomicBool);

pub fn lights_hovered(window: &Window) -> bool {
    window.user_data().get::<LightsHover>().is_some_and(|h| h.0.load(Ordering::Relaxed))
}

/// Set on a surface whose app agreed to our title bar through the older server-decoration protocol (GTK3, Qt 5).
#[derive(Default)]
struct KdeServerSide(AtomicBool);

/// The height of our title bar on `window`: `BAR` if the app agreed (xdg-decoration or server-decoration) and it is not fullscreen.
pub fn bar_height(window: &Window) -> i32 {
    let Some(toplevel) = window.toplevel() else { return 0 };
    let (xdg, fullscreen) = toplevel.with_pending_state(|s| (s.decoration_mode == Some(Mode::ServerSide), s.states.contains(xdg_toplevel::State::Fullscreen)));
    let kde = with_states(toplevel.wl_surface(), |s| s.data_map.get::<KdeServerSide>().is_some_and(|k| k.0.load(Ordering::Relaxed)));
    if (xdg || kde) && !fullscreen { BAR } else { 0 }
}

impl State {
    /// The window's frame: its visible content plus our title bar above it.
    pub fn frame(&self, window: &Window) -> Option<Rect> {
        let mut rect = self.space.element_geometry(window)?;
        let bar = bar_height(window);
        rect.loc.y -= bar;
        rect.size.h += bar;
        Some(rect)
    }

    /// The content size the app gets for a frame of `size`.
    pub fn content_size(&self, window: &Window, size: Size<i32, Logical>) -> Size<i32, Logical> {
        (size.w, (size.h - bar_height(window)).max(1)).into()
    }

    /// The title bar and part at `location`, if a bar is the topmost thing there.
    pub fn bar_under(&self, location: Point<f64, Logical>) -> Option<(Window, Part)> {
        if self.layer_under(location, true).is_some() {
            return None;
        }
        for window in self.space.elements().rev() {
            let Some(frame) = self.frame(window) else { continue };
            let p = location - frame.loc.to_f64();
            if p.x >= 0.0 && p.y >= 0.0 && p.x < f64::from(frame.size.w) && p.y < f64::from(bar_height(window)) {
                return Some((window.clone(), part_at(p)));
            }
            let local = location - (self.space.element_location(window)? - window.geometry().loc).to_f64();
            if frame.to_f64().contains(location) || window.surface_under(local, smithay::desktop::WindowSurfaceType::ALL).is_some() {
                return None;
            }
        }
        None
    }

    /// A press on a title bar: the lights act on release (macOS), the title moves the window or, double-clicked, does
    /// `windows.double_click_title` (zoom by default).
    pub fn bar_press(&mut self, window: Window, part: Part, start_data: GrabStartData<Self>, serial: Serial) {
        if part != Part::Title {
            self.zones.bar_press = Some((window, part));
            return;
        }
        let double = self.zones.last_title_press.take().is_some_and(|(at, w)| at.elapsed() < wm::DOUBLE_CLICK && w == window);
        if double {
            match self.config.windows.double_click_title {
                crate::config::TitleAction::Zoom => self.toggle_zoom(&window),
                crate::config::TitleAction::Minimise => self.minimise(vec![window]),
                crate::config::TitleAction::Nothing => {}
            }
            return;
        }
        self.zones.last_title_press = Some((Instant::now(), window.clone()));
        if let Some(toplevel) = window.toplevel().cloned()
            && let Some(restore) = self.unfill(&toplevel, xdg_toplevel::State::Maximized)
        {
            // Dragging a zoomed window un-zooms it first, under the pointer (macOS).
            let at = start_data.location.to_i32_round::<i32>();
            self.set_visible_loc(&window, (at.x - restore.size.w / 2, at.y - BAR / 2).into());
        }
        self.start_move(window, start_data, serial);
    }

    /// The button went up: a light acts if the pointer is still on the one that was pressed.
    pub fn bar_release(&mut self, location: Point<f64, Logical>) {
        let Some((window, part)) = self.zones.bar_press.take() else { return };
        if self.bar_under(location) != Some((window.clone(), part)) {
            return;
        }
        match part {
            Part::Close => self.close_window(&window),
            Part::Minimise => self.minimise(vec![window]),
            // macOS: the green light is full screen, Alt+green zoom.
            Part::Zoom if self.modifiers().alt => self.toggle_zoom(&window),
            Part::Zoom => self.toggle_fullscreen(&window),
            Part::Title => {}
        }
    }

    pub fn set_lights_hover(&mut self, window: Option<Window>) {
        if self.zones.lights_hover == window {
            return;
        }
        for (w, on) in [(self.zones.lights_hover.take(), false), (window.clone(), true)] {
            if let Some(w) = w {
                w.user_data().get_or_insert_threadsafe(LightsHover::default).0.store(on, Ordering::Relaxed);
            }
        }
        self.zones.lights_hover = window;
        self.schedule_redraw();
    }

    pub fn toggle_fullscreen(&mut self, window: &Window) {
        let Some(toplevel) = window.toplevel().cloned() else { return };
        if self.unfill(&toplevel, xdg_toplevel::State::Fullscreen).is_none() {
            self.fill(&toplevel, xdg_toplevel::State::Fullscreen);
        }
    }

    pub fn toggle_zoom(&mut self, window: &Window) {
        let Some(toplevel) = window.toplevel().cloned() else { return };
        if self.unfill(&toplevel, xdg_toplevel::State::Maximized).is_none() {
            self.fill(&toplevel, xdg_toplevel::State::Maximized);
        }
    }

    /// Changes whether a window has our title bar, keeping its frame where it was so the content doesn't jump.
    fn change_decoration(&mut self, surface: &WlSurface, change: impl FnOnce()) {
        let before = self.window_for(surface).cloned().and_then(|w| Some((self.frame(&w)?, w)));
        change();
        if let Some((frame, window)) = before {
            self.set_visible_loc(&window, frame.loc);
        }
    }

    /// The mode a window gets: what the app asked for, unless a rule (`decorations = false`) says no title bar.
    fn decoration_mode(&self, toplevel: &ToplevelSurface, wanted: Mode) -> Mode {
        if self.resolve_rules(toplevel).decorations == Some(false) { Mode::ClientSide } else { wanted }
    }

    fn set_xdg_decoration(&mut self, toplevel: ToplevelSurface, wanted: Mode) {
        let mode = self.decoration_mode(&toplevel, wanted);
        self.change_decoration(toplevel.wl_surface(), || toplevel.with_pending_state(|s| s.decoration_mode = Some(mode)));
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    fn set_kde_decoration(&mut self, surface: &WlSurface, server: bool) {
        self.change_decoration(surface, || {
            with_states(surface, |s| s.data_map.get_or_insert_threadsafe(KdeServerSide::default).0.store(server, Ordering::Relaxed));
        });
    }
}

/// A row of the window menu (E8.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Minimise,
    Zoom,
    Fill,
    Center,
    /// `TILES[n]`.
    Tile(usize),
    Fullscreen,
    KeepOnTop,
    /// Space n, from 0.
    ToSpace(usize),
    ToDisplay,
    Close,
}

/// Move & Resize: a name and the work-area edges it tiles against.
pub const TILES: [(&str, wm::Edges); 8] = {
    const fn e(left: bool, right: bool, top: bool, bottom: bool) -> wm::Edges {
        wm::Edges { left, right, top, bottom }
    }
    [
        ("Left", e(true, false, false, false)),
        ("Right", e(false, true, false, false)),
        ("Top", e(false, false, true, false)),
        ("Bottom", e(false, false, false, true)),
        ("Top Left", e(true, false, true, false)),
        ("Top Right", e(false, true, true, false)),
        ("Bottom Left", e(true, false, false, true)),
        ("Bottom Right", e(false, true, false, true)),
    ]
};

/// The window menu: `full` (a full-screen window) greys what would move or resize it; Keep on Top ticked while
/// `on_top`; `spaces` (name, can move there) and Next Display with more than one of `displays`.
// ponytail: Move & Resize and Move to are inline sections under a greyed heading, since menus have no submenus yet.
pub fn menu_rows(full: bool, on_top: bool, spaces: &[(String, bool)], displays: usize) -> Vec<Option<crate::shell::Row>> {
    let row = |label: &str, shortcut, command: Option<Command>| {
        Some(crate::shell::Row { label: label.to_owned(), shortcut, action: command.map(crate::shortcuts::Action::WindowMenu) })
    };
    let free = |command| (!full).then_some(command);
    let mut rows = vec![
        row("Minimize", "", free(Command::Minimise)),
        row("Zoom", "", free(Command::Zoom)),
        row("Fill", "", free(Command::Fill)),
        row("Center", "", free(Command::Center)),
        None,
        row("Move & Resize", "", None),
    ];
    rows.extend(TILES.iter().enumerate().map(|(i, (name, ..))| row(&format!("    {name}"), "", free(Command::Tile(i)))));
    rows.extend([
        None,
        row(if full { "Exit Full Screen" } else { "Enter Full Screen" }, "", Some(Command::Fullscreen)),
        row("Keep on Top", if on_top { "✓" } else { "" }, Some(Command::KeepOnTop)),
        None,
        row("Move to", "", None),
    ]);
    rows.extend(spaces.iter().enumerate().map(|(i, (name, can))| row(&format!("    {name}"), "", (*can && !full).then_some(Command::ToSpace(i)))));
    rows.push(row("    Next Display", "", (displays > 1).then_some(Command::ToDisplay)));
    rows.extend([None, row("Close", "", Some(Command::Close))]);
    rows
}

impl State {
    /// Opens the window menu for `window` at `at` (output-relative): a right click on its title bar, or the app asked.
    pub fn open_window_menu(&mut self, window: Window, at: Point<f64, Logical>) {
        self.zones.window_menu = Some((window, at.to_i32_round()));
        self.popup = Some(crate::shell::Popup::Menu(crate::shell::Menu::Window));
        self.schedule_redraw();
    }

    pub fn window_menu_rows(&mut self) -> Vec<Option<crate::shell::Row>> {
        let Some((window, _)) = self.zones.window_menu.clone() else { return Vec::new() };
        let full = window.toplevel().is_some_and(|t| t.with_pending_state(|s| s.states.contains(xdg_toplevel::State::Fullscreen)));
        let on_top = crate::state::on_top(&window);
        let here = self.desk_of_window(&window);
        let list = self.desks().map(|d| d.list.clone()).unwrap_or_default();
        let (names, _) = self.desk_names();
        // Not its own Space, nor another window's full-screen Space.
        let spaces: Vec<_> = names.into_iter().enumerate().map(|(i, n)| (n, i != here && list.get(i).is_none_or(Option::is_none))).collect();
        menu_rows(full, on_top, &spaces, self.space.outputs().count())
    }

    pub fn window_menu_anchor(&self) -> Option<crate::shell::Anchor> {
        self.zones.window_menu.as_ref().map(|(_, at)| crate::shell::Anchor::At(*at))
    }

    pub fn window_menu_command(&mut self, command: Command) {
        let Some((window, _)) = self.zones.window_menu.take() else { return };
        if !smithay::utils::IsAlive::alive(&window) {
            return;
        }
        match command {
            Command::Minimise => self.minimise(vec![window]),
            Command::Zoom => self.toggle_zoom(&window),
            Command::Fill => self.tile_to(&window, wm::Edges::default()),
            Command::Center => self.center(&window),
            Command::Tile(n) => self.tile_to(&window, TILES[n].1),
            Command::Fullscreen => self.toggle_fullscreen(&window),
            Command::KeepOnTop => self.keep_on_top(&window),
            Command::ToSpace(n) => self.move_window_to_space(&window, n),
            Command::ToDisplay => self.move_to_display(&window, 1),
            Command::Close => self.close_window(&window),
        }
    }
}

impl XdgDecorationHandler for State {
    /// Every app that can leave the title bar to us gets ours.
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        self.set_xdg_decoration(toplevel, Mode::ServerSide);
    }

    /// An app that insists on its own title bar keeps it (Helium with its tabs in the title bar, F2).
    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: Mode) {
        self.set_xdg_decoration(toplevel, mode);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.set_xdg_decoration(toplevel, Mode::ServerSide);
    }
}

impl KdeDecorationHandler for State {
    fn kde_decoration_state(&self) -> &KdeDecorationState {
        &self.decorations.kde
    }

    fn new_decoration(&mut self, surface: &WlSurface, decoration: &org_kde_kwin_server_decoration::OrgKdeKwinServerDecoration) {
        KdeDecorationHandler::request_mode(self, surface, decoration, WEnum::Value(org_kde_kwin_server_decoration::Mode::Server));
    }

    fn request_mode(
        &mut self,
        surface: &WlSurface,
        decoration: &org_kde_kwin_server_decoration::OrgKdeKwinServerDecoration,
        mode: WEnum<org_kde_kwin_server_decoration::Mode>,
    ) {
        use org_kde_kwin_server_decoration::Mode as Kde;
        let ruled_off =
            self.window_for(surface).and_then(|w| w.toplevel().cloned()).is_some_and(|t| self.decoration_mode(&t, Mode::ServerSide) == Mode::ClientSide);
        let server = !ruled_off && mode == WEnum::Value(Kde::Server);
        // Echo the mode back: the app only switches its own title bar off once we confirm.
        decoration.mode(if server { Kde::Server } else { Kde::Client });
        self.set_kde_decoration(surface, server);
    }

    fn release(&mut self, _decoration: &org_kde_kwin_server_decoration::OrgKdeKwinServerDecoration, surface: &WlSurface) {
        self.set_kde_decoration(surface, false);
    }
}

delegate_xdg_decoration!(State);
delegate_kde_decoration!(State);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lights_and_title() {
        let at = |x, y| part_at(Point::from((x, y)));
        assert_eq!(at(14.0, 14.0), Part::Close);
        assert_eq!(at(34.0, 14.0), Part::Minimise);
        assert_eq!(at(54.0, 14.0), Part::Zoom);
        assert_eq!(at(21.0, 14.0), Part::Close, "just past the circle");
        assert_eq!(at(24.0, 14.0), Part::Title, "the gap between lights is title");
        assert_eq!(at(200.0, 14.0), Part::Title);
        assert_eq!(at(14.0, 2.0), Part::Title, "above the light");
    }

    #[test]
    fn window_menu_rows() {
        let spaces = [("Desktop 1".to_owned(), false), ("Desktop 2".to_owned(), true)];
        let rows = menu_rows(false, true, &spaces, 1);
        let find = |rows: &[Option<crate::shell::Row>], label: &str| rows.iter().flatten().find(|r| r.label.trim() == label).cloned().unwrap();
        let labels: Vec<_> = rows.iter().map(|r| r.as_ref().map(|r| r.label.trim().to_owned())).collect();
        assert_eq!(labels.len(), 24);
        assert_eq!(labels[..6], [Some("Minimize".into()), Some("Zoom".into()), Some("Fill".into()), Some("Center".into()), None, Some("Move & Resize".into())]);
        assert_eq!(labels.last(), Some(&Some("Close".to_owned())));
        assert_eq!(find(&rows, "Keep on Top").shortcut, "✓");
        assert_eq!(find(&menu_rows(false, false, &spaces, 1), "Keep on Top").shortcut, "");
        let greyed =
            |rows: &[Option<crate::shell::Row>]| rows.iter().flatten().filter(|r| r.action.is_none()).map(|r| r.label.trim().to_owned()).collect::<Vec<_>>();
        assert_eq!(greyed(&rows), ["Move & Resize", "Move to", "Desktop 1", "Next Display"]);
        assert_eq!(find(&rows, "Desktop 2").action, Some(crate::shortcuts::Action::WindowMenu(Command::ToSpace(1))));
        assert!(find(&menu_rows(false, false, &spaces, 2), "Next Display").action.is_some());
        // Full screen: only Exit Full Screen, Keep on Top, displays and Close stay.
        let full = menu_rows(true, false, &spaces, 2);
        assert!(find(&full, "Exit Full Screen").action.is_some());
        assert_eq!(full.iter().flatten().filter(|r| r.action.is_some()).count(), 4);
    }
}
