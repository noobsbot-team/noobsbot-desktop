//! The lock screen (E6.4): drawn by the compositor itself, so while it is up no app is drawn and no input reaches
//! one. The password is checked by PAM on a worker thread. `ext-session-lock` clients (swaylock and friends) can lock
//! too: their surfaces are drawn instead of ours, and if one dies without unlocking, our lock screen takes over, so it
//! never fails open. Drawn by render/lock.rs.

use std::time::{Duration, Instant};

use smithay::{
    delegate_session_lock,
    input::keyboard::Keysym,
    output::Output,
    reexports::{
        calloop::{
            RegistrationToken, channel,
            timer::{TimeoutAction, Timer},
        },
        wayland_protocols::ext::session_lock::v1::server::ext_session_lock_v1::ExtSessionLockV1,
        wayland_server::{Resource, protocol::wl_output::WlOutput},
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Size},
    wayland::session_lock::{LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker},
};

use crate::{
    config::Previews,
    services::{Command, fingerprint::Verify, notifications::Notification},
    shell::alert::Power,
    state::State,
    text::ts,
    ui::field::{Edit, TextField},
    wm::Rect,
};

/// How long PAM may take before the attempt counts as wrong.
const PAM_TIMEOUT: Duration = Duration::from_secs(20);

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
    /// Notifications that arrived while locked and aren't listed; only their count shows (privacy).
    pub missed: usize,
    /// Notifications listed under the clock, newest first (`listed`), at most `MAX_LISTED`.
    pub listed: Vec<Listed>,
    /// logind waits for the lock frame before the machine sleeps (E6.5).
    pub sleep_pending: bool,
    pub hover: Option<Button>,
    /// The keyboard layout's short name for the status corner ("US").
    pub keyboard: String,
    /// Taken by an ext-session-lock client: `unlock` comes from it, not from PAM.
    pub client: bool,
    /// That client's confirmed lock object: only its `unlock_and_destroy` unlocks.
    pub locker: Option<ExtSessionLockV1>,
    /// The client's surfaces, each with the display it covers, drawn instead of ours while one is alive.
    pub surfaces: Vec<(LockSurface, Output)>,
    /// Greeter mode (E7.2): users, sessions, greetd's last error.
    pub greeter: Option<crate::shell::greeter::Greeter>,
    /// fprintd's verify, shown under the field.
    pub finger: Verify,
}

/// A notification on the lock screen: its app, title, and body (empty unless previews are Always).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub app: String,
    pub title: String,
    pub body: String,
}

/// The lock screen lists this many notifications; older ones join the count.
pub const MAX_LISTED: usize = 3;

/// The privacy rule: `n` is listed only when its app allows it (`[notification_apps]` `lock`) and previews aren't
/// Never; its body only with previews Always. `None`: only counted.
pub fn listed(n: &Notification, allowed: bool, previews: Previews) -> Option<Listed> {
    (allowed && previews != Previews::Never).then(|| Listed {
        app: n.app.clone(),
        title: n.summary.clone(),
        body: if previews == Previews::Always { n.body.clone() } else { String::new() },
    })
}

/// What a key did on the lock screen.
#[derive(PartialEq)]
pub enum Key {
    /// Check this password.
    Submit(String),
    Redraw,
    Nothing,
}

/// Not derived: the password stays out of logs and assertion messages.
impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Key::Submit(_) => "Submit(…)",
            Key::Redraw => "Redraw",
            Key::Nothing => "Nothing",
        })
    }
}

impl Lock {
    /// A notification that arrived while locked: listed (newest first, the oldest then counted), or counted.
    pub fn arrived(&mut self, listed: Option<Listed>) {
        match listed {
            Some(l) => {
                self.listed.insert(0, l);
                if self.listed.len() > MAX_LISTED {
                    self.listed.pop();
                    self.missed += 1;
                }
            }
            None => self.missed += 1,
        }
    }

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
                if let Some(g) = self.greeter.as_mut() {
                    g.error = None;
                }
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
        self.surfaces.iter().map(|(s, _)| s).find(|s| s.alive())
    }

    /// The client's live lock surface on `output`.
    pub fn surface_on(&self, output: &Output) -> Option<&LockSurface> {
        self.surfaces.iter().find(|(s, o)| o == output && s.alive()).map(|(s, _)| s)
    }
}

/// The size a client's lock surface is configured with on a display of `size`: all of it, never 0.
pub fn surface_size(size: Size<i32, Logical>) -> (u32, u32) {
    (size.w.max(1) as u32, size.h.max(1) as u32)
}

/// Overwrites a password before it is dropped.
pub fn wipe(field: &mut TextField) {
    let mut bytes = std::mem::take(&mut field.text).into_bytes();
    crate::pam::zero(&mut bytes);
    *field = TextField::secure();
}

/// Three swings that fade out over `SHAKE`, 10 px at most (a "no" shake).
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
    /// The listed notifications' cards under the clock, as many as fit.
    pub cards: [Option<Rect>; MAX_LISTED],
    pub missed: Rect,
    /// `[idle] lock_message`.
    pub message: Rect,
    pub avatar: Rect,
    pub name: Rect,
    pub field: Rect,
    pub hint: Rect,
    pub buttons: [Rect; 3],
    /// Keyboard layout and battery, right-aligned in it.
    pub status: Rect,
    /// The Now Playing card, above the user block; `None` when the screen is too short for it.
    pub media: Option<Rect>,
    /// The keyboard layout pill right of the field, with more than one layout (E8.7).
    pub layouts: Rect,
}

pub const AVATAR: i32 = 64;
pub const FIELD_W: i32 = 200;
pub const FIELD_H: i32 = 30;
const BUTTON_W: i32 = 84;
const BUTTON_H: i32 = 28;
const EDGE: i32 = 24;
pub const MEDIA_W: i32 = 320;
pub const MEDIA_H: i32 = 64;
/// The album art's side in the Now Playing card.
pub const ART: i32 = 48;
const PILL_W: i32 = 40;
/// A listed notification's card.
const CARD_W: i32 = 360;

/// Rows, buttons, the field and the Now Playing card grow with the text size (E8.4); the picture and gaps stay.
/// `listed`: the notifications listed under the clock, each a card of app and title (and body, if any has one).
pub fn layout(size: Size<i32, Logical>, listed: &[Listed]) -> Layout {
    let (w, h) = (size.w, size.h);
    let centred = |width: i32, y: i32, height: i32| Rectangle::new(((w - width) / 2, y).into(), (width, height).into());
    let (title_h, clock_h, callout_h, headline_h) = (ts(22), ts(100), ts(15), ts(16));
    let (field_w, field_h, button_w, button_h) = (ts(FIELD_W), ts(FIELD_H), ts(BUTTON_W), ts(BUTTON_H));
    let top = (h / 10).max(32);
    let date = centred(w.min(600), top, title_h);
    let clock = centred(w.min(600), top + title_h, clock_h);
    // The user block sits in the lower third, above the buttons; cards that would push it down are left out.
    let block = AVATAR + 12 + headline_h + 16 + field_h + 8 + callout_h;
    let avatar_top = (h * 64 / 100).min(h - EDGE - button_h - 24 - block);
    let bodies = listed.iter().any(|l| !l.body.is_empty());
    let card_h = 8 + callout_h + 2 + headline_h + if bodies { 2 + headline_h } else { 0 } + 8;
    let below_clock = clock.loc.y + clock_h + 8;
    let fit = (0..=listed.len().min(MAX_LISTED)).rev().find(|&n| below_clock + n as i32 * (card_h + 6) + 40 <= avatar_top).unwrap_or(0);
    let card_w = ts(CARD_W).min(w - 2 * EDGE);
    let cards = std::array::from_fn(|i| (i < fit).then(|| centred(card_w, below_clock + i as i32 * (card_h + 6), card_h)));
    let missed = centred(w.min(600), below_clock + fit as i32 * (card_h + 6), callout_h);
    let message = centred(w.min(600), missed.loc.y + ts(20), callout_h);
    let avatar_y = avatar_top.max(missed.loc.y + 40);
    let avatar = centred(AVATAR, avatar_y, AVATAR);
    let name = centred(w.min(400), avatar_y + AVATAR + 12, headline_h);
    let field = centred(field_w, name.loc.y + headline_h + 16, field_h);
    let hint = centred(w.min(400), field.loc.y + field_h + 8, callout_h);
    let y = h - EDGE - button_h;
    let row = 3 * button_w + 2 * 8;
    let buttons = [0, 1, 2].map(|i| Rectangle::new(((w - row) / 2 + i * (button_w + 8), y).into(), (button_w, button_h).into()));
    let status = Rectangle::new((w - EDGE - ts(200), y).into(), (ts(200), button_h).into());
    let media_h = ts(MEDIA_H);
    let media = centred(ts(MEDIA_W).min(w - 2 * EDGE), avatar_y - 16 - media_h, media_h);
    let media = (media.loc.y >= message.loc.y + message.size.h + 8).then_some(media);
    let layouts = Rectangle::new((field.loc.x + field_w + 8, field.loc.y).into(), (ts(PILL_W), field_h).into());
    Layout { date, clock, cards, missed, message, avatar, name, field, hint, buttons, status, layouts, media }
}

/// The layout pill's label ("EN"), shown with more than one layout.
pub fn layout_pill(layouts: &crate::ime::Layouts) -> Option<String> {
    layouts.list.get(layouts.active).filter(|_| layouts.list.len() > 1).map(|(code, _)| code.to_uppercase())
}

/// The Now Playing button under `location` in `card`: previous, play/pause, next (the control centre's spots).
pub fn media_hit(card: Rect, location: Point<f64, Logical>) -> Option<crate::services::media::Action> {
    use crate::services::media::Action;
    let buttons = crate::shell::control::media_buttons(card);
    [Action::Previous, Action::PlayPause, Action::Next].into_iter().zip(buttons).find(|(_, r)| r.to_f64().contains(location)).map(|(a, _)| a)
}

/// Whether fprintd's word on verify `tag` counts: only the `current` verify's, on our own lock screen, and none while
/// the machine goes to sleep (a finger on a power-button sensor) or while the session is in the background.
pub fn finger_counts(lock: &Lock, tag: u64, current: u64, active: bool) -> bool {
    tag == current && active && !lock.sleep_pending && !lock.client && lock.greeter.is_none()
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
        // A turn later: a locker client or the login screen takes the lock first, and needs no fingerprint.
        self.loop_handle.insert_idle(|state| state.fingerprint(true));
        let serial = SERIAL_COUNTER.next_serial();
        // Grabs go too: a popup's, or an input method's, which would otherwise see the password typed.
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.unset_grab(self);
            keyboard.set_focus(self, None, serial);
        }
        if let Some(pointer) = self.seat.get_pointer() {
            pointer.unset_grab(self, serial, 0);
            let location = pointer.current_location();
            pointer.motion(self, None, &smithay::input::pointer::MotionEvent { location, serial, time: 0 });
            pointer.frame(self);
        }
        self.schedule_redraw();
    }

    fn unlock_screen(&mut self) {
        // The login screen only goes away by exiting into the session.
        if self.greeter().is_some() {
            return;
        }
        let Some(lock) = self.lock.take() else { return };
        if let Some(token) = lock.checking {
            self.loop_handle.remove(token);
        }
        self.fingerprint(false);
        tracing::info!("screen unlocked");
        let top = self.top_window();
        self.focus_window(top, SERIAL_COUNTER.next_serial());
        self.ask_thunderbolt();
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
        if self.greeter_key(keysym) {
            return true;
        }
        let Some(lock) = self.lock.as_mut() else { return false };
        match lock.key(keysym, keysym.key_char().map(String::from).as_deref(), ctrl) {
            Key::Submit(password) => self.lock_submit(password),
            Key::Redraw => self.schedule_redraw(),
            Key::Nothing => {}
        }
        true
    }

    /// Checks `password` with PAM on a worker thread; the answer unlocks or shakes the field.
    fn lock_submit(&mut self, password: String) {
        if self.greeter().is_some() {
            return self.greeter_submit(password);
        }
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
            crate::pam::zero(&mut password);
            let _ = tx.send(ok);
        });
        match spawned {
            Ok(_) => {
                if let Some(lock) = self.lock.as_mut() {
                    lock.checking = Some(token);
                }
                // A PAM module waiting on a server that's gone would keep the field locked for good: after
                // PAM_TIMEOUT the attempt counts as wrong and its late answer is dropped.
                let timeout = Timer::from_duration(PAM_TIMEOUT);
                let _ = self.loop_handle.insert_source(timeout, move |_, _, state| {
                    if state.lock.as_mut().is_some_and(|l| l.checking.take_if(|t| *t == token).is_some()) {
                        state.loop_handle.remove(token);
                        tracing::warn!("lock screen: no answer from PAM in {PAM_TIMEOUT:?}");
                        state.lock_checked(false);
                    }
                    TimeoutAction::Drop
                });
            }
            Err(err) => {
                tracing::warn!("lock screen: {err}");
                self.loop_handle.remove(token);
            }
        }
        self.schedule_redraw();
    }

    /// Starts (`on`) or stops fprintd's verify, each call under a new tag (`finger_counts`). Only on our own lock
    /// screen: not on the login screen, nor under a locker client, which checks its own way, nor while the session is
    /// in the background (the reader stays free for the session in front).
    /// ponytail: a client that dies and leaves ours up gets none until the next lock.
    pub fn fingerprint(&mut self, on: bool) {
        let active = self.session_active();
        let mut user = None;
        if let Some(lock) = self.lock.as_mut().filter(|l| !l.client && l.greeter.is_none()) {
            lock.finger = Verify::Off;
            user = (on && active).then(|| crate::pam::user().0).filter(|u| !u.is_empty());
        }
        self.fingerprint_tag += 1;
        self.services.send(Command::Fingerprint(self.fingerprint_tag, user));
    }

    /// fprintd's verify `tag` said something: a touch wakes the screen, and a match unlocks as the right password does.
    pub fn fingerprint_changed(&mut self, tag: u64, finger: Verify) {
        let (current, active) = (self.fingerprint_tag, self.session_active());
        let Some(lock) = self.lock.as_mut().filter(|l| finger_counts(l, tag, current, active)) else { return };
        lock.finger = finger;
        if finger == Verify::Match {
            // Anything typed is wiped, as after a password check.
            wipe(&mut lock.field);
            tracing::info!("lock screen: fingerprint matched");
            self.unlock_screen();
        }
        // Only a touch: the first "touch the sensor" comes as the screen locks, maybe from idle, and mustn't wake it.
        if matches!(finger, Verify::Match | Verify::NoMatch) {
            self.idle_activity();
        }
    }

    pub(super) fn lock_checked(&mut self, ok: bool) {
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
    pub(crate) fn lock_layout(&self) -> Option<Layout> {
        let output = self.space.outputs().next()?;
        Some(layout(self.space.output_geometry(output)?.size, self.lock.as_ref().map_or(&[], |l| l.listed.as_slice())))
    }

    /// The pointer moved while locked (`location` output-relative): the bottom buttons hover.
    pub fn lock_motion(&mut self, location: Point<f64, Logical>) {
        let hit = self.lock_layout().and_then(|l| Button::ALL.into_iter().zip(l.buttons).find(|(_, r)| r.to_f64().contains(location)).map(|(b, _)| b));
        if let Some(lock) = self.lock.as_mut().filter(|l| l.hover != hit) {
            lock.hover = hit;
            self.schedule_redraw();
        }
    }

    /// A press while locked: Sleep, Restart, Shut Down or a Now Playing button when it hits one.
    pub fn lock_press(&mut self, location: Point<f64, Logical>) {
        if self.lock.as_ref().is_some_and(|l| l.client_surface().is_some()) {
            return;
        }
        let Some(layout) = self.lock_layout() else { return };
        if self.greeter_press(&layout, location) {
            return;
        }
        // Now Playing's buttons (never on the login screen: no session player there).
        if self.greeter().is_none()
            && let (Some(media), Some(card)) = (self.media.as_ref(), layout.media)
            && let Some(action) = media_hit(card, location)
        {
            return self.services.send(crate::services::Command::Media { player: media.player.clone(), action });
        }
        if layout_pill(&self.layouts).is_some() && layout.layouts.to_f64().contains(location) {
            return self.switch_layout(None);
        }
        let hit = Button::ALL.into_iter().zip(layout.buttons).find(|(_, r)| r.to_f64().contains(location)).map(|(b, _)| b);
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
        // After `lock_screen`'s start, if it queued one; started again on `Resumed`.
        self.loop_handle.insert_idle(|state| state.fingerprint(false));
        self.schedule_redraw();
    }

    /// A display changed size (mode, scale, rotation): a locker client's surface on it is sized to it again. Our own
    /// lock screen is laid out from the display's size every frame; until the client draws the new size, the
    /// wallpaper shows where its old one doesn't reach.
    pub fn refit_lock(&self) {
        for (surface, output) in self.lock.iter().flat_map(|l| &l.surfaces).filter(|(s, _)| s.alive()) {
            if let Some(geo) = self.space.output_geometry(output) {
                // Sent only when the size differs from the last one sent.
                surface.with_pending_state(|s| s.size = Some(surface_size(geo.size).into()));
                surface.send_configure();
            }
        }
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

    /// A locker client asks to lock. Refused (dropping `confirmation` says so) while any lock is up: ours, or
    /// another client's whose surfaces are alive. Otherwise a client could lock over our screen and then unlock the
    /// session. A locker that died (no surfaces left) can be replaced.
    /// ponytail: confirmed at once, not after the first frame with its surface is presented; nothing of the apps is
    /// drawn from this point on anyway.
    fn lock(&mut self, confirmation: SessionLocker) {
        if self.lock.as_ref().is_some_and(|l| !l.client || l.client_surface().is_some()) {
            return;
        }
        self.lock_screen();
        if let Some(lock) = self.lock.as_mut() {
            lock.client = true;
            lock.locker = Some(confirmation.ext_session_lock().clone());
        }
        confirmation.lock();
    }

    /// `unlock_and_destroy`. smithay calls this for any client's lock object, confirmed or not (an unconfirmed one
    /// gets a protocol error as well), and doesn't say whose. So the unlock waits a loop turn: by then the confirmed
    /// object is gone if it was the one destroyed, and still alive if another client asked.
    fn unlock(&mut self) {
        if !self.lock.as_ref().is_some_and(|l| l.client) {
            return;
        }
        self.loop_handle.insert_idle(|state| {
            if state.lock.as_ref().is_some_and(|l| l.client && l.locker.as_ref().is_none_or(|lock| !lock.is_alive())) {
                state.unlock_screen();
            }
        });
    }

    /// A surface for `output`, sized to that display (and again when it changes, `refit_lock`).
    fn new_surface(&mut self, surface: LockSurface, output: WlOutput) {
        let Some(output) = Output::from_resource(&output) else { return };
        let Some(size) = self.space.output_geometry(&output).map(|g| g.size) else { return };
        surface.with_pending_state(|s| s.size = Some(surface_size(size).into()));
        surface.send_configure();
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, Some(surface.wl_surface().clone()), SERIAL_COUNTER.next_serial());
        }
        if let Some(lock) = self.lock.as_mut() {
            lock.surfaces.retain(|(s, o)| s.alive() && *o != output);
            lock.surfaces.push((surface, output));
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
    fn client_lock_surfaces_fill_their_own_display() {
        assert_eq!(surface_size(Size::from((1536, 960))), (1536, 960));
        assert_eq!(surface_size(Size::from((2560, 1440))), (2560, 1440));
        assert_eq!(surface_size(Size::from((0, 0))), (1, 1), "never 0, which the protocol forbids");
    }

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
    fn only_the_current_verify_counts_and_never_as_the_machine_sleeps() {
        let mut lock = Lock::new();
        assert!(finger_counts(&lock, 4, 4, true), "the current verify");
        assert!(!finger_counts(&lock, 3, 4, true), "a stopped verify's late match, or the last lock's");
        assert!(!finger_counts(&lock, 4, 4, false), "the session is in the background");
        lock.sleep_pending = true;
        assert!(!finger_counts(&lock, 4, 4, true), "logind is about to sleep");
        lock.sleep_pending = false;
        lock.client = true;
        assert!(!finger_counts(&lock, 4, 4, true), "a locker client checks its own way");
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
            let l = layout(size, &[]);
            let mid = |r: Rect| r.loc.x + r.size.w / 2;
            assert!([l.date, l.clock, l.avatar, l.name, l.field].iter().all(|r| (mid(*r) - size.w / 2).abs() <= 1), "{size:?}");
            assert!(l.date.loc.y < l.clock.loc.y && l.clock.loc.y + 100 <= l.missed.loc.y && l.missed.loc.y < l.avatar.loc.y);
            assert!(l.avatar.loc.y + AVATAR < l.name.loc.y && l.name.loc.y < l.field.loc.y && l.field.loc.y < l.hint.loc.y);
            assert!(l.hint.loc.y + l.hint.size.h < l.buttons[0].loc.y, "{size:?}: the user block clears the buttons");
            assert!(l.buttons[0].loc.x + l.buttons[0].size.w < l.buttons[1].loc.x);
            assert!(l.buttons.iter().all(|b| b.loc.y + b.size.h <= size.h - EDGE));
            assert_eq!(l.field.size, (FIELD_W, FIELD_H).into());
            let media = l.media.unwrap();
            assert!((mid(media) - size.w / 2).abs() <= 1 && media.loc.y > l.message.loc.y + 15, "{size:?}");
            assert!(media.loc.y + MEDIA_H < l.avatar.loc.y, "{size:?}: the card sits above the user block");
            assert!(l.layouts.loc.x > l.field.loc.x + FIELD_W && l.layouts.loc.x + PILL_W <= size.w && l.layouts.loc.y == l.field.loc.y);
        }
        assert_eq!(layout(Size::from((800, 400)), &[]).media, None, "no room under the clock: no card");
    }

    #[test]
    fn listed_only_when_allowed_and_body_only_with_previews_always() {
        let n = Notification { app: "Mail".into(), summary: "Lunch?".into(), body: "Friday at noon".into(), ..Default::default() };
        let l = listed(&n, true, Previews::Always).unwrap();
        assert_eq!((l.app.as_str(), l.title.as_str(), l.body.as_str()), ("Mail", "Lunch?", "Friday at noon"));
        assert_eq!(listed(&n, true, Previews::Unlocked).unwrap().body, "", "the title only");
        assert_eq!(listed(&n, true, Previews::Never), None, "previews never: only counted");
        assert_eq!(listed(&n, false, Previews::Always), None, "the app isn't allowed: only counted");
        let mut lock = Lock::new();
        for title in ["a", "b", "c", "d"] {
            lock.arrived(listed(&Notification { summary: title.into(), ..n.clone() }, true, Previews::Never));
            lock.arrived(listed(&Notification { summary: title.into(), ..n.clone() }, true, Previews::Unlocked));
        }
        let titles: Vec<_> = lock.listed.iter().map(|l| l.title.as_str()).collect();
        assert_eq!((titles, lock.missed), (vec!["d", "c", "b"], 5), "newest first; the oldest joins the count");
    }

    #[test]
    fn listed_cards_sit_under_the_clock_as_far_as_they_fit() {
        let l = |body: &str| Listed { app: "Mail".into(), title: "Lunch?".into(), body: body.into() };
        let three = [l(""), l(""), l("")];
        for size in [Size::from((1280, 800)), Size::from((1920, 1080))] {
            let (plain, lay) = (layout(size, &[]), layout(size, &three));
            let cards: Vec<Rect> = lay.cards.into_iter().flatten().collect();
            assert_eq!(cards.len(), 3, "{size:?}");
            assert!(cards[0].loc.y >= lay.clock.loc.y + lay.clock.size.h && cards.windows(2).all(|c| c[0].loc.y + c[0].size.h < c[1].loc.y));
            assert!(cards.iter().all(|c| (c.loc.x + c.size.w / 2 - size.w / 2).abs() <= 1));
            assert!(lay.missed.loc.y >= cards[2].loc.y + cards[2].size.h && lay.avatar == plain.avatar, "{size:?}: the user block stays put");
            let tall = layout(size, &[l("Friday at noon")]).cards[0].unwrap();
            assert!(tall.size.h > cards[0].size.h, "a body adds a line");
        }
        let short = layout(Size::from((800, 600)), &three);
        assert!(short.cards.iter().flatten().count() < 3 && short.missed.loc.y + 40 <= short.avatar.loc.y, "a short screen lists fewer");
    }

    #[test]
    fn layout_grows_with_the_text_size() {
        let size = Size::from((1920, 1080));
        let (small, l) = (layout(size, &[]), crate::text::with_scale(1.5, || layout(size, &[])));
        // Line heights at 1.5: Title 2 33, Display 150, Callout 23 (22.5 rounded), Headline and Body 24.
        assert!(l.date.size.h >= 33 && l.clock.size.h >= 150 && l.missed.size.h >= 23 && l.message.size.h >= 23);
        assert!(l.date.loc.y + l.date.size.h <= l.clock.loc.y && l.clock.loc.y + l.clock.size.h <= l.missed.loc.y);
        assert!(l.name.size.h >= 24 && l.field.size.h >= 24 + 8 && l.hint.size.h >= 23 && l.buttons[0].size.h >= 23 + 8);
        assert!(l.name.loc.y + l.name.size.h <= l.field.loc.y && l.field.loc.y + l.field.size.h <= l.hint.loc.y);
        assert!(l.hint.loc.y + l.hint.size.h < l.buttons[0].loc.y && l.buttons[0].loc.x + l.buttons[0].size.w < l.buttons[1].loc.x);
        assert!(l.field.size.w > small.field.size.w && l.avatar.size == small.avatar.size && l.layouts.size.h == l.field.size.h);
        let media = l.media.unwrap();
        // Title and artist: 24 + 2 + 24 inside the card, which still clears the message and the picture.
        assert!(media.size.h >= 50 + 8 && media.size.w > small.media.unwrap().size.w && media.loc.y + media.size.h < l.avatar.loc.y);
        assert!(media.loc.y >= l.message.loc.y + l.message.size.h + 8);
        assert!(crate::shell::control::media_buttons(media).iter().all(|b| media.contains_rect(*b)));
    }

    #[test]
    fn media_buttons_hit() {
        use crate::services::media::Action;
        let card = layout(Size::from((1280, 800)), &[]).media.unwrap();
        let centre = |r: Rect| Point::from((f64::from(r.loc.x + r.size.w / 2), f64::from(r.loc.y + r.size.h / 2)));
        let buttons = crate::shell::control::media_buttons(card);
        assert_eq!(buttons.map(|b| media_hit(card, centre(b))), [Some(Action::Previous), Some(Action::PlayPause), Some(Action::Next)]);
        assert!(buttons.iter().all(|b| card.contains_rect(*b)), "inside the card");
        assert!(buttons[0].loc.x > card.loc.x + 8 + ART + 100, "room for the art and the words");
        assert_eq!(media_hit(card, centre(card) - Point::from((100.0, 0.0))), None, "the words take no click");
    }

    #[test]
    fn layout_pill_label_and_hit() {
        let mut layouts = crate::ime::Layouts { list: vec![("us".into(), "English (US)".into())], ..Default::default() };
        assert_eq!(layout_pill(&layouts), None, "one layout: no pill");
        layouts.list.push(("np".into(), "Nepali".into()));
        assert_eq!(layout_pill(&layouts).as_deref(), Some("US"));
        layouts.active = 1;
        assert_eq!(layout_pill(&layouts).as_deref(), Some("NP"));
        let l = layout(Size::from((1280, 800)), &[]);
        let inside = l.layouts.to_f64().loc + Point::from((PILL_W as f64 / 2.0, FIELD_H as f64 / 2.0));
        assert!(l.layouts.to_f64().contains(inside) && !l.field.to_f64().contains(inside));
        assert!(!l.layouts.to_f64().contains(l.field.to_f64().loc), "the field isn't the pill");
    }
}
