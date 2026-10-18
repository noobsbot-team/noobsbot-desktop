//! Input routing: keyboard to the focused app, pointer to the surface under it.

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent, KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
    },
    desktop::{Window, WindowSurfaceType},
    input::{
        keyboard::{FilterResult, Keysym, XkbConfig},
        pointer::{AxisFrame, ButtonEvent, CursorIcon, GrabStartData},
    },
    reexports::{wayland_protocols::xdg::shell::server::xdg_toplevel, wayland_server::protocol::wl_surface::WlSurface},
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::{
    config::Keyboard,
    shortcuts::{self, Action},
    state::State,
    wm::{self, Edges},
};

/// Linux input code of the left mouse button.
const BTN_LEFT: u32 = 0x110;

/// The xkb names for a resolved `[keyboard]` section (see `Keyboard::resolved`).
pub fn xkb(keyboard: &Keyboard) -> XkbConfig<'_> {
    let options = (!keyboard.options.is_empty()).then(|| keyboard.options.clone());
    XkbConfig { layout: &keyboard.layout, variant: &keyboard.variant, options, ..Default::default() }
}

/// Pointer state of our resize zones.
#[derive(Default)]
pub struct Zones {
    /// The zone the pointer is in, whose cursor is showing.
    pub hover: Option<Edges>,
    /// The last press in a zone, to spot a double-click.
    pub last_press: Option<(std::time::Instant, Window, Edges)>,
    /// A traffic light pressed and not yet released.
    pub bar_press: Option<(Window, crate::decorations::Part)>,
    /// The last press on a title, to spot a double-click.
    pub last_title_press: Option<(std::time::Instant, Window)>,
    /// The window whose traffic lights the pointer is over (they show their glyphs).
    pub lights_hover: Option<Window>,
}

/// ↔ ↕ ⤡ ⤢ for a zone.
pub fn resize_icon(edges: Edges) -> CursorIcon {
    match (edges.left || edges.right, edges.top || edges.bottom) {
        (true, false) => CursorIcon::EwResize,
        (false, _) => CursorIcon::NsResize,
        _ if edges.left == edges.top => CursorIcon::NwseResize,
        _ => CursorIcon::NeswResize,
    }
}

impl State {
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event } => {
                let Some(keyboard) = self.seat.get_keyboard() else { return };
                let serial = SERIAL_COUNTER.next_serial();
                let key_state = event.state();
                let inhibited = self.shortcuts_inhibited();
                let action = keyboard.input::<Option<Action>, _>(self, event.key_code(), key_state, serial, event.time_msec(), |state, mods, key| {
                    let code = key.raw_code();
                    if key_state == KeyState::Released {
                        state.switcher_release(mods);
                        // The release of a shortcut key never reaches the app, which never saw the press.
                        return match state.suppressed.iter().position(|c| *c == code) {
                            Some(i) => {
                                state.suppressed.swap_remove(i);
                                FilterResult::Intercept(None)
                            }
                            None => FilterResult::Forward,
                        };
                    }
                    // An alert answers Esc and Return first; then Esc closes an open menu-bar popup.
                    let answer = match key.modified_sym() {
                        _ if state.alert.is_none() => None,
                        Keysym::Escape => Some(crate::shell::alert::Button::Cancel),
                        Keysym::Return | Keysym::KP_Enter => Some(crate::shell::alert::Button::Confirm),
                        _ => None,
                    };
                    if let Some(answer) = answer {
                        state.alert_answer(answer);
                        state.suppressed.push(code);
                        return FilterResult::Intercept(None);
                    }
                    // The app switcher takes every key while it's up; the Force Quit window its few.
                    let raw = key.raw_latin_sym_or_raw_current_sym().unwrap_or(key.modified_sym());
                    if state.switcher_key(raw, mods) || state.force_quit_key(key.modified_sym()) {
                        state.suppressed.push(code);
                        return FilterResult::Intercept(None);
                    }
                    if key.modified_sym() == Keysym::Escape && state.close_popup() {
                        state.suppressed.push(code);
                        return FilterResult::Intercept(None);
                    }
                    // An app that inhibits shortcuts (a VM, remote desktop) gets every key.
                    let Some(sym) = key.raw_latin_sym_or_raw_current_sym().filter(|_| !inhibited) else { return FilterResult::Forward };
                    match shortcuts::lookup(&state.bindings, mods, sym, state.nested).cloned() {
                        Some(action) => {
                            state.suppressed.push(code);
                            FilterResult::Intercept(Some(action))
                        }
                        None => FilterResult::Forward,
                    }
                });
                self.switcher_settle();
                match action {
                    Some(Some(action)) => self.run_action(action),
                    // A key the app gets: is it still answering (E4.7)?
                    None if key_state == KeyState::Pressed => {
                        if let Some(window) = self.focused_window() {
                            self.ping(&window);
                        }
                    }
                    _ => {}
                }
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let Some(output_geo) = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)) else { return };
                self.pointer_motion(event.position_transformed(output_geo.size) + output_geo.loc.to_f64(), event.time());
            }
            InputEvent::PointerButton { event } => {
                let Some(pointer) = self.seat.get_pointer() else { return };
                let serial = SERIAL_COUNTER.next_serial();
                let state = event.state();
                let location = pointer.current_location();
                // The menu bar and its popups take their clicks; elsewhere, click to focus: the window under the
                // pointer comes to the front.
                if state == ButtonState::Pressed && !pointer.is_grabbed() && !self.shell_press(self.to_output(location)) {
                    let zone = self.zone_under(location);
                    let bar = if zone.is_some() { None } else { self.bar_under(location) };
                    let window = zone
                        .as_ref()
                        .map(|(w, _)| w.clone())
                        .or_else(|| bar.as_ref().map(|(w, _)| w.clone()))
                        .or_else(|| self.space.element_under(location).map(|(w, _)| w.clone()));
                    // Bars and launchers above windows (or the wallpaper below, where no window is) take the click instead.
                    let layer = self.layer_under(location, true).or_else(|| window.is_none().then(|| self.layer_under(location, false)).flatten());
                    if let Some((layer, ..)) = layer {
                        if layer.can_receive_keyboard_focus()
                            && let Some(keyboard) = self.seat.get_keyboard()
                        {
                            keyboard.set_focus(self, Some(layer.wl_surface().clone()), serial);
                        }
                    } else {
                        self.focus_window(window.clone(), serial);
                        if let Some(window) = &window {
                            self.ping(window);
                        }
                        let start_data = GrabStartData { focus: None, button: BTN_LEFT, location };
                        // Super + drag anywhere in a window moves it (the click never reaches the app).
                        if let Some(window) = window.filter(|_| event.button_code() == BTN_LEFT && self.move_key_held()) {
                            self.start_move(window, start_data, serial);
                        } else if let Some((window, edges)) = zone.filter(|_| event.button_code() == BTN_LEFT) {
                            self.zone_press(window, edges, start_data, serial);
                        } else if let Some((window, part)) = bar.filter(|_| event.button_code() == BTN_LEFT) {
                            self.bar_press(window, part, start_data, serial);
                        }
                    }
                }
                if state == ButtonState::Released && event.button_code() == BTN_LEFT {
                    self.bar_release(pointer.current_location());
                    self.shell_release();
                }
                pointer.button(self, &ButtonEvent { button: event.button_code(), state, serial, time: event.time_msec() });
                pointer.frame(self);
            }
            InputEvent::PointerAxis { event } => {
                let Some(pointer) = self.seat.get_pointer() else { return };
                let source = event.source();
                let mut frame = AxisFrame::new(event.time_msec()).source(source);
                for axis in [Axis::Horizontal, Axis::Vertical] {
                    let v120 = event.amount_v120(axis);
                    let amount = event.amount(axis).unwrap_or_else(|| v120.unwrap_or(0.0) * 15.0 / 120.0);
                    if amount != 0.0 {
                        frame = frame.value(axis, amount);
                        if let Some(v120) = v120 {
                            frame = frame.v120(axis, v120 as i32);
                        }
                    } else if source == AxisSource::Finger && event.amount(axis) == Some(0.0) {
                        frame = frame.stop(axis);
                    }
                }
                pointer.axis(self, frame);
                pointer.frame(self);
            }
            _ => {}
        }
    }

    /// The resize zone at `location`, if any: the topmost window whose zone or content is there decides, so a zone
    /// never reaches into a window above (or under a bar). Zoomed and fullscreen windows have none.
    pub fn zone_under(&self, location: Point<f64, Logical>) -> Option<(Window, Edges)> {
        if self.layer_under(location, true).is_some() {
            return None;
        }
        for window in self.space.elements().rev() {
            let (Some(geo), Some(frame)) = (self.space.element_geometry(window), self.frame(window)) else { continue };
            let local = location - (geo.loc - window.geometry().loc).to_f64();
            let popup = window.surface_under(local, WindowSurfaceType::POPUP).is_some();
            let filled = window.toplevel().is_some_and(|t| {
                t.with_pending_state(|s| s.states.contains(xdg_toplevel::State::Maximized) || s.states.contains(xdg_toplevel::State::Fullscreen))
            });
            if !popup
                && !filled
                && let Some(edges) = wm::zone_edges(frame, location)
            {
                return Some((window.clone(), edges));
            }
            if popup || frame.to_f64().contains(location) || window.surface_under(local, WindowSurfaceType::ALL).is_some() {
                return None;
            }
        }
        None
    }

    /// Top/overlay layer surfaces, then windows, then bottom/background layer surfaces; nothing over the menu bar or
    /// an open popup, which the shell draws itself.
    pub fn surface_under(&self, location: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        if self.shell_covers(self.to_output(location)) {
            return None;
        }
        let window = || {
            self.space.element_under(location).and_then(|(window, loc)| {
                window.surface_under(location - loc.to_f64(), WindowSurfaceType::ALL).map(|(surface, offset)| (surface, (offset + loc).to_f64()))
            })
        };
        self.layer_under(location, true).map(|(_, s, loc)| (s, loc)).or_else(window).or_else(|| self.layer_under(location, false).map(|(_, s, loc)| (s, loc)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_cursors() {
        let e = |left, right, top, bottom| resize_icon(Edges { left, right, top, bottom });
        assert_eq!(e(true, false, false, false), CursorIcon::EwResize);
        assert_eq!(e(false, false, false, true), CursorIcon::NsResize);
        assert_eq!(e(true, false, true, false), CursorIcon::NwseResize);
        assert_eq!(e(false, true, false, true), CursorIcon::NwseResize);
        assert_eq!(e(false, true, true, false), CursorIcon::NeswResize);
    }
}
