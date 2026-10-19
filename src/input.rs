//! Input routing: keyboard to the focused app, pointer to the surface under it.

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, GestureBeginEvent, GestureEndEvent, GesturePinchUpdateEvent as _,
        GestureSwipeUpdateEvent as _, InputBackend, InputEvent, KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
    },
    desktop::{Window, WindowSurfaceType},
    input::{
        keyboard::{FilterResult, Keycode, Keysym, ModifiersState, XkbConfig},
        pointer::{self as ptr, AxisFrame, ButtonEvent, CursorIcon, GrabStartData},
    },
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::{
    config::Keyboard,
    gestures::Kind,
    shortcuts::{self, Action},
    state::State,
    wm::{self, Edges},
};

/// Key repeat, ours and the one apps are told: held this long, then once a `REPEAT_RATE`.
pub const REPEAT_DELAY: std::time::Duration = std::time::Duration::from_millis(500);
pub const REPEAT_RATE: std::time::Duration = std::time::Duration::from_millis(33);

/// Whether a key repeats while held: not a modifier, nor one whose repeat would act again (Return, Escape, Tab).
pub fn repeats(sym: Keysym) -> bool {
    !sym.is_modifier_key() && !matches!(sym, Keysym::Return | Keysym::KP_Enter | Keysym::Escape | Keysym::Tab | Keysym::ISO_Left_Tab)
}

/// Linux input code of the left mouse button.
pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;

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
    /// A key one of our surfaces takes before any app: the lock screen (every key while locked), an alert, the
    /// screenshot overlay, the switcher, Force Quit, Mission Control, Spotlight, the emoji picker, an open popup's
    /// Esc, the app drawer, Launchpad, a desktop icon's name. Overlays that take every key still let desktop
    /// shortcuts through. True when the key was used.
    fn shell_key(&mut self, sym: Keysym, raw: Keysym, mods: &ModifiersState) -> bool {
        if self.locked() {
            return self.lock_key(sym, mods.ctrl);
        }
        if self.alert_key(sym, mods.ctrl) {
            return true;
        }
        let unbound = shortcuts::lookup(&self.bindings, mods, raw, self.nested).is_none();
        if (self.screenshots.shot.is_some() && unbound && self.shot_key(sym))
            || self.switcher_key(raw, mods)
            || self.force_quit_key(sym)
            || self.mission_key(sym)
            || self.spotlight_key(sym, raw, mods)
            || self.emoji_key(sym, raw, mods)
            || (sym == Keysym::Escape && self.close_popup())
        {
            return true;
        }
        if self.drawer_open() && unbound {
            self.drawer_key(sym, mods.ctrl);
            return true;
        }
        if self.launchpad_open() && unbound {
            self.launchpad_key(sym, mods.ctrl);
            return true;
        }
        self.desktop_key(sym, mods.ctrl)
    }

    /// Repeats `sym` into the shell surface that took it while `code` stays down (D14), at the rate apps get; it
    /// stops when the key goes up, another is pressed, or nothing of ours takes it any more.
    fn start_repeat(&mut self, code: Keycode, sym: Keysym, raw: Keysym, mods: ModifiersState) {
        if !repeats(sym) {
            return;
        }
        let timer = Timer::from_duration(REPEAT_DELAY);
        let token = self.loop_handle.insert_source(timer, move |_, _, state| {
            if state.key_repeat.as_ref().is_some_and(|(c, _)| *c == code) && state.shell_key(sym, raw, &mods) {
                state.schedule_redraw();
                return TimeoutAction::ToDuration(REPEAT_RATE);
            }
            state.key_repeat = None;
            TimeoutAction::Drop
        });
        match token {
            Ok(token) => self.key_repeat = Some((code, token)),
            Err(err) => tracing::warn!("key repeat: {err}"),
        }
    }

    pub fn stop_repeat(&mut self) {
        if let Some((_, token)) = self.key_repeat.take() {
            self.loop_handle.remove(token);
        }
    }

    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        self.idle_activity();
        // Clicks, scrolls and gestures with Super held aren't a Super tap; moving the pointer is fine.
        if !matches!(event, InputEvent::Keyboard { .. } | InputEvent::PointerMotion { .. } | InputEvent::PointerMotionAbsolute { .. }) {
            self.super_tap.cancel();
        }
        match event {
            InputEvent::Keyboard { event } => {
                let Some(keyboard) = self.seat.get_keyboard() else { return };
                let serial = SERIAL_COUNTER.next_serial();
                let key_state = event.state();
                let inhibited = self.shortcuts_inhibited();
                let action = keyboard.input::<Option<Action>, _>(self, event.key_code(), key_state, serial, event.time_msec(), |state, mods, key| {
                    let code = key.raw_code();
                    let logo = matches!(key.modified_sym(), Keysym::Super_L | Keysym::Super_R);
                    state.super_tap.key(logo, key_state == KeyState::Pressed, !mods.ctrl && !mods.alt && !mods.shift, std::time::Instant::now());
                    if key_state == KeyState::Released {
                        if state.key_repeat.as_ref().is_some_and(|(c, _)| *c == code) {
                            state.stop_repeat();
                        }
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
                    // A new press ends the repeat of the last one, as on the app side.
                    state.stop_repeat();
                    // Ctrl+Alt+F1…F12 on the real screen switch virtual terminals, whatever is open.
                    if state.switch_vt(key.modified_sym()) {
                        state.suppressed.push(code);
                        return FilterResult::Intercept(None);
                    }
                    let raw = key.raw_latin_sym_or_raw_current_sym().unwrap_or(key.modified_sym());
                    if state.shell_key(key.modified_sym(), raw, mods) {
                        state.suppressed.push(code);
                        state.start_repeat(code, key.modified_sym(), raw, *mods);
                        return FilterResult::Intercept(None);
                    }
                    // Locked: the lock screen took every key, or its client's lock surface gets them.
                    if state.locked() {
                        return FilterResult::Forward;
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
                // A lone Super tap (its release goes on to the app, which saw the press).
                if std::mem::take(&mut self.super_tap.fired) && self.config.app_drawer.super_tap && !inhibited && !self.locked() && self.alert.is_none() {
                    self.run_action(Action::AppDrawer);
                }
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
                if self.locked() {
                    if state == ButtonState::Pressed {
                        self.lock_press(self.to_output(location));
                    }
                    // Only a client's lock surface (under the pointer, see surface_under) gets the button.
                    pointer.button(self, &ButtonEvent { button: event.button_code(), state, serial, time: event.time_msec() });
                    pointer.frame(self);
                    return;
                }
                if state == ButtonState::Pressed {
                    self.desktop_blur(self.to_output(location));
                }
                // The menu bar and its popups take their clicks; elsewhere, click to focus: the window under the
                // pointer comes to the front.
                if state == ButtonState::Pressed
                    && !pointer.is_grabbed()
                    && !self.shot_press(self.to_output(location), event.button_code() == BTN_LEFT)
                    && !self.drawer_press(self.to_output(location), event.button_code() == BTN_RIGHT)
                    && !self.dock_press(self.to_output(location), event.button_code())
                    && !self.mission_press(self.to_output(location), event.button_code())
                    && !self.shell_press(self.to_output(location), event.button_code() == BTN_RIGHT)
                {
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
                        // The bare desktop: its icons.
                        if window.is_none() {
                            self.desktop_press(self.to_output(location), event.button_code());
                        }
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
                    self.shot_release();
                    self.bar_release(pointer.current_location());
                    self.mission_release(self.to_output(pointer.current_location()));
                    self.shell_release();
                    self.desktop_release();
                    self.dock_release(self.to_output(pointer.current_location()));
                }
                pointer.button(self, &ButtonEvent { button: event.button_code(), state, serial, time: event.time_msec() });
                pointer.frame(self);
            }
            InputEvent::PointerAxis { event } => {
                let Some(pointer) = self.seat.get_pointer() else { return };
                let source = event.source();
                if self.lock.as_ref().is_some_and(|l| l.client_surface().is_none()) {
                    return;
                }
                // A client's lock surface (E6.4) gets the scroll as it is: none of the shell's handlers below.
                let shell = !self.locked();
                // Notification Center scrolls; a banner swipes away.
                let amount = |axis| event.amount(axis).unwrap_or_else(|| event.amount_v120(axis).unwrap_or(0.0) * 15.0 / 120.0);
                let pos = self.to_output(pointer.current_location());
                // Over the dock, a wheel step cycles the app's windows; touchpad scrolling there is just swallowed.
                let wheel = event.amount_v120(Axis::Vertical).unwrap_or(0.0);
                // Launchpad pages: a wheel step either way, or a finger swipe sideways.
                let page_wheel = if wheel != 0.0 { wheel } else { event.amount_v120(Axis::Horizontal).unwrap_or(0.0) };
                let lifted = source == AxisSource::Finger && event.amount(Axis::Horizontal) == Some(0.0);
                let swipe = if page_wheel == 0.0 { amount(Axis::Horizontal) } else { 0.0 };
                if shell && self.launchpad_scroll(swipe, amount(Axis::Vertical), page_wheel, lifted) {
                    return;
                }
                if shell
                    && (self.emoji_scroll(pos, amount(Axis::Vertical))
                        || self.drawer_scroll(pos, amount(Axis::Vertical), wheel)
                        || self.control_scroll(pos, amount(Axis::Vertical))
                        || self.menu_scroll(pos, amount(Axis::Vertical)))
                {
                    return;
                }
                if shell && (self.notify_scroll(pos, amount(Axis::Horizontal), amount(Axis::Vertical)) || self.dock_scroll(pos, page_wheel)) {
                    return;
                }
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
            // System gestures (E5.3) are ours; the rest (2-finger pinch, holds) go to the app (pointer-gestures).
            InputEvent::GestureSwipeBegin { event } => {
                if !self.gesture_begin(Kind::Swipe, event.fingers(), event.time_msec())
                    && let Some(pointer) = self.seat.get_pointer()
                {
                    let serial = SERIAL_COUNTER.next_serial();
                    pointer.gesture_swipe_begin(self, &ptr::GestureSwipeBeginEvent { serial, time: event.time_msec(), fingers: event.fingers() });
                }
            }
            InputEvent::GestureSwipeUpdate { event } => {
                if !self.gesture_update(event.time_msec(), event.delta(), 1.0)
                    && let Some(pointer) = self.seat.get_pointer()
                {
                    pointer.gesture_swipe_update(self, &ptr::GestureSwipeUpdateEvent { time: event.time_msec(), delta: event.delta() });
                }
            }
            InputEvent::GestureSwipeEnd { event } => {
                if !self.gesture_end(event.time_msec(), event.cancelled())
                    && let Some(pointer) = self.seat.get_pointer()
                {
                    let serial = SERIAL_COUNTER.next_serial();
                    pointer.gesture_swipe_end(self, &ptr::GestureSwipeEndEvent { serial, time: event.time_msec(), cancelled: event.cancelled() });
                }
            }
            InputEvent::GesturePinchBegin { event } => {
                if !self.gesture_begin(Kind::Pinch, event.fingers(), event.time_msec())
                    && let Some(pointer) = self.seat.get_pointer()
                {
                    let serial = SERIAL_COUNTER.next_serial();
                    pointer.gesture_pinch_begin(self, &ptr::GesturePinchBeginEvent { serial, time: event.time_msec(), fingers: event.fingers() });
                }
            }
            InputEvent::GesturePinchUpdate { event } => {
                if !self.gesture_update(event.time_msec(), event.delta(), event.scale())
                    && let Some(pointer) = self.seat.get_pointer()
                {
                    let update =
                        ptr::GesturePinchUpdateEvent { time: event.time_msec(), delta: event.delta(), scale: event.scale(), rotation: event.rotation() };
                    pointer.gesture_pinch_update(self, &update);
                }
            }
            InputEvent::GesturePinchEnd { event } => {
                if !self.gesture_end(event.time_msec(), event.cancelled())
                    && let Some(pointer) = self.seat.get_pointer()
                {
                    let serial = SERIAL_COUNTER.next_serial();
                    pointer.gesture_pinch_end(self, &ptr::GesturePinchEndEvent { serial, time: event.time_msec(), cancelled: event.cancelled() });
                }
            }
            InputEvent::GestureHoldBegin { event } => {
                if let Some(pointer) = self.seat.get_pointer() {
                    let serial = SERIAL_COUNTER.next_serial();
                    pointer.gesture_hold_begin(self, &ptr::GestureHoldBeginEvent { serial, time: event.time_msec(), fingers: event.fingers() });
                }
            }
            InputEvent::GestureHoldEnd { event } => {
                if let Some(pointer) = self.seat.get_pointer() {
                    let serial = SERIAL_COUNTER.next_serial();
                    pointer.gesture_hold_end(self, &ptr::GestureHoldEndEvent { serial, time: event.time_msec(), cancelled: event.cancelled() });
                }
            }
            _ => {}
        }
    }

    /// The resize zone at `location`, if any: the topmost window whose zone or content is there decides, so a zone
    /// never reaches into a window above (or under a bar). Zoomed and fullscreen windows have none.
    pub fn zone_under(&self, location: Point<f64, Logical>) -> Option<(Window, Edges)> {
        if self.layer_under(location, true).is_some() || self.mission_open() {
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
        // Locked: only a client's lock surface, covering the output.
        if let Some(lock) = &self.lock {
            let origin = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map(|g| g.loc.to_f64()).unwrap_or_default();
            return lock.client_surface().map(|s| (s.wl_surface().clone(), origin));
        }
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

    #[test]
    fn letters_arrows_and_backspace_repeat_modifiers_and_return_dont() {
        assert!([Keysym::a, Keysym::Down, Keysym::BackSpace, Keysym::space, Keysym::Delete].into_iter().all(repeats));
        assert!(![Keysym::Shift_L, Keysym::Control_R, Keysym::Super_L, Keysym::Return, Keysym::KP_Enter, Keysym::Escape, Keysym::Tab].into_iter().any(repeats));
        // Delay then rate, as told to apps: 500 ms, 30/s.
        assert_eq!((REPEAT_DELAY.as_millis(), 1000 / REPEAT_RATE.as_millis()), (500, 30));
    }
}
