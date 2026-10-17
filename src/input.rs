//! Input routing: keyboard to the focused app, pointer to the surface under it.

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent, KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
    },
    desktop::{Window, WindowSurfaceType},
    input::{
        keyboard::{FilterResult, XkbConfig},
        pointer::{AxisFrame, ButtonEvent, CursorIcon, CursorImageStatus, GrabStartData, MotionEvent},
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
}

/// ↔ ↕ ⤡ ⤢ for a zone.
fn resize_icon(edges: Edges) -> CursorIcon {
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
                let action = keyboard.input::<Option<Action>, _>(self, event.key_code(), key_state, serial, event.time_msec(), |state, mods, key| {
                    let code = key.raw_code();
                    if key_state == KeyState::Released {
                        // The release of a shortcut key never reaches the app, which never saw the press.
                        return match state.suppressed.iter().position(|c| *c == code) {
                            Some(i) => {
                                state.suppressed.swap_remove(i);
                                FilterResult::Intercept(None)
                            }
                            None => FilterResult::Forward,
                        };
                    }
                    let Some(sym) = key.raw_latin_sym_or_raw_current_sym() else { return FilterResult::Forward };
                    match shortcuts::lookup(&state.bindings, mods, sym, state.nested).cloned() {
                        Some(action) => {
                            state.suppressed.push(code);
                            FilterResult::Intercept(Some(action))
                        }
                        None => FilterResult::Forward,
                    }
                });
                if let Some(action) = action.flatten() {
                    self.run_action(action);
                }
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let Some(output_geo) = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)) else { return };
                let Some(pointer) = self.seat.get_pointer() else { return };
                let location = event.position_transformed(output_geo.size) + output_geo.loc.to_f64();
                // A resize zone takes the pointer from whatever app is underneath it.
                let zone = if pointer.is_grabbed() { None } else { self.zone_under(location) };
                let under = if zone.is_some() { None } else { self.surface_under(location) };
                // Our cursor in a zone; entering an app resets to the default, then the app sets its own.
                let hover = zone.map(|(_, edges)| edges);
                if !pointer.is_grabbed() && (hover != self.zones.hover || pointer.current_focus() != under.as_ref().map(|(s, _)| s.clone())) {
                    self.zones.hover = hover;
                    self.set_cursor(hover.map_or_else(CursorImageStatus::default_named, |e| CursorImageStatus::Named(resize_icon(e))));
                }
                pointer.motion(self, under, &MotionEvent { location, serial: SERIAL_COUNTER.next_serial(), time: event.time_msec() });
                pointer.frame(self);
            }
            InputEvent::PointerButton { event } => {
                let Some(pointer) = self.seat.get_pointer() else { return };
                let serial = SERIAL_COUNTER.next_serial();
                let state = event.state();
                // Click to focus: the window under the pointer comes to the front.
                if state == ButtonState::Pressed && !pointer.is_grabbed() {
                    let location = pointer.current_location();
                    let zone = self.zone_under(location);
                    let window = zone.as_ref().map(|(w, _)| w.clone()).or_else(|| self.space.element_under(location).map(|(w, _)| w.clone()));
                    self.focus_window(window.clone(), serial);
                    let start_data = GrabStartData { focus: None, button: BTN_LEFT, location };
                    // Super + drag anywhere in a window moves it (the click never reaches the app).
                    if let Some(window) = window.filter(|_| event.button_code() == BTN_LEFT && self.move_key_held()) {
                        self.start_move(window, start_data, serial);
                    } else if let Some((window, edges)) = zone.filter(|_| event.button_code() == BTN_LEFT) {
                        self.zone_press(window, edges, start_data, serial);
                    }
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
    /// never reaches into a window above. Zoomed and fullscreen windows have none.
    pub fn zone_under(&self, location: Point<f64, Logical>) -> Option<(Window, Edges)> {
        for window in self.space.elements().rev() {
            let Some(geo) = self.space.element_geometry(window) else { continue };
            let local = location - (geo.loc - window.geometry().loc).to_f64();
            let popup = window.surface_under(local, WindowSurfaceType::POPUP).is_some();
            let filled = window.toplevel().is_some_and(|t| {
                t.with_pending_state(|s| s.states.contains(xdg_toplevel::State::Maximized) || s.states.contains(xdg_toplevel::State::Fullscreen))
            });
            if !popup
                && !filled
                && let Some(edges) = wm::zone_edges(geo, location)
            {
                return Some((window.clone(), edges));
            }
            if popup || window.surface_under(local, WindowSurfaceType::ALL).is_some() {
                return None;
            }
        }
        None
    }

    pub fn surface_under(&self, location: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.space.element_under(location).and_then(|(window, loc)| {
            window.surface_under(location - loc.to_f64(), WindowSurfaceType::ALL).map(|(surface, offset)| (surface, (offset + loc).to_f64()))
        })
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
