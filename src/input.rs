//! Input routing: keyboard to the focused app, pointer to the surface under it.

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent, KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
    },
    desktop::WindowSurfaceType,
    input::{
        keyboard::FilterResult,
        pointer::{AxisFrame, ButtonEvent, GrabStartData, MotionEvent},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::{
    shortcuts::{self, Action},
    state::State,
};

/// Linux input code of the left mouse button.
const BTN_LEFT: u32 = 0x110;

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
                let under = self.surface_under(location);
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
                    let window = self.space.element_under(location).map(|(w, _)| w.clone());
                    self.focus_window(window.clone(), serial);
                    // Super + drag anywhere in a window moves it (the click never reaches the app).
                    if let Some(window) = window.filter(|_| event.button_code() == BTN_LEFT && self.move_key_held()) {
                        self.start_move(window, GrabStartData { focus: None, button: BTN_LEFT, location }, serial);
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

    pub fn surface_under(&self, location: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.space.element_under(location).and_then(|(window, loc)| {
            window.surface_under(location - loc.to_f64(), WindowSurfaceType::ALL).map(|(surface, offset)| (surface, (offset + loc).to_f64()))
        })
    }
}
