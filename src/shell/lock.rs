//! The lock screen (E6.4): drawn by the compositor itself, so while it is up no app is drawn and no input reaches
//! one. The password is checked by PAM on a worker thread. `ext-session-lock` clients (swaylock and friends) can lock
//! too: their surfaces are drawn instead of ours, and if one dies without unlocking, our lock screen takes over, so it
//! never fails open. Drawn by render/lock.rs.

use std::time::{Duration, Instant};

use smithay::{
    delegate_session_lock,
    input::keyboard::Keysym,
    reexports::{
        calloop::{RegistrationToken, channel},
        wayland_server::protocol::wl_output::WlOutput,
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Size},
    wayland::session_lock::{LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker},
};

use crate::{
    shell::alert::Power,
    state::State,
    ui::field::{Edit, TextField},
    wm::Rect,
};

/// How long a wrong password shakes the field.
pub const SHAKE: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Sleep,
    Restart,
    ShutDown,
}

impl Button {
    pub const ALL: [Button; 3] = [Button::Sleep, Button::Restart, Button::ShutDown];

    pub fn label(self) -> &'static str {
        match self {
            Button::Sleep => "Sleep",
            Button::Restart => "Restart",
            Button::ShutDown => "Shut Down",
        }
    }
}

#[derive(Debug, Default)]
pub struct Lock {
    pub field: TextField,
    /// The last wrong password: the field shakes, then "Incorrect password" shows until the next key.
    pub wrong: Option<Instant>,
    /// PAM is checking; its answer comes through this channel source.
    pub checking: Option<RegistrationToken>,
    /// Notifications that arrived while locked; only their count shows (privacy).
    pub missed: usize,
    /// logind waits for the lock frame before the machine sleeps (E6.5).
    pub sleep_pending: bool,
    pub hover: Option<Button>,
    /// The keyboard layout's short name for the status corner ("US").
    pub keyboard: String,
    /// Taken by an ext-session-lock client: `unlock` comes from it, not from PAM.
    pub client: bool,
    /// The client's surfaces, drawn instead of ours while one is alive.
    pub surfaces: Vec<LockSurface>,
}

/// What a key did on the lock screen.
#[derive(Debug, PartialEq)]
pub enum Key {
    /// Check this password.
    Submit(String),
    Redraw,
    Nothing,
}

impl Lock {
    pub fn new() -> Self {
        Self { field: TextField::secure(), ..Self::default() }
    }

    /// One key press. Nothing is typed while PAM is still checking.
    pub fn key(&mut self, keysym: Keysym, utf8: Option<&str>, ctrl: bool) -> Key {
        if self.checking.is_some() {
            return Key::Nothing;
        }
        match self.field.key(keysym, utf8, ctrl) {
            Edit::Submit if self.field.text.is_empty() => Key::Nothing,
            Edit::Submit => Key::Submit(self.field.text.clone()),
            Edit::Cancel => {
                wipe(&mut self.field);
                Key::Redraw
            }
            Edit::Changed => {
                self.wrong = None;
                Key::Redraw
            }
            Edit::Moved => Key::Redraw,
            Edit::Ignored => Key::Nothing,
        }
    }

    /// PAM answered: `true` unlocks. A wrong password empties the field and shakes it.
    pub fn checked(&mut self, ok: bool, now: Instant) -> bool {
        self.checking = None;
        wipe(&mut self.field);
        if !ok {
            self.wrong = Some(now);
        }
        ok
    }

    /// The field's sideways offset now; `None` once it has settled.
    pub fn shake(&self, now: Instant) -> Option<i32> {
        let t = now.saturating_duration_since(self.wrong?);
        (t < SHAKE).then(|| shake_offset(t))
    }

    /// A client lock surface that can take input.
    pub fn client_surface(&self) -> Option<&LockSurface> {
        self.surfaces.iter().find(|s| s.alive())
    }
}

/// Overwrites a password before it is dropped.
pub fn wipe(field: &mut TextField) {
    let mut bytes = std::mem::take(&mut field.text).into_bytes();
    bytes.fill(0);
    *field = TextField::secure();
}

/// Three swings that fade out over `SHAKE`, 10 px at most (macOS's "no" shake).
pub fn shake_offset(t: Duration) -> i32 {
    let k = t.as_secs_f64() / SHAKE.as_secs_f64();
    if k >= 1.0 {
        return 0;
    }
    (10.0 * (1.0 - k) * (k * 3.0 * std::f64::consts::TAU).sin()).round() as i32
}

/// Where everything sits on an output of `size` (logical px), per the E6.4 sketch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    pub date: Rect,
    pub clock: Rect,
    pub missed: Rect,
    pub avatar: Rect,
    pub name: Rect,
    pub field: Rect,
    pub hint: Rect,
    pub buttons: [Rect; 3],
    /// Keyboard layout and battery, right-aligned in it.
    pub status: Rect,
}

pub const AVATAR: i32 = 64;
pub const FIELD_W: i32 = 200;
pub const FIELD_H: i32 = 30;
const BUTTON_W: i32 = 84;
const BUTTON_H: i32 = 28;
const EDGE: i32 = 24;

pub fn layout(size: Size<i32, Logical>) -> Layout {
    let (w, h) = (size.w, size.h);
    let centred = |width: i32, y: i32, height: i32| Rectangle::new(((w - width) / 2, y).into(), (width, height).into());
    let top = (h / 10).max(32);
    let date = centred(w.min(600), top, 22);
    let clock = centred(w.min(600), top + 22, 100);
    let missed = centred(w.min(600), clock.loc.y + 100 + 8, 15);
    // The user block sits in the lower third, above the buttons.
    let avatar_y = (h * 64 / 100).min(h - EDGE - BUTTON_H - 24 - (AVATAR + 12 + 16 + 16 + FIELD_H + 8 + 15)).max(missed.loc.y + 40);
    let avatar = centred(AVATAR, avatar_y, AVATAR);
    let name = centred(w.min(400), avatar_y + AVATAR + 12, 16);
    let field = centred(FIELD_W, name.loc.y + 16 + 16, FIELD_H);
    let hint = centred(w.min(400), field.loc.y + FIELD_H + 8, 15);
    let y = h - EDGE - BUTTON_H;
    let row = 3 * BUTTON_W + 2 * 8;
    let buttons = [0, 1, 2].map(|i| Rectangle::new(((w - row) / 2 + i * (BUTTON_W + 8), y).into(), (BUTTON_W, BUTTON_H).into()));
    let status = Rectangle::new((w - EDGE - 200, y).into(), (200, BUTTON_H).into());
    Layout { date, clock, missed, avatar, name, field, hint, buttons, status }
}

impl State {
    pub fn locked(&self) -> bool {
        self.lock.is_some()
    }

    /// Locks now (Super+Ctrl+Q, the logo menu, idle, sleep): popups close, the keyboard and pointer leave every app.
    pub fn lock_screen(&mut self) {
        if self.lock.is_some() {
            return;
        }
        tracing::info!("screen locked");
        self.popup = None;
        self.spotlight = None;
        let layout = self.config.keyboard.resolved().layout;
        let keyboard = layout.split(',').next().unwrap_or_default().to_uppercase();
        self.lock = Some(Lock { keyboard, ..Lock::new() });
        let serial = SERIAL_COUNTER.next_serial();
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, None, serial);
        }
        if let Some(pointer) = self.seat.get_pointer() {
            let location = pointer.current_location();
            pointer.motion(self, None, &smithay::input::pointer::MotionEvent { location, serial, time: 0 });
            pointer.frame(self);
        }
        self.schedule_redraw();
    }

    fn unlock_screen(&mut self) {
        let Some(lock) = self.lock.take() else { return };
        if let Some(token) = lock.checking {
            self.loop_handle.remove(token);
        }
        tracing::info!("screen unlocked");
        let top = self.top_window();
        self.focus_window(top, SERIAL_COUNTER.next_serial());
        self.schedule_redraw();
    }

    /// Every key while locked: `true` if the lock screen took it (all of them, unless a client's lock surface has the
    /// keyboard).
    pub fn lock_key(&mut self, keysym: Keysym, ctrl: bool) -> bool {
        let Some(lock) = self.lock.as_mut() else { return false };
        if let Some(surface) = lock.client_surface() {
            let focus = self.seat.get_keyboard().and_then(|k| k.current_focus());
            return focus.as_ref() != Some(surface.wl_surface());
        }
        match lock.key(keysym, keysym.key_char().map(String::from).as_deref(), ctrl) {
            Key::Submit(password) => self.lock_submit(password),
            Key::Redraw => self.schedule_redraw(),
            Key::Nothing => {}
        }
        true
    }

    /// Checks `password` with PAM on a worker thread; the answer unlocks or shakes the field.
    fn lock_submit(&mut self, password: String) {
        let (tx, rx) = channel::channel();
        let token = self.loop_handle.insert_source(rx, |event, _, state| {
            let channel::Event::Msg(ok) = event else { return };
            let token = state.lock.as_mut().and_then(|l| l.checking.take());
            if let Some(token) = token {
                state.loop_handle.insert_idle(move |state| state.loop_handle.remove(token));
            }
            state.lock_checked(ok);
        });
        let Ok(token) = token.inspect_err(|err| tracing::warn!("lock screen: {err}")) else { return };
        let (user, _) = crate::pam::user();
        let spawned = std::thread::Builder::new().name("pam".into()).spawn(move || {
            let mut password = password.into_bytes();
            let ok = crate::pam::authenticate(&user, &String::from_utf8_lossy(&password));
            password.fill(0);
            let _ = tx.send(ok);
        });
        match spawned {
            Ok(_) => {
                if let Some(lock) = self.lock.as_mut() {
                    lock.checking = Some(token);
                }
            }
            Err(err) => {
                tracing::warn!("lock screen: {err}");
                self.loop_handle.remove(token);
            }
        }
        self.schedule_redraw();
    }

    fn lock_checked(&mut self, ok: bool) {
        let Some(lock) = self.lock.as_mut() else { return };
        if lock.checked(ok, Instant::now()) {
            return self.unlock_screen();
        }
        tracing::info!("lock screen: incorrect password");
        // Redraw every frame while the field shakes.
        self.loop_handle
            .insert_source(smithay::reexports::calloop::timer::Timer::from_duration(Duration::ZERO), |_, _, state| {
                state.schedule_redraw();
                match state.lock.as_ref().and_then(|l| l.shake(Instant::now())) {
                    Some(_) => smithay::reexports::calloop::timer::TimeoutAction::ToDuration(Duration::from_millis(16)),
                    None => smithay::reexports::calloop::timer::TimeoutAction::Drop,
                }
            })
            .map_or_else(|err| tracing::warn!("lock screen: {err}"), drop);
    }

    /// The lock screen's layout on the first output.
    fn lock_layout(&self) -> Option<Layout> {
        let output = self.space.outputs().next()?;
        Some(layout(self.space.output_geometry(output)?.size))
    }

    /// The pointer moved while locked (`location` output-relative): the bottom buttons hover.
    pub fn lock_motion(&mut self, location: Point<f64, Logical>) {
        let hit = self.lock_layout().and_then(|l| Button::ALL.into_iter().zip(l.buttons).find(|(_, r)| r.to_f64().contains(location)).map(|(b, _)| b));
        if let Some(lock) = self.lock.as_mut().filter(|l| l.hover != hit) {
            lock.hover = hit;
            self.schedule_redraw();
        }
    }

    /// A press while locked: Sleep, Restart or Shut Down when it hits one.
    pub fn lock_press(&mut self, location: Point<f64, Logical>) {
        if self.lock.as_ref().is_some_and(|l| l.client_surface().is_some()) {
            return;
        }
        let hit = self.lock_layout().and_then(|l| Button::ALL.into_iter().zip(l.buttons).find(|(_, r)| r.to_f64().contains(location)).map(|(b, _)| b));
        match hit {
            Some(Button::Sleep) => self.sleep(),
            Some(Button::Restart) => self.power_now(Power::Restart),
            Some(Button::ShutDown) => self.power_now(Power::ShutDown),
            None => {}
        }
    }

    /// Sleep (logo menu, lock screen, idle): locks first, so waking never shows the desktop. Nested, the machine under
    /// us is never put to sleep: only the lock happens.
    pub fn sleep(&mut self) {
        self.lock_screen();
        if self.nested {
            tracing::warn!("sleep: not done in a nested session");
        } else {
            self.services.send(crate::services::Command::Suspend);
        }
    }

    /// logind is about to sleep: lock, and let it go once that frame is on screen (see `lock_frame_shown`).
    pub fn prepare_for_sleep(&mut self) {
        self.lock_screen();
        if let Some(lock) = self.lock.as_mut() {
            lock.sleep_pending = true;
        }
        self.schedule_redraw();
    }

    /// After a frame was shown: a sleep waiting for the lock frame may go ahead.
    pub fn lock_frame_shown(&mut self) {
        if let Some(lock) = self.lock.as_mut().filter(|l| l.sleep_pending) {
            lock.sleep_pending = false;
            self.services.send(crate::services::Command::SleepReady);
        }
    }
}

impl SessionLockHandler for State {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.idle.session_lock
    }

    /// A locker client asks to lock. Refused (dropping `confirmation` says so) while another client holds the lock.
    /// ponytail: confirmed at once, not after the first frame with its surface is presented; nothing of the apps is
    /// drawn from this point on anyway.
    fn lock(&mut self, confirmation: SessionLocker) {
        if self.lock.as_ref().is_some_and(|l| l.client && l.client_surface().is_some()) {
            return;
        }
        self.lock_screen();
        if let Some(lock) = self.lock.as_mut() {
            lock.client = true;
        }
        confirmation.lock();
    }

    fn unlock(&mut self) {
        if self.lock.as_ref().is_some_and(|l| l.client) {
            self.unlock_screen();
        }
    }

    /// ponytail: one output; every lock surface gets its size.
    fn new_surface(&mut self, surface: LockSurface, _output: WlOutput) {
        let Some(size) = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map(|g| g.size) else { return };
        surface.with_pending_state(|s| s.size = Some((size.w.max(1) as u32, size.h.max(1) as u32).into()));
        surface.send_configure();
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, Some(surface.wl_surface().clone()), SERIAL_COUNTER.next_serial());
        }
        if let Some(lock) = self.lock.as_mut() {
            lock.surfaces.retain(LockSurface::alive);
            lock.surfaces.push(surface);
        }
    }
}

delegate_session_lock!(State);

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(lock: &mut Lock, s: &str) {
        for c in s.chars() {
            assert_eq!(lock.key(Keysym::NoSymbol, Some(&c.to_string()), false), Key::Redraw);
        }
    }

    /// The whole flow with a mock checker in place of PAM.
    #[test]
    fn unlock_flow_with_mock_check() {
        let check = |password: &str| password == "right";
        let now = Instant::now();
        let mut lock = Lock::new();
        assert_eq!(lock.key(Keysym::Return, Some("\r"), false), Key::Nothing, "an empty field submits nothing");

        typed(&mut lock, "wrong");
        assert_eq!(lock.field.display(), "•••••");
        let Key::Submit(password) = lock.key(Keysym::Return, Some("\r"), false) else { panic!("no submit") };
        lock.checking = Some(dummy_token());
        assert_eq!(lock.key(Keysym::a, Some("a"), false), Key::Nothing, "no typing while PAM checks");
        assert!(!lock.checked(check(&password), now));
        assert!(lock.field.text.is_empty() && lock.checking.is_none());
        assert!(lock.shake(now).is_some() && lock.shake(now + SHAKE).is_none());
        assert!(lock.wrong.is_some(), "the hint stays until the next key");

        typed(&mut lock, "right");
        assert!(lock.wrong.is_none());
        let Key::Submit(password) = lock.key(Keysym::KP_Enter, None, false) else { panic!("no submit") };
        assert!(lock.checked(check(&password), now));
        assert!(lock.field.text.is_empty(), "the password is not kept");
    }

    fn dummy_token() -> RegistrationToken {
        // A token from a throwaway loop: only its presence matters to `Lock`.
        let event_loop = smithay::reexports::calloop::EventLoop::<()>::try_new().unwrap();
        event_loop
            .handle()
            .insert_source(smithay::reexports::calloop::timer::Timer::immediate(), |_, _, _| smithay::reexports::calloop::timer::TimeoutAction::Drop)
            .unwrap()
    }

    #[test]
    fn escape_clears() {
        let mut lock = Lock::new();
        typed(&mut lock, "abc");
        assert_eq!(lock.key(Keysym::Escape, None, false), Key::Redraw);
        assert!(lock.field.text.is_empty() && lock.field.secure);
    }

    #[test]
    fn shake_swings_and_settles() {
        assert_eq!(shake_offset(Duration::ZERO), 0);
        let swings: Vec<i32> = (0..40).map(|ms| shake_offset(Duration::from_millis(ms * 10))).collect();
        assert!(swings.iter().any(|&x| x >= 6) && swings.iter().any(|&x| x <= -6));
        assert!(swings.iter().all(|x| x.abs() <= 10));
        assert_eq!(shake_offset(SHAKE), 0);
    }

    #[test]
    fn layout_is_centred_and_ordered() {
        for size in [Size::from((1280, 800)), Size::from((1920, 1080)), Size::from((800, 600))] {
            let l = layout(size);
            let mid = |r: Rect| r.loc.x + r.size.w / 2;
            assert!([l.date, l.clock, l.avatar, l.name, l.field].iter().all(|r| (mid(*r) - size.w / 2).abs() <= 1), "{size:?}");
            assert!(l.date.loc.y < l.clock.loc.y && l.clock.loc.y + 100 <= l.missed.loc.y && l.missed.loc.y < l.avatar.loc.y);
            assert!(l.avatar.loc.y + AVATAR < l.name.loc.y && l.name.loc.y < l.field.loc.y && l.field.loc.y < l.hint.loc.y);
            assert!(l.hint.loc.y + l.hint.size.h < l.buttons[0].loc.y, "{size:?}: the user block clears the buttons");
            assert!(l.buttons[0].loc.x + l.buttons[0].size.w < l.buttons[1].loc.x);
            assert!(l.buttons.iter().all(|b| b.loc.y + b.size.h <= size.h - EDGE));
            assert_eq!(l.field.size, (FIELD_W, FIELD_H).into());
        }
    }
}
