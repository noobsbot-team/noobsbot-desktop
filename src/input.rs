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
    utils::{IsAlive, Logical, Point, SERIAL_COUNTER},
};

use std::time::{Duration, Instant};

use crate::{
    config::Keyboard,
    gestures::Kind,
    shell::Osd,
    shortcuts::{self, Action},
    state::State,
    symbols::Symbol,
    wm::{self, Edges},
};

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

/// Evdev codes of Ctrl, Shift, Alt and Super, left and right.
const MODIFIERS: [u32; 8] = [29, 97, 42, 54, 56, 100, 125, 126];
const SHIFTS: [u32; 2] = [42, 54];

/// Whether `code` is a modifier key, by its place on the keyboard. ponytail: a modifier moved by an xkb option (Caps
/// as Ctrl) doesn't stick; ask the keymap if that's ever wanted.
fn is_modifier(code: Keycode) -> bool {
    MODIFIERS.contains(&code.raw().wrapping_sub(8))
}

/// Sticky, slow and bounce keys (E8.6): what happens to a key before the keymap sees it.
#[derive(Default)]
pub struct AccessKeys {
    /// Sticky: modifiers tapped once (held for the next key) and twice (held until tapped again). Their release was
    /// kept from the keymap, so they are still down there.
    latched: Vec<Keycode>,
    locked: Vec<Keycode>,
    /// The modifier pressed with no other key since, so its release is a tap.
    tap: Option<Keycode>,
    /// Shift taps in a row and the last one, for the five-press toggle.
    shifts: (u8, Option<Instant>),
    /// Slow: keys pressed and when, not yet held long enough.
    slow: Vec<(Keycode, Instant)>,
    /// Bounce: the last key let go and when.
    released: Option<(Keycode, Instant)>,
    /// Presses bounce keys dropped; their release is dropped too.
    bounced: Vec<Keycode>,
    /// Mouse keys: the keypad keys held (evdev codes), since when, and whether the timer moving the pointer runs.
    mouse_held: Vec<u32>,
    mouse_since: Option<Instant>,
}

impl AccessKeys {
    /// Sticky keys: `None` drops the event (a latched modifier stays down); `Some(codes)` passes it on, then the
    /// modifiers in `codes` are let go.
    pub fn sticky(&mut self, code: Keycode, pressed: bool, on: bool) -> Option<Vec<Keycode>> {
        if !on {
            return Some(Vec::new());
        }
        let held = self.latched.contains(&code) || self.locked.contains(&code);
        if !is_modifier(code) {
            if pressed {
                self.tap = None;
                return Some(std::mem::take(&mut self.latched));
            }
            return Some(Vec::new());
        }
        if pressed {
            self.tap = Some(code);
            return (!held).then(Vec::new);
        }
        if self.tap.take() != Some(code) {
            // Held with another key: an ordinary chord, unless it was already stuck.
            return (!held).then(Vec::new);
        }
        if let Some(i) = self.locked.iter().position(|c| *c == code) {
            self.locked.remove(i);
            return Some(Vec::new());
        }
        match self.latched.iter().position(|c| *c == code) {
            Some(i) => self.locked.push(self.latched.remove(i)),
            None => self.latched.push(code),
        }
        None
    }

    /// Every stuck modifier, now let go (sticky keys turned off).
    pub fn unstick(&mut self) -> Vec<Keycode> {
        self.tap = None;
        let mut all = std::mem::take(&mut self.latched);
        all.append(&mut self.locked);
        all
    }

    /// The stuck modifiers, for the OSD: latched, then locked.
    pub fn stuck(&self) -> (Vec<Keycode>, Vec<Keycode>) {
        (self.latched.clone(), self.locked.clone())
    }

    /// True on the fifth Shift tap in a row, each within a second of the last.
    pub fn five_shifts(&mut self, code: Keycode, pressed: bool, now: Instant) -> bool {
        let shift = SHIFTS.contains(&code.raw().wrapping_sub(8));
        if pressed && !shift {
            self.shifts = (0, None);
        }
        if pressed || !shift {
            return false;
        }
        let quick = self.shifts.1.is_some_and(|t| now.saturating_duration_since(t) < Duration::from_secs(1));
        self.shifts = (if quick { self.shifts.0 + 1 } else { 1 }, Some(now));
        if self.shifts.0 < 5 {
            return false;
        }
        self.shifts = (0, None);
        true
    }

    /// Bounce keys: false drops the event (a press too soon after the same key went up, and its release).
    pub fn bounce(&mut self, code: Keycode, pressed: bool, now: Instant, window: Duration) -> bool {
        if pressed {
            let bounced = self.released.is_some_and(|(c, t)| c == code && now.saturating_duration_since(t) < window);
            if bounced {
                self.bounced.push(code);
            }
            return !bounced;
        }
        if let Some(i) = self.bounced.iter().position(|c| *c == code) {
            self.bounced.swap_remove(i);
            return false;
        }
        self.released = Some((code, now));
        true
    }

    /// Slow keys: a press waits for `slow_fire`; false drops the release of a key let go before it counted.
    pub fn slow_press(&mut self, code: Keycode, now: Instant) {
        self.slow.retain(|(c, _)| *c != code);
        self.slow.push((code, now));
    }

    pub fn slow_release(&mut self, code: Keycode) -> bool {
        let before = self.slow.len();
        self.slow.retain(|(c, _)| *c != code);
        self.slow.len() == before
    }

    /// The press made at `at` is still held: it counts now.
    pub fn slow_fire(&mut self, code: Keycode, at: Instant) -> bool {
        let before = self.slow.len();
        self.slow.retain(|e| *e != (code, at));
        self.slow.len() != before
    }
}

/// How often mouse keys move the pointer while a key is held.
const MOUSE_TICK: Duration = Duration::from_millis(16);

/// Keypad 7 8 9 / 4 6 / 1 2 3 (evdev codes) as a step on each axis.
fn mouse_direction(evdev: u32) -> Option<(i8, i8)> {
    Some(match evdev {
        71 => (-1, -1),
        72 => (0, -1),
        73 => (1, -1),
        75 => (-1, 0),
        77 => (1, 0),
        79 => (-1, 1),
        80 => (0, 1),
        81 => (1, 1),
        _ => return None,
    })
}

/// One tick's move for the keypad keys `held`: 1 px at first for fine placing, speeding up to 20 px over a second.
fn mouse_step(held: &[u32], held_for: Duration) -> Point<f64, Logical> {
    let (x, y) = held.iter().filter_map(|c| mouse_direction(*c)).fold((0i8, 0i8), |(x, y), (dx, dy)| (x + dx, y + dy));
    let speed = 1.0 + 19.0 * held_for.as_secs_f64().min(1.0);
    (f64::from(x.signum()) * speed, f64::from(y.signum()) * speed).into()
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
    /// `focus = "hover"`: the window the pointer rests on, focused when its timer fires.
    pub hover_focus: Option<(Window, smithay::reexports::calloop::RegistrationToken)>,
    /// The window the window menu acts on, and where it opened (output-relative).
    pub window_menu: Option<(Window, Point<i32, Logical>)>,
}

/// How long the pointer rests on a window before hover focus gives it the keyboard.
const HOVER_FOCUS: std::time::Duration = std::time::Duration::from_millis(200);

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
            || self.mission_key(sym, mods)
            || self.spotlight_key(sym, raw, mods)
            || self.emoji_key(sym, raw, mods)
            || self.menu_key(sym)
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
        // The rate and delay apps are told (`[keyboard] repeat_*`).
        let (rate, delay) = self.config.keyboard.repeat();
        if !repeats(sym) || rate == 0 {
            return;
        }
        let every = std::time::Duration::from_millis(1000 / rate as u64);
        let timer = Timer::from_duration(std::time::Duration::from_millis(delay as u64));
        let token = self.loop_handle.insert_source(timer, move |_, _, state| {
            if state.key_repeat.as_ref().is_some_and(|(c, _)| *c == code) && state.shell_key(sym, raw, &mods) {
                state.schedule_redraw();
                return TimeoutAction::ToDuration(every);
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

    /// A key from the keyboard, through bounce, slow, mouse and sticky keys (E8.6), to `key`.
    fn key_event(&mut self, code: Keycode, key_state: KeyState, time: u32) {
        let (pressed, now) = (key_state == KeyState::Pressed, Instant::now());
        let a11y = &self.config.accessibility;
        let (bounce, slow) = (Duration::from_millis(a11y.bounce_keys_ms.into()), Duration::from_millis(a11y.slow_keys_ms.into()));
        if !self.access.bounce(code, pressed, now, bounce) {
            return;
        }
        if !slow.is_zero() && pressed {
            self.access.slow_press(code, now);
            let timer = self.loop_handle.insert_source(Timer::from_duration(slow), move |_, _, state| {
                if state.access.slow_fire(code, now) {
                    state.key_counted(code, KeyState::Pressed, time.wrapping_add(slow.as_millis() as u32));
                }
                TimeoutAction::Drop
            });
            if let Err(err) = timer {
                tracing::warn!("slow keys: {err}");
            }
            return;
        }
        if pressed || self.access.slow_release(code) {
            self.key_counted(code, key_state, time);
        }
    }

    /// A key that counts: mouse keys take the keypad, sticky keys hold modifiers, the rest goes to `key`.
    fn key_counted(&mut self, code: Keycode, key_state: KeyState, time: u32) {
        let pressed = key_state == KeyState::Pressed;
        if self.config.accessibility.mouse_keys && self.mouse_key(code, pressed, time) {
            return;
        }
        if self.config.accessibility.sticky_keys_shortcut && self.access.five_shifts(code, pressed, Instant::now()) {
            let on = !self.config.accessibility.sticky_keys;
            self.config.accessibility.sticky_keys = on;
            crate::config::save_key("accessibility", "sticky_keys", if on { "true" } else { "false" });
            self.show_osd(Osd { symbol: Symbol::Keyboard, level: None, text: Some(format!("Sticky Keys {}", if on { "On" } else { "Off" })) });
            if !on {
                for c in self.access.unstick() {
                    self.key(c, KeyState::Released, time);
                }
            }
        }
        let before = self.access.stuck();
        let Some(release) = self.access.sticky(code, pressed, self.config.accessibility.sticky_keys) else {
            self.sticky_osd(before);
            return;
        };
        self.key(code, key_state, time);
        for c in release {
            self.key(c, KeyState::Released, time);
        }
        self.sticky_osd(before);
    }

    /// The OSD names the stuck modifiers when they change.
    fn sticky_osd(&mut self, before: (Vec<Keycode>, Vec<Keycode>)) {
        let (latched, locked) = self.access.stuck();
        if (latched.clone(), locked.clone()) == before {
            return;
        }
        let name = |c: &Keycode| match c.raw().wrapping_sub(8) {
            29 | 97 => "Control",
            42 | 54 => "Shift",
            56 | 100 => "Option",
            _ => "Super",
        };
        let mut words: Vec<String> = latched.iter().map(|c| name(c).to_owned()).collect();
        words.extend(locked.iter().map(|c| format!("{} (locked)", name(c))));
        let text = if words.is_empty() { "Modifiers released".to_owned() } else { words.join(" + ") };
        self.show_osd(Osd { symbol: Symbol::Keyboard, level: None, text: Some(text) });
    }

    /// A key to the shell, its shortcuts and the focused app; the on-screen keyboard types through it too.
    pub fn key(&mut self, code: Keycode, key_state: KeyState, time: u32) {
        let Some(keyboard) = self.seat.get_keyboard() else { return };
        let serial = SERIAL_COUNTER.next_serial();
        // Settings recording a shortcut gets every key, as an app that inhibits shortcuts does.
        let inhibited = self.shortcuts_inhibited() || self.recording_on();
        let super_down = self.super_tap.down();
        let before = keyboard.modifier_state();
        let mut reader = false;
        let action = keyboard.input::<Option<Action>, _>(self, code, key_state, serial, time, |state, mods, key| {
            let code = key.raw_code();
            // The screen reader sees keys first; its grabs reach nothing else (E8.6).
            let sym = key.modified_sym();
            let s = &mods.serialized;
            let a11y = crate::services::a11y::Key {
                released: key_state == KeyState::Released,
                state: s.depressed | s.latched | s.locked,
                keysym: sym.raw(),
                unichar: sym.key_char().map_or(0, u32::from),
                keycode: code.raw() as u16,
            };
            if state.services.keys.key(a11y) {
                reader = true;
                return FilterResult::Intercept(None);
            }
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
        // A grabbed Caps Lock (the reader's modifier) doesn't toggle Caps Lock.
        if reader {
            let now = keyboard.modifier_state();
            if (now.caps_lock, now.num_lock) != (before.caps_lock, before.num_lock) {
                keyboard.set_modifier_state(ModifiersState { caps_lock: before.caps_lock, num_lock: before.num_lock, ..now });
            }
        }
        self.switcher_settle();
        // Super pressed or let go alone: the dock's number pills come or go (E8.3).
        if self.super_tap.down() != super_down {
            self.schedule_redraw();
        }
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

    /// A pointer button, from the device (after the accessibility helpers in pointer_assist.rs) or mouse keys.
    pub fn pointer_button(&mut self, button: u32, state: ButtonState, time: u32) {
        let Some(pointer) = self.seat.get_pointer() else { return };
        let serial = SERIAL_COUNTER.next_serial();
        let location = pointer.current_location();
        // The on-screen keyboard is above everything, the lock screen too; its clicks reach no app.
        if state == ButtonState::Pressed && self.osk_press(self.to_output(location)) {
            return;
        }
        if self.locked() {
            if state == ButtonState::Pressed {
                self.lock_press(self.to_output(location));
            }
            // Only a client's lock surface (under the pointer, see surface_under) gets the button.
            pointer.button(self, &ButtonEvent { button, state, serial, time });
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
            && !self.switcher_press(location)
            && !self.shot_press(self.to_output(location), button == BTN_LEFT)
            && !self.drawer_press(self.to_output(location), button == BTN_RIGHT)
            && !self.dock_press(self.to_output(location), button)
            && !self.mission_press(self.to_output(location), button)
            && !self.shell_press(self.to_output(location), button == BTN_RIGHT)
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
                    self.desktop_press(self.to_output(location), button);
                }
                if let Some(window) = &window {
                    self.ping(window);
                }
                let start_data = GrabStartData { focus: None, button: BTN_LEFT, location };
                // Super + drag anywhere in a window moves it (the click never reaches the app).
                if let Some(window) = window.clone().filter(|_| button == BTN_LEFT && self.move_key_held()) {
                    self.start_move(window, start_data, serial);
                } else if let Some((window, frame)) =
                    window.filter(|_| button == BTN_RIGHT && self.move_key_held()).and_then(|w| Some((w.clone(), self.frame(&w)?)))
                {
                    // Super + right-drag resizes from the nearest corner.
                    self.start_resize(window, wm::nearest_corner(frame, location), start_data, serial);
                } else if let Some((window, edges)) = zone.filter(|_| button == BTN_LEFT) {
                    self.zone_press(window, edges, start_data, serial);
                } else if let Some((window, part)) = bar {
                    match button {
                        BTN_LEFT => self.bar_press(window, part, start_data, serial),
                        BTN_RIGHT => self.open_window_menu(window, self.to_output(location)),
                        _ => {}
                    }
                }
            }
        }
        if state == ButtonState::Released && button == BTN_LEFT {
            self.shot_release();
            self.bar_release(pointer.current_location());
            self.mission_release(self.to_output(pointer.current_location()));
            self.shell_release();
            self.desktop_release();
            self.launchpad_release();
            self.dock_release(self.to_output(pointer.current_location()));
        }
        pointer.button(self, &ButtonEvent { button, state, serial, time });
        pointer.frame(self);
    }

    /// Mouse keys (E8.6): true when `code` is a keypad key they took.
    fn mouse_key(&mut self, code: Keycode, pressed: bool, time: u32) -> bool {
        let evdev = code.raw().wrapping_sub(8);
        let click = |state: &mut Self, button, states: &[ButtonState]| {
            for s in states {
                state.pointer_button(button, *s, time);
            }
        };
        match evdev {
            76 | 55 | 82 | 83 if !pressed => {}
            76 => click(self, BTN_LEFT, &[ButtonState::Pressed, ButtonState::Released]),
            55 => click(self, BTN_RIGHT, &[ButtonState::Pressed, ButtonState::Released]),
            82 => click(self, BTN_LEFT, &[ButtonState::Pressed]),
            83 => click(self, BTN_LEFT, &[ButtonState::Released]),
            _ if mouse_direction(evdev).is_some() => {
                self.access.mouse_held.retain(|c| *c != evdev);
                if !pressed {
                    return true;
                }
                self.access.mouse_held.push(evdev);
                if self.access.mouse_since.is_none() {
                    self.access.mouse_since = Some(Instant::now());
                    self.mouse_keys_step();
                    let timer = self.loop_handle.insert_source(Timer::from_duration(MOUSE_TICK), |_, _, state| {
                        if state.access.mouse_held.is_empty() {
                            state.access.mouse_since = None;
                            return TimeoutAction::Drop;
                        }
                        state.mouse_keys_step();
                        TimeoutAction::ToDuration(MOUSE_TICK)
                    });
                    if let Err(err) = timer {
                        tracing::warn!("mouse keys: {err}");
                    }
                }
            }
            _ => return false,
        }
        true
    }

    /// One tick of the pointer moving under held keypad keys.
    fn mouse_keys_step(&mut self) {
        let held_for = self.access.mouse_since.map_or(Duration::ZERO, |t| t.elapsed());
        let delta = mouse_step(&self.access.mouse_held, held_for);
        if delta != Point::default() {
            let utime = Duration::from(self.protocols.clock.now()).as_micros() as u64;
            self.relative_motion(delta, utime);
        }
    }

    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        self.idle_activity();
        // Clicks, scrolls and gestures with Super held aren't a Super tap; moving the pointer is fine.
        if !matches!(event, InputEvent::Keyboard { .. } | InputEvent::PointerMotion { .. } | InputEvent::PointerMotionAbsolute { .. }) {
            self.super_tap.cancel();
        }
        match event {
            InputEvent::Keyboard { event } => self.key_event(event.key_code(), event.state(), event.time_msec()),
            InputEvent::PointerMotionAbsolute { event } => {
                let Some(output_geo) = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)) else { return };
                self.pointer_motion(event.position_transformed(output_geo.size) + output_geo.loc.to_f64(), event.time());
            }
            InputEvent::PointerButton { event } => self.assisted_button(event.button_code(), event.state(), event.time_msec()),
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

    /// `focus = "hover"`: the window under `location` gets the keyboard once the pointer rests there, unraised.
    pub fn hover_focus(&mut self, location: Point<f64, Logical>) {
        let grabbed = self.seat.get_pointer().is_some_and(|p| p.is_grabbed());
        let target = (self.config.windows.focus == crate::config::FocusMode::Hover
            && !grabbed
            && self.switcher.open.is_none()
            && !self.shell_covers(self.to_output(location))
            && self.layer_under(location, true).is_none())
        .then(|| self.space.element_under(location).map(|(w, _)| w.clone()))
        .flatten()
        .filter(|w| self.focused_window().as_ref() != Some(w));
        if self.zones.hover_focus.as_ref().map(|(w, _)| w) == target.as_ref() {
            return;
        }
        if let Some((_, token)) = self.zones.hover_focus.take() {
            self.loop_handle.remove(token);
        }
        let Some(window) = target else { return };
        let token = self.loop_handle.insert_source(Timer::from_duration(HOVER_FOCUS), |_, _, state| {
            if let Some((window, _)) = state.zones.hover_focus.take()
                && window.alive()
            {
                state.focus_unraised(&window);
            }
            TimeoutAction::Drop
        });
        if let Ok(token) = token {
            self.zones.hover_focus = Some((window, token));
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
        if self.shell_covers(self.to_output(location)) || self.switcher_covers(location) {
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
        // Rate then delay, as told to apps: 30/s, 500 ms by default; 0 turns repeat off.
        assert_eq!(crate::config::Keyboard::default().repeat(), (30, 500));
    }

    /// Xkb keycode of evdev `code`.
    fn k(code: u32) -> Keycode {
        Keycode::new(code + 8)
    }

    #[test]
    fn sticky_tap_latches_twice_locks_third_unlocks() {
        let (shift, a) = (k(42), k(30));
        let mut keys = AccessKeys::default();
        // Off: everything passes.
        assert_eq!(keys.sticky(shift, true, false), Some(vec![]));
        // Tap: the press passes, the release is kept (Shift stays down), the next key lets it go.
        assert_eq!(keys.sticky(shift, true, true), Some(vec![]));
        assert_eq!(keys.sticky(shift, false, true), None);
        assert_eq!(keys.sticky(a, true, true), Some(vec![shift]));
        assert_eq!(keys.sticky(a, false, true), Some(vec![]));
        // Tap twice: locked, kept through keys; a third tap's release goes through.
        assert_eq!(keys.sticky(shift, true, true), Some(vec![]));
        assert_eq!(keys.sticky(shift, false, true), None);
        assert_eq!(keys.sticky(shift, true, true), None);
        assert_eq!(keys.sticky(shift, false, true), None);
        assert_eq!(keys.stuck(), (vec![], vec![shift]));
        assert_eq!(keys.sticky(a, true, true), Some(vec![]));
        assert_eq!(keys.sticky(shift, true, true), None);
        assert_eq!(keys.sticky(shift, false, true), Some(vec![]));
        assert_eq!(keys.stuck(), (vec![], vec![]));
        // A chord (Shift held while A) is ordinary.
        assert_eq!(keys.sticky(shift, true, true), Some(vec![]));
        assert_eq!(keys.sticky(a, true, true), Some(vec![]));
        assert_eq!(keys.sticky(shift, false, true), Some(vec![]));
        assert_eq!(keys.stuck(), (vec![], vec![]));
    }

    #[test]
    fn five_quick_shifts_toggle() {
        let (shift, a, t) = (k(54), k(30), Instant::now());
        let mut keys = AccessKeys::default();
        let ms = |n| t + Duration::from_millis(n);
        assert!(!(0..4).any(|i| keys.five_shifts(shift, false, ms(i * 200))));
        assert!(keys.five_shifts(shift, false, ms(800)));
        // Another key or a pause starts over.
        assert!(!(0..4).any(|i| keys.five_shifts(shift, false, ms(1000 + i * 200))));
        keys.five_shifts(a, true, ms(1700));
        assert!(!keys.five_shifts(shift, false, ms(1800)));
        assert!(!(0..4).any(|i| keys.five_shifts(shift, false, ms(4000 + i * 1200))));
    }

    #[test]
    fn bounce_drops_a_quick_second_press_and_its_release() {
        let (a, b, t) = (k(30), k(48), Instant::now());
        let window = Duration::from_millis(100);
        let mut keys = AccessKeys::default();
        assert!(keys.bounce(a, true, t, window));
        assert!(keys.bounce(a, false, t, window));
        assert!(!keys.bounce(a, true, t + Duration::from_millis(50), window));
        assert!(!keys.bounce(a, false, t + Duration::from_millis(60), window));
        // Another key, or the same one later, passes.
        assert!(keys.bounce(b, true, t + Duration::from_millis(70), window));
        assert!(keys.bounce(a, true, t + Duration::from_millis(150), window));
    }

    #[test]
    fn slow_keys_count_only_presses_still_held() {
        let (a, t) = (k(30), Instant::now());
        let mut keys = AccessKeys::default();
        keys.slow_press(a, t);
        // Let go early: the release is dropped and the timer finds nothing.
        assert!(!keys.slow_release(a));
        assert!(!keys.slow_fire(a, t));
        // Held: it counts, and its release goes through.
        let later = t + Duration::from_millis(10);
        keys.slow_press(a, later);
        assert!(!keys.slow_fire(a, t), "an old timer doesn't count a newer press");
        assert!(keys.slow_fire(a, later));
        assert!(keys.slow_release(a));
    }

    #[test]
    fn mouse_keys_speed_up_and_combine() {
        let step = |held: &[u32], ms| mouse_step(held, Duration::from_millis(ms));
        assert_eq!(step(&[77], 0), (1.0, 0.0).into());
        assert_eq!(step(&[71], 2000), (-20.0, -20.0).into());
        assert_eq!(step(&[72, 75], 0), (-1.0, -1.0).into());
        assert_eq!(step(&[75, 77], 0), (0.0, 0.0).into());
    }
}
