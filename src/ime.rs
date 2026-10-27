//! Input methods (E6.12): input-method-v2 for fcitx5 (or ibus) behind the text-input-v3 that Qt 6, GTK 4 and Chromium speak,
//! the candidate popup placed under the text cursor, and the keyboard layouts the menu bar's input item lists.

use smithay::{
    delegate_input_method_manager,
    desktop::{PopupKind, PopupManager, utils::bbox_from_surface_tree},
    input::keyboard::Layout,
    reexports::{
        wayland_protocols::wp::text_input::zv3::server::{
            zwp_text_input_manager_v3::ZwpTextInputManagerV3,
            zwp_text_input_v3::{self, ZwpTextInputV3},
        },
        wayland_server::{Client, DataInit, Dispatch, DisplayHandle, Resource, backend::ClientId, protocol::wl_surface::WlSurface},
    },
    utils::{Logical, Point, Rectangle, Size},
    wayland::{
        input_method::{InputMethodHandler, PopupSurface},
        seat::WaylandFocus,
        text_input::{TextInputManagerState, TextInputSeat, TextInputUserData},
    },
};

use crate::{
    shell::{Osd, Row},
    shortcuts::Action,
    state::State,
    symbols::Symbol,
};

/// The keyboard's xkb layouts, as `[keyboard] layout = "us,np"` lists them.
#[derive(Debug, Default)]
pub struct Layouts {
    /// (code, name): ("np", "Nepali").
    pub list: Vec<(String, String)>,
    /// Index into `list`.
    // ponytail: only our own switches update it; an xkb `grp:` toggle option changes the layout behind its back.
    pub active: usize,
    /// The engine's settings app when it is installed (`fcitx5-configtool`, `ibus-setup`): the menu offers it.
    pub ime_settings: Option<&'static str>,
    /// fcitx5's or ibus's input methods while it runs.
    pub methods: Option<crate::services::fcitx::Fcitx>,
    /// `[keyboard] per_window_source`: each surface's layout when it lost focus.
    pub remembered: Remembered<WlSurface>,
}

/// The layout each window had when it lost focus, and the window with focus now.
#[derive(Debug)]
pub struct Remembered<K> {
    focused: Option<K>,
    by: Vec<(K, usize)>,
}

impl<K> Default for Remembered<K> {
    fn default() -> Self {
        Self { focused: None, by: Vec::new() }
    }
}

impl<K: PartialEq + Clone> Remembered<K> {
    /// Focus moves to `to` while layout `active` is on: the window losing focus keeps `active`; `to`'s own layout,
    /// if it has one, comes back.
    pub fn focus(&mut self, to: Option<&K>, active: usize) -> Option<usize> {
        if let Some(from) = self.focused.take() {
            match self.by.iter_mut().find(|(k, _)| *k == from) {
                Some(entry) => entry.1 = active,
                None => self.by.push((from, active)),
            }
        }
        self.focused = to.cloned();
        to.and_then(|to| self.by.iter().find(|(k, _)| k == to)).map(|(_, i)| *i)
    }

    pub fn retain(&mut self, keep: impl Fn(&K) -> bool) {
        self.by.retain(|(k, _)| keep(k));
    }
}

/// One text-input-v3 object as its requests left it. smithay drops every request while no input method runs, so we
/// read them first: the emoji picker (E7.2) commits into the field and opens at its cursor, fcitx5 or not.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Field {
    /// Commit requests so far: `done` must carry it.
    serial: u32,
    enabled: bool,
    /// The text cursor, surface-local.
    cursor: Option<Rectangle<i32, Logical>>,
    /// Double-buffered until the next commit.
    pending: (Option<bool>, Option<Rectangle<i32, Logical>>),
}

impl Field {
    fn request(&mut self, request: &zwp_text_input_v3::Request) {
        match *request {
            zwp_text_input_v3::Request::Enable => self.pending.0 = Some(true),
            zwp_text_input_v3::Request::Disable => self.pending.0 = Some(false),
            zwp_text_input_v3::Request::SetCursorRectangle { x, y, width, height } => {
                self.pending.1 = Some(Rectangle::new((x, y).into(), (width, height).into()));
            }
            zwp_text_input_v3::Request::Commit => {
                self.serial = self.serial.wrapping_add(1);
                let (enabled, cursor) = std::mem::take(&mut self.pending);
                // Enabling starts from a clean state (the protocol's reset).
                if enabled == Some(true) {
                    self.cursor = None;
                }
                self.enabled = enabled.unwrap_or(self.enabled);
                self.cursor = cursor.or(self.cursor);
            }
            _ => {}
        }
    }
}

#[derive(Debug, Default)]
pub struct TextInputs {
    fields: Vec<(ZwpTextInputV3, Field)>,
    /// The surface we sent text-input `enter` for; smithay only does while an input method runs.
    entered: Option<WlSurface>,
}

/// Pairs the configured codes with xkb's names; a missing code (the layout fell back to xkb's default) becomes the
/// name's first two letters.
fn pair(codes: &str, names: Vec<String>) -> Vec<(String, String)> {
    let mut codes = codes.split(',').map(str::trim);
    names
        .into_iter()
        .map(|name| {
            let code = codes.next().filter(|c| !c.is_empty()).map_or_else(|| name.chars().take(2).collect::<String>().to_lowercase(), str::to_owned);
            (code, name)
        })
        .collect()
}

/// Where the candidate popup goes, in global coordinates: under the text cursor, or above it when there's no room
/// below, kept on `screen` sideways.
pub(crate) fn place(cursor: Rectangle<i32, Logical>, size: Size<i32, Logical>, screen: Rectangle<i32, Logical>) -> Point<i32, Logical> {
    let right = screen.loc.x + screen.size.w - size.w;
    let x = cursor.loc.x.min(right).max(screen.loc.x);
    let below = cursor.loc.y + cursor.size.h;
    let y = if below + size.h <= screen.loc.y + screen.size.h { below } else { (cursor.loc.y - size.h).max(screen.loc.y) };
    (x, y).into()
}

/// The input menu's rows: layouts, the engine's methods (a click switches), its settings.
fn menu_rows(layouts: &Layouts) -> Vec<Option<Row>> {
    let tick = |on: bool| if on { "✓" } else { "" };
    let mut rows: Vec<Option<Row>> = layouts
        .list
        .iter()
        .enumerate()
        .map(|(i, (_, name))| Some(Row { sub: Vec::new(), label: name.clone(), shortcut: tick(i == layouts.active), action: Some(Action::Layout(Some(i))) }))
        .collect();
    if let Some(fcitx) = layouts.methods.as_ref().filter(|f| !f.methods.is_empty()) {
        rows.push(None);
        rows.extend(fcitx.methods.iter().map(|(id, name)| {
            Some(Row { sub: Vec::new(), label: name.clone(), shortcut: tick(*id == fcitx.current), action: Some(Action::InputMethod(id.clone())) })
        }));
    }
    if let Some(program) = layouts.ime_settings {
        rows.push(None);
        rows.push(Some(Row { sub: Vec::new(), label: "Input Method Settings…".into(), shortcut: "", action: Some(Action::Spawn(program.into())) }));
    }
    rows
}

/// The engine's settings app, when the engine is installed.
fn ime_settings(engine: &str) -> Option<&'static str> {
    match engine {
        "fcitx5" if crate::ibus::on_path("fcitx5") => Some("fcitx5-configtool"),
        "ibus" if crate::ibus::on_path("ibus") => Some("ibus-setup"),
        _ => None,
    }
}

/// The layout `by` steps from `active` among `count`, wrapping round.
pub(crate) fn step(active: usize, count: usize, by: isize) -> usize {
    (active as isize + by).rem_euclid(count.max(1) as isize) as usize
}

/// The engine's method `by` steps from its current one, when it lists more than one.
fn method_step(fcitx: &crate::services::fcitx::Fcitx, by: isize) -> Option<&(String, String)> {
    let at = fcitx.methods.iter().position(|m| m.0 == fcitx.current).unwrap_or(0);
    (fcitx.methods.len() > 1).then(|| &fcitx.methods[step(at, fcitx.methods.len(), by)])
}

impl State {
    /// Super+Ctrl+Space (`by` 1) or with Shift (-1): the next or previous input method while the engine lists several,
    /// else layout; the OSD says which.
    // ponytail: fcitx5 tells nobody when its own hotkey switches, so this steps from the method we last read.
    pub fn cycle_input(&mut self, by: isize) {
        let Some((id, name)) = self.layouts.methods.as_ref().and_then(|f| method_step(f, by)).cloned() else {
            return self.switch_layout(Some(step(self.layouts.active, self.layouts.list.len(), by)));
        };
        if let Some(methods) = self.layouts.methods.as_mut() {
            // At once, so presses faster than the engine's echo still step on.
            methods.current = id.clone();
        }
        let ibus = self.config.keyboard.engine() == "ibus";
        self.services.send(crate::services::Command::InputMethod { name: id, ibus });
        self.show_osd(Osd { symbol: Symbol::Keyboard, level: None, text: Some(name) });
    }

    /// Reads the keymap's layouts after the keyboard got a new xkb config; `codes` is its `layout` string.
    pub fn read_layouts(&mut self, codes: &str) {
        let Some(keyboard) = self.seat.get_keyboard() else { return };
        let names = keyboard.with_xkb_state(self, |ctx| {
            let xkb = ctx.xkb().lock().unwrap();
            xkb.layouts().map(|l| xkb.layout_name(l).to_owned()).collect()
        });
        let methods = self.layouts.methods.take();
        self.layouts = Layouts { list: pair(codes, names), ime_settings: ime_settings(self.config.keyboard.engine()), methods, ..Default::default() };
    }

    /// Keyboard focus moved to `focused`: with `[keyboard] per_window_source`, it gets its own layout back, on the next
    /// idle while it still has focus. smithay calls this from `focus_changed` holding the keyboard's lock, which
    /// `with_xkb_state` takes too.
    pub fn layout_focus(&mut self, focused: Option<&WlSurface>) {
        if !self.config.keyboard.per_window_source {
            return;
        }
        let focused = focused.cloned();
        self.loop_handle.insert_idle(move |state| {
            let keyboard = state.seat.get_keyboard();
            if keyboard.as_ref().and_then(|k| k.current_focus()) != focused || focused.as_ref().is_some_and(|s| !s.is_alive()) {
                return;
            }
            let remembered = &mut state.layouts.remembered;
            remembered.retain(WlSurface::is_alive);
            let index = remembered.focus(focused.as_ref(), state.layouts.active).filter(|&i| i != state.layouts.active && i < state.layouts.list.len());
            if let (Some(index), Some(keyboard)) = (index, keyboard) {
                keyboard.with_xkb_state(state, |mut ctx| ctx.set_layout(Layout(index as u32)));
                state.layouts.active = index;
            }
        });
    }

    /// Switches to layout `index` (past the last one wraps round), or the next one (Super+Ctrl+Space), and says which
    /// in the OSD.
    pub fn switch_layout(&mut self, index: Option<usize>) {
        let count = self.layouts.list.len();
        let (Some(keyboard), true) = (self.seat.get_keyboard(), count > 1) else { return };
        let index = index.map_or_else(|| step(self.layouts.active, count, 1), |i| step(i, count, 0));
        keyboard.with_xkb_state(self, |mut ctx| ctx.set_layout(Layout(index as u32)));
        self.layouts.active = index;
        let text = Some(self.layouts.list[index].1.clone());
        self.show_osd(Osd { symbol: Symbol::Keyboard, level: None, text });
    }

    /// The menu bar's input item: the active layout's code, shown with more than one layout or with an input-method engine.
    pub fn input_item(&self) -> Option<String> {
        let layouts = &self.layouts;
        let code = layouts.list.get(layouts.active).map_or_else(|| "EN".into(), |(code, _)| code.to_uppercase());
        (layouts.list.len() > 1 || layouts.ime_settings.is_some() || layouts.methods.is_some()).then_some(code)
    }

    /// The input menu: `menu_rows`, the Keyboard Viewer and the on-screen keyboard.
    pub fn input_menu_rows(&self) -> Vec<Option<Row>> {
        let label = if self.osk_shown() { "Hide On-Screen Keyboard" } else { "Show On-Screen Keyboard" };
        let viewer = if self.osk.viewer { "Hide Keyboard Viewer" } else { "Show Keyboard Viewer" };
        let mut rows = menu_rows(&self.layouts);
        rows.extend([
            None,
            Some(Row { sub: Vec::new(), label: viewer.into(), shortcut: "", action: Some(Action::KeyboardViewer) }),
            Some(Row { sub: Vec::new(), label: label.into(), shortcut: "", action: Some(Action::OnScreenKeyboard) }),
        ]);
        rows
    }

    /// Moves an input-method popup to under its text cursor, now that its size or the cursor changed.
    fn place_ime_popup(&self, popup: &PopupSurface) {
        let cursor = popup.text_input_rectangle();
        let below = Point::from((cursor.loc.x, cursor.loc.y + cursor.size.h));
        // The parent's surface origin on screen; only windows are known here.
        let parent = popup.get_parent().map(|p| &p.surface);
        let window = self.space.elements().find(|w| w.wl_surface().as_deref() == parent);
        let Some((window, geo)) = window.and_then(|w| Some((w, self.space.element_geometry(w)?))) else {
            // ponytail: a popup or layer surface parent gets no screen-edge flip; add when such a text field shows up.
            popup.set_location(below);
            return;
        };
        let origin = geo.loc - window.geometry().loc;
        let screen = self.space.outputs().filter_map(|o| self.space.output_geometry(o)).find(|g| g.contains(origin + cursor.loc));
        let size = bbox_from_surface_tree(popup.wl_surface(), (0, 0)).size;
        let at = screen.map_or(origin + below, |screen| place(Rectangle::new(origin + cursor.loc, cursor.size), size, screen));
        popup.set_location(at - origin);
    }

    /// Keyboard focus moved to `focused`: without an input method smithay sends text-input `enter`/`leave` to nobody,
    /// so apps never enable their text fields; send them ourselves.
    // ponytail: smithay hides whether an input method runs, so with fcitx5 the app gets enter/leave twice (Qt and GTK
    // just enable again); intercept zwp_input_method_v2 the way text input is, if a client minds.
    pub fn text_input_focus(&mut self, focused: Option<&WlSurface>) {
        self.zoom.refocus();
        let handle = self.seat.text_input().clone();
        if let Some(old) = self.text_inputs.entered.take().filter(|old| old.is_alive() && Some(old) != focused) {
            handle.set_focus(Some(old));
            handle.leave();
            handle.set_focus(focused.cloned());
        }
        if focused.is_some() {
            handle.enter();
        }
        self.text_inputs.entered = focused.cloned();
    }

    /// The enabled text field of the app with keyboard focus, and its surface.
    fn focused_field(&self) -> Option<(ZwpTextInputV3, Field, WlSurface)> {
        let focus = self.seat.text_input().focus()?;
        let (input, field) = self.text_inputs.fields.iter().find(|(t, f)| f.enabled && t.is_alive() && t.id().same_client_as(&focus.id()))?;
        Some((input.clone(), *field, focus))
    }

    /// Types `text` into the focused text field, as an input method would; `false` when there is none.
    pub fn commit_text(&mut self, text: &str) -> bool {
        let Some((input, field, _)) = self.focused_field() else { return false };
        input.commit_string(Some(text.to_owned()));
        input.done(field.serial);
        true
    }

    /// The focused text field's cursor, in global coordinates, when its app says where it is.
    pub fn text_cursor(&self) -> Option<Rectangle<i32, Logical>> {
        let (_, field, surface) = self.focused_field()?;
        let window = self.space.elements().find(|w| w.wl_surface().as_deref() == Some(&surface))?;
        let origin = self.space.element_geometry(window)?.loc - window.geometry().loc;
        field.cursor.map(|c| Rectangle::new(origin + c.loc, c.size))
    }

    /// An input-method popup committed: its size may have changed.
    pub fn ime_commit(&mut self, surface: &WlSurface) {
        if let Some(PopupKind::InputMethod(popup)) = self.popups.find_popup(surface) {
            self.place_ime_popup(&popup);
        }
    }
}

impl InputMethodHandler for State {
    fn new_popup(&mut self, surface: PopupSurface) {
        self.place_ime_popup(&surface);
        if let Err(err) = self.popups.track_popup(PopupKind::from(surface)) {
            tracing::warn!("input method popup: {err}");
        }
    }

    fn dismiss_popup(&mut self, surface: PopupSurface) {
        if let Some(parent) = surface.get_parent().map(|p| p.surface.clone()) {
            let _ = PopupManager::dismiss_popup(&parent, &PopupKind::from(surface));
        }
        self.schedule_redraw();
    }

    fn popup_repositioned(&mut self, surface: PopupSurface) {
        self.place_ime_popup(&surface);
        self.schedule_redraw();
    }

    /// The window geometry, so the popup's location stays relative to the text field's surface (see
    /// `render::window_elements`).
    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
        self.space.elements().find(|w| w.wl_surface().as_deref() == Some(parent)).map(|w| w.geometry()).unwrap_or_default()
    }
}

delegate_input_method_manager!(State);

// smithay's `delegate_text_input_manager!`, but each text-input request passes `Field::request` first.
smithay::reexports::wayland_server::delegate_global_dispatch!(State: [ZwpTextInputManagerV3: ()] => TextInputManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(State: [ZwpTextInputManagerV3: ()] => TextInputManagerState);

impl Dispatch<ZwpTextInputV3, TextInputUserData> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &ZwpTextInputV3,
        request: zwp_text_input_v3::Request,
        data: &TextInputUserData,
        dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match state.text_inputs.fields.iter_mut().find(|(t, _)| t == resource) {
            Some((_, field)) => field.request(&request),
            None => {
                let mut field = Field::default();
                field.request(&request);
                state.text_inputs.fields.push((resource.clone(), field));
            }
        }
        <TextInputManagerState as Dispatch<ZwpTextInputV3, TextInputUserData, State>>::request(state, client, resource, request, data, dh, init);
    }

    fn destroyed(state: &mut Self, client: ClientId, resource: &ZwpTextInputV3, data: &TextInputUserData) {
        state.text_inputs.fields.retain(|(t, _)| t != resource);
        <TextInputManagerState as Dispatch<ZwpTextInputV3, TextInputUserData, State>>::destroyed(state, client, resource, data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_pair_with_names() {
        let names = || vec!["English (US)".to_string(), "Nepali".to_string()];
        assert_eq!(pair("us, np", names()), [("us".into(), "English (US)".into()), ("np".into(), "Nepali".into())]);
        assert_eq!(pair("", names()), [("en".into(), "English (US)".into()), ("ne".into(), "Nepali".into())]);
    }

    #[test]
    fn each_window_keeps_its_layout() {
        let mut r = Remembered::default();
        assert_eq!(r.focus(Some(&"editor"), 0), None, "new: stays as is");
        // Switched to layout 1 in the editor, then to the terminal: it hasn't one of its own.
        assert_eq!(r.focus(Some(&"terminal"), 1), None);
        assert_eq!(r.focus(Some(&"editor"), 0), Some(1));
        assert_eq!(r.focus(None, 1), None, "the shell has focus");
        assert_eq!(r.focus(Some(&"terminal"), 1), Some(0));
        // The editor closed.
        r.retain(|k| *k != "editor");
        assert_eq!(r.focus(Some(&"editor"), 0), None, "forgotten");
    }

    #[test]
    fn methods_cycle_from_the_current_one() {
        let methods = ["keyboard-us", "m17n_ne_rom", "pinyin"].map(|id| (id.to_owned(), id.to_uppercase())).to_vec();
        let mut fcitx = crate::services::fcitx::Fcitx { methods, current: "pinyin".into() };
        assert_eq!(method_step(&fcitx, 1).unwrap().0, "keyboard-us");
        assert_eq!(method_step(&fcitx, -1).unwrap().0, "m17n_ne_rom");
        fcitx.methods.truncate(1);
        assert_eq!(method_step(&fcitx, 1), None, "one method: the layouts cycle");
    }

    #[test]
    fn steps_wrap_round() {
        assert_eq!(step(0, 3, 1), 1);
        assert_eq!(step(2, 3, 1), 0);
        assert_eq!(step(0, 3, -1), 2);
        assert_eq!(step(1, 3, -1), 0);
        assert_eq!(step(0, 0, -1), 0);
        // "input-source 5" (index 4) with three layouts: the second.
        assert_eq!(step(4, 3, 0), 1);
    }

    #[test]
    fn menu_lists_fcitx_methods_while_it_runs() {
        let mut layouts = Layouts { list: vec![("us".into(), "English (US)".into())], ..Default::default() };
        assert_eq!(menu_rows(&layouts).len(), 1);
        let methods = vec![("keyboard-us".into(), "English (US)".into()), ("m17n_ne_rom".into(), "Nepali (Romanized)".into())];
        layouts.methods = Some(crate::services::fcitx::Fcitx { methods, current: "m17n_ne_rom".into() });
        layouts.ime_settings = Some("fcitx5-configtool");
        let rows = menu_rows(&layouts);
        // Layout, gap, two methods, gap, settings.
        assert_eq!(rows.len(), 6);
        assert!(rows[1].is_none() && rows[4].is_none());
        let nepali = rows[3].as_ref().unwrap();
        assert_eq!((nepali.shortcut, &nepali.action), ("✓", &Some(Action::InputMethod("m17n_ne_rom".into()))));
        assert_eq!(rows[2].as_ref().unwrap().shortcut, "");
    }

    #[test]
    fn field_state_applies_on_commit() {
        use zwp_text_input_v3::Request;
        let mut f = Field::default();
        f.request(&Request::Enable);
        f.request(&Request::SetCursorRectangle { x: 5, y: 6, width: 1, height: 18 });
        assert!(!f.enabled && f.cursor.is_none(), "double-buffered");
        f.request(&Request::Commit);
        assert_eq!((f.serial, f.enabled, f.cursor), (1, true, Some(Rectangle::new((5, 6).into(), (1, 18).into()))));
        // A commit without a new rectangle keeps the old one; disabling counts as a commit too.
        f.request(&Request::Commit);
        f.request(&Request::Disable);
        f.request(&Request::Commit);
        assert_eq!((f.serial, f.enabled, f.cursor.is_some()), (3, false, true));
    }

    #[test]
    fn popup_goes_below_else_above() {
        let screen = Rectangle::new((0, 0).into(), (1000, 800).into());
        let size = Size::from((300, 100));
        let cursor = |x, y| Rectangle::new((x, y).into(), (2, 20).into());
        assert_eq!(place(cursor(100, 100), size, screen), Point::from((100, 120)));
        // No room below: above the cursor.
        assert_eq!(place(cursor(100, 750), size, screen), Point::from((100, 650)));
        // Kept on screen at the right edge.
        assert_eq!(place(cursor(900, 100), size, screen), Point::from((700, 120)));
    }
}
