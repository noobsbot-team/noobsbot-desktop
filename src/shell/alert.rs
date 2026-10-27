//! Alerts (D8). Power (E2.2): Restart / Shut Down / Log Out ask first and go ahead by themselves after 60 s; the power
//! button (E6.5) asks Shut Down with Sleep and Restart beside it.
//! Pairing (E2.3): a Bluetooth device asks to pair, through our BlueZ agent. A frozen app asks
//! Wait / Force Quit (E4.7).
//! Drawn by render/alert.rs, which keeps the button rects of the last frame.

use std::time::{Duration, Instant};

use smithay::{
    input::keyboard::Keysym,
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_server::backend::ClientId,
    },
    utils::{Logical, Point},
};

use crate::{
    services::{
        Command,
        bluetooth::Pairing,
        bolt::Accessory,
        network::Secret,
        polkit::{Auth, Event},
    },
    state::State,
    ui::field::{Edit, TextField},
};

const COUNTDOWN: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Power {
    Restart,
    ShutDown,
    LogOut,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Power(Power),
    /// The power button (E6.5): Shut Down's alert with Sleep and Restart buttons too.
    PowerButton,
    Pair(Pairing),
    /// A frozen app's window was closed (E4.7): Wait / Force Quit.
    NotResponding {
        app: String,
        pid: i32,
        client: ClientId,
    },
    /// A Wi-Fi password (E2.3): `join` has the access point and device paths when we join, `None` when
    /// NetworkManager asked (its secret agent); `field` is the password typed so far; `secret` what it is.
    Wifi {
        ssid: String,
        join: Option<(String, String)>,
        field: TextField,
        secret: Secret,
    },
    /// An old Bluetooth device needs a PIN (or six-digit passkey) typed (E2.3).
    Pin {
        device: String,
        passkey: bool,
        field: TextField,
    },
    /// An app asks to capture the screen (E6.11): `exe` is its program, remembered in `[privacy]`.
    ScreenRecording {
        app: String,
        exe: String,
        client: ClientId,
    },
    /// A Thunderbolt device waits for boltd's authorization (E8.8): Don't Allow / Allow.
    Thunderbolt(Accessory),
    /// The Trash's Empty Trash (E8.8).
    EmptyTrash,
    /// An admin password for polkit (E7.2): `user` indexes `auth.users` (Tab picks the next); `checking` while
    /// polkit's helper has it; `wrong` since the last wrong one (the field shakes, the body says so).
    Polkit {
        auth: Auth,
        user: usize,
        field: TextField,
        checking: bool,
        wrong: Option<Instant>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Alert {
    pub kind: Kind,
    /// When a power alert acts by itself.
    pub deadline: Option<Instant>,
}

impl Alert {
    pub fn power(power: Power, now: Instant) -> Self {
        Self { kind: Kind::Power(power), deadline: Some(now + COUNTDOWN) }
    }

    /// Whole seconds left, rounded up, so it shows 60 first and never 0 before it acts.
    pub fn seconds_left(&self, now: Instant) -> Option<u64> {
        self.deadline.map(|d| d.saturating_duration_since(now).as_millis().div_ceil(1000) as u64)
    }

    pub fn title(&self) -> String {
        match &self.kind {
            Kind::Power(Power::Restart) => "Are you sure you want to restart your computer now?".into(),
            Kind::Power(Power::ShutDown) | Kind::PowerButton => "Are you sure you want to shut down your computer now?".into(),
            Kind::Power(Power::LogOut) => "Are you sure you want to quit all applications and log out now?".into(),
            Kind::Pair(p) if p.answer => format!("Pair with “{}”?", p.device),
            Kind::Pair(p) => format!("Pair with “{}”", p.device),
            Kind::NotResponding { app, .. } => format!("{app} is not responding."),
            Kind::Wifi { ssid, .. } => format!("Join “{ssid}”?"),
            Kind::Pin { device, .. } => format!("Pair with “{device}”"),
            Kind::ScreenRecording { app, .. } => format!("“{app}” would like to record this computer's screen."),
            Kind::Thunderbolt(a) => format!("Allow “{}” to connect?", a.name),
            Kind::Polkit { auth, .. } => auth.message.clone(),
            Kind::EmptyTrash => "Are you sure you want to permanently erase the items in the Trash?".into(),
        }
    }

    pub fn body(&self, now: Instant) -> String {
        let n = self.seconds_left(now).unwrap_or(0);
        match &self.kind {
            Kind::Power(Power::Restart) => format!("If you do nothing, the computer will restart automatically in {n} seconds."),
            Kind::Power(Power::ShutDown) | Kind::PowerButton => format!("If you do nothing, the computer will shut down automatically in {n} seconds."),
            Kind::Power(Power::LogOut) => format!("If you do nothing, you will be logged out automatically in {n} seconds."),
            Kind::Pair(Pairing { device, code: Some(code), answer: true }) => format!("Make sure “{device}” shows the code {code}."),
            Kind::Pair(Pairing { device, code: None, .. }) => format!("“{device}” wants to pair with this computer."),
            Kind::Pair(Pairing { device, code: Some(code), answer: false }) => format!("Type {code} on “{device}”, then press Return."),
            Kind::Wifi { secret: Secret::Psk, .. } => "Enter the password for this Wi-Fi network.".into(),
            Kind::Wifi { secret: Secret::Wep, .. } => "Enter the WEP key or passphrase for this Wi-Fi network.".into(),
            Kind::Wifi { secret: Secret::Identity, .. } => "Enter your user name for this network.".into(),
            Kind::Wifi { secret: Secret::Password, .. } => "Enter the password for your account on this network.".into(),
            Kind::Pin { passkey: false, .. } => "Enter the PIN shown on the device or in its manual.".into(),
            Kind::Pin { passkey: true, .. } => "Enter the six-digit passkey shown on the device.".into(),
            Kind::NotResponding { .. } => "You can wait to see if it responds, or force it to quit and lose any unsaved changes.".into(),
            Kind::ScreenRecording { .. } => "It will see everything on the screen, including other apps' windows. The answer is kept in config.toml.".into(),
            Kind::Thunderbolt(_) => "Allow only accessories you trust. Once allowed, it connects by itself next time.".into(),
            Kind::EmptyTrash => "You can't undo this action.".into(),
            Kind::Polkit { wrong: Some(_), .. } => "The password is incorrect. Try again.".into(),
            Kind::Polkit { auth, user, .. } => {
                let name = auth.users.get(*user).map_or("", String::as_str);
                let other = if auth.users.len() > 1 { " Press Tab for another administrator." } else { "" };
                format!("Enter the password for “{name}” to allow this.{other}")
            }
        }
    }

    pub fn cancel(&self) -> &'static str {
        match &self.kind {
            Kind::NotResponding { .. } => "Wait",
            Kind::ScreenRecording { .. } | Kind::Thunderbolt(_) => "Don't Allow",
            _ => "Cancel",
        }
    }

    pub fn confirm(&self) -> &'static str {
        match &self.kind {
            Kind::Power(Power::Restart) => "Restart",
            Kind::Power(Power::ShutDown) | Kind::PowerButton => "Shut Down",
            Kind::Power(Power::LogOut) => "Log Out",
            Kind::Pair(p) if p.answer => "Pair",
            Kind::Pair(_) => "OK",
            Kind::NotResponding { .. } => "Force Quit",
            Kind::Wifi { .. } => "Join",
            Kind::Pin { .. } => "Pair",
            Kind::ScreenRecording { .. } | Kind::Thunderbolt(_) => "Allow",
            Kind::EmptyTrash => "Empty Trash",
            Kind::Polkit { .. } => "OK",
        }
    }

    /// Buttons left of Cancel and the default one, in order.
    pub fn extra(&self) -> &'static [(&'static str, Button)] {
        match &self.kind {
            Kind::PowerButton => &[("Sleep", Button::Sleep), ("Restart", Button::Restart)],
            _ => &[],
        }
    }

    /// The text field the alert types into, if it has one.
    pub fn field(&self) -> Option<&TextField> {
        match &self.kind {
            Kind::Wifi { field, .. } | Kind::Pin { field, .. } | Kind::Polkit { field, .. } => Some(field),
            _ => None,
        }
    }

    /// `false`: one button only (a code to type on the device needs no answer).
    pub fn cancellable(&self) -> bool {
        !matches!(&self.kind, Kind::Pair(Pairing { answer: false, .. }))
    }
}

impl Alert {
    /// The field's sideways offset after a wrong password; `None` once it has settled.
    pub fn shake(&self) -> Option<i32> {
        let Kind::Polkit { wrong: Some(at), .. } = &self.kind else { return None };
        let t = at.elapsed();
        (t < crate::shell::lock::SHAKE).then(|| crate::shell::lock::shake_offset(t))
    }
}

/// Which alert button a click hit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Button {
    Cancel,
    Confirm,
    /// A power alert's "Reopen windows when logging back in" row: toggles it.
    Reopen,
    /// The power button's alert: sleep, or restart without asking again.
    Sleep,
    Restart,
}

impl State {
    /// Asks before `power`; the countdown redraws once a second while the alert is up.
    pub fn open_alert(&mut self, power: Power) {
        self.open_ask(Kind::Power(power));
    }

    pub fn open_ask(&mut self, kind: Kind) {
        // Another alert over a polkit one: its request must not wait forever.
        if matches!(&self.alert, Some(Alert { kind: Kind::Polkit { .. }, .. })) && !matches!(kind, Kind::Polkit { .. }) {
            self.services.send(Command::PolkitAnswer(None));
        }
        self.close_alert();
        self.popup = None;
        let power = matches!(kind, Kind::Power(_) | Kind::PowerButton);
        self.alert = Some(Alert { kind, deadline: power.then(|| Instant::now() + COUNTDOWN) });
        self.schedule_redraw();
        if !power {
            return;
        }
        let tick = self.loop_handle.insert_source(Timer::from_duration(Duration::from_secs(1)), |_, _, state| {
            let Some(left) = state.alert.as_ref().and_then(|a| a.seconds_left(Instant::now())) else { return TimeoutAction::Drop };
            state.schedule_redraw();
            if left == 0 {
                state.alert_timer = None;
                state.alert_answer(Button::Confirm);
                return TimeoutAction::Drop;
            }
            TimeoutAction::ToDuration(Duration::from_secs(1))
        });
        self.alert_timer = tick.inspect_err(|err| tracing::warn!("alert timer: {err}")).ok();
    }

    /// A key while the alert is up: `true` if it took it. An alert with a field takes every key, so a password
    /// never reaches the focused app; without one, only Esc and Return.
    pub fn alert_key(&mut self, keysym: Keysym, ctrl: bool) -> bool {
        let Some(alert) = self.alert.as_mut() else { return false };
        let edit = match &mut alert.kind {
            Kind::Polkit { checking: true, .. } => return true,
            Kind::Polkit { auth, user, wrong, .. } if keysym == Keysym::Tab => {
                (*user, *wrong) = ((*user + 1) % auth.users.len().max(1), None);
                Edit::Changed
            }
            Kind::Polkit { field, wrong, .. } => {
                let edit = field.key(keysym, keysym.key_char().map(String::from).as_deref(), ctrl);
                if edit == Edit::Changed {
                    *wrong = None;
                }
                edit
            }
            Kind::Wifi { field, .. } | Kind::Pin { field, .. } => field.key(keysym, keysym.key_char().map(String::from).as_deref(), ctrl),
            _ => match keysym {
                Keysym::Escape => Edit::Cancel,
                Keysym::Return | Keysym::KP_Enter => Edit::Submit,
                _ => return false,
            },
        };
        match edit {
            Edit::Submit => self.alert_answer(Button::Confirm),
            Edit::Cancel => self.alert_answer(Button::Cancel),
            Edit::Changed | Edit::Moved => self.schedule_redraw(),
            Edit::Ignored => {}
        }
        true
    }

    /// NetworkManager asks for `ssid`'s password, key or user name (`Some`) or gives up (`None`, which closes only its
    /// alert).
    pub fn show_wifi_secret(&mut self, ask: Option<(String, Secret)>) {
        match ask {
            Some((ssid, secret)) => {
                let field = if secret == Secret::Identity { TextField::default() } else { TextField::secure() };
                self.open_ask(Kind::Wifi { ssid, join: None, field, secret });
            }
            None if matches!(&self.alert, Some(Alert { kind: Kind::Wifi { join: None, .. }, .. })) => self.close_alert(),
            None => {}
        }
    }

    /// Our polkit agent: ask, shake after a wrong password, or close (only the alert with that cookie).
    pub fn polkit_event(&mut self, event: Event) {
        let ours = |alert: &Option<Alert>, cookie: &str| matches!(alert, Some(Alert { kind: Kind::Polkit { auth, .. }, .. }) if auth.cookie == cookie);
        match event {
            Event::Ask(auth) => self.open_ask(Kind::Polkit { auth, user: 0, field: TextField::secure(), checking: false, wrong: None }),
            Event::Done(cookie) if ours(&self.alert, &cookie) => self.close_alert(),
            Event::Wrong(cookie) if ours(&self.alert, &cookie) => {
                if let Some(Alert { kind: Kind::Polkit { field, checking, wrong, .. }, .. }) = self.alert.as_mut() {
                    crate::shell::lock::wipe(field);
                    (*checking, *wrong) = (false, Some(Instant::now()));
                }
                // Redraw every frame while the field shakes.
                let shaking = |_, _: &mut (), state: &mut State| {
                    state.schedule_redraw();
                    match state.alert.as_ref().and_then(Alert::shake) {
                        Some(_) => TimeoutAction::ToDuration(Duration::from_millis(16)),
                        None => TimeoutAction::Drop,
                    }
                };
                self.loop_handle.insert_source(Timer::from_duration(Duration::ZERO), shaking).map_or_else(|err| tracing::warn!("polkit alert: {err}"), drop);
            }
            Event::Done(_) | Event::Wrong(_) => {}
        }
    }

    /// BlueZ wants a PIN or passkey typed for `device`.
    pub fn show_pin_entry(&mut self, device: String, passkey: bool) {
        self.open_ask(Kind::Pin { device, passkey, field: TextField::default() });
    }

    /// BlueZ asks (`Some`) or gives up (`None`, which closes only a pairing alert).
    pub fn show_pairing(&mut self, pairing: Option<Pairing>) {
        match pairing {
            Some(pairing) => {
                self.close_alert();
                self.popup = None;
                self.alert = Some(Alert { kind: Kind::Pair(pairing), deadline: None });
                self.schedule_redraw();
            }
            None if matches!(&self.alert, Some(Alert { kind: Kind::Pair(_) | Kind::Pin { .. }, .. })) => self.close_alert(),
            None => {}
        }
    }

    /// boltd's waiting devices changed: keep which were asked, close the alert of one that went, ask the next.
    pub fn thunderbolt_changed(&mut self, waiting: Vec<Accessory>) {
        let before = std::mem::take(&mut self.thunderbolt);
        let asked = |uid: &str| before.iter().any(|(a, asked)| *asked && a.uid == uid);
        self.thunderbolt = waiting.into_iter().map(|a| (a.clone(), asked(&a.uid))).collect();
        if let Some(Alert { kind: Kind::Thunderbolt(shown), .. }) = &self.alert
            && !self.thunderbolt.iter().any(|(a, _)| a.uid == shown.uid)
        {
            self.close_alert();
        }
        self.ask_thunderbolt();
    }

    /// Asks about the next waiting Thunderbolt device, unless locked (unlocking asks) or one is already up.
    // ponytail: another alert opening over it drops that question; the device stays unauthorized until replugged.
    pub fn ask_thunderbolt(&mut self) {
        if self.lock.is_some() || matches!(&self.alert, Some(Alert { kind: Kind::Thunderbolt(_), .. })) {
            return;
        }
        let Some((accessory, asked)) = self.thunderbolt.iter_mut().find(|(_, asked)| !*asked) else { return };
        *asked = true;
        let kind = Kind::Thunderbolt(accessory.clone());
        self.open_ask(kind);
    }

    fn close_alert(&mut self) {
        if let Some(token) = self.alert_timer.take() {
            self.loop_handle.remove(token);
        }
        if self.alert.take().is_some() {
            self.schedule_redraw();
        }
    }

    /// Cancel (Esc) or the default button (Return, or the countdown running out).
    pub fn alert_answer(&mut self, button: Button) {
        let Some(alert) = self.alert.clone() else { return };
        if button == Button::Reopen {
            return self.toggle_reopen();
        }
        if let Kind::Polkit { auth, user, field, checking, .. } = &alert.kind {
            if button == Button::Confirm {
                if !*checking && !field.text.is_empty() {
                    let user = auth.users.get(*user).cloned().unwrap_or_default();
                    self.services.send(Command::PolkitAnswer(Some((user, field.text.clone()))));
                    if let Some(Alert { kind: Kind::Polkit { checking, .. }, .. }) = self.alert.as_mut() {
                        *checking = true;
                    }
                }
                return;
            }
            self.services.send(Command::PolkitAnswer(None));
        }
        if button == Button::Confirm && matches!(&alert.kind, Kind::Wifi { field, secret, .. } if field.text.chars().count() < secret.shortest()) {
            return;
        }
        self.close_alert();
        let power = match alert.kind {
            Kind::Pair(pairing) => {
                if pairing.answer {
                    self.services.send(Command::PairingAnswer(button == Button::Confirm));
                }
                return;
            }
            Kind::Power(_) | Kind::PowerButton if button == Button::Cancel => return,
            Kind::PowerButton if button == Button::Sleep => return self.sleep(),
            Kind::Power(power) => {
                self.session_save(self.config.windows.reopen_at_login);
                power
            }
            Kind::PowerButton => {
                self.session_save(self.config.windows.reopen_at_login);
                if button == Button::Restart { Power::Restart } else { Power::ShutDown }
            }
            Kind::NotResponding { .. } if button == Button::Cancel => return,
            Kind::NotResponding { pid, client, .. } => return self.kill(pid, client),
            Kind::Pin { field, .. } => return self.services.send(Command::PinEntry((button == Button::Confirm).then_some(field.text))),
            Kind::Wifi { join: None, field, .. } => return self.services.send(Command::WifiSecret((button == Button::Confirm).then_some(field.text))),
            Kind::Wifi { .. } if button == Button::Cancel => return,
            Kind::Wifi { ssid, join: Some((ap, device)), field, .. } => {
                return self.services.send(Command::WifiJoin { ssid, ap, device, password: Some(field.text) });
            }
            Kind::ScreenRecording { exe, client, .. } => return self.recording_answer(client, exe, button == Button::Confirm),
            Kind::Thunderbolt(accessory) => {
                if button == Button::Confirm {
                    self.services.send(Command::Thunderbolt(accessory.uid));
                }
                return self.ask_thunderbolt();
            }
            Kind::EmptyTrash if button == Button::Cancel => return,
            Kind::EmptyTrash => {
                // Off the frame thread: a big trash takes a while; the watcher turns the icon empty.
                let _ = std::thread::Builder::new().name("empty-trash".into()).spawn(crate::shell::trash::empty_all);
                self.ui_sound("trash-empty");
                return;
            }
            Kind::Polkit { .. } => return,
        };
        self.power_now(power);
    }

    /// The checkbox: remembered in `[windows] reopen_at_login`.
    fn toggle_reopen(&mut self) {
        let on = !self.config.windows.reopen_at_login;
        self.config.windows.reopen_at_login = on;
        let path = crate::config::path();
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if let Err(err) = crate::config::save_atomic(&path, &crate::config::set_key(&text, "windows", "reopen_at_login", &on.to_string())) {
            tracing::warn!("{}: {err}", path.display());
        }
        self.schedule_redraw();
    }

    /// Restart, shut down or log out without asking (the alert asked, or the lock screen's buttons).
    pub fn power_now(&mut self, power: Power) {
        match (power, self.nested) {
            (Power::LogOut, _) => self.loop_signal.stop(),
            // A nested session is a window on another desktop: never restart or power off the machine under it.
            (power, true) => tracing::warn!("{power:?}: not done in a nested session"),
            (Power::Restart, false) => self.services.send(Command::Reboot),
            (Power::ShutDown, false) => self.services.send(Command::PowerOff),
        }
    }

    /// A press at `location` (output-relative): `true` if the alert took it. Outside, the click goes on (the alert
    /// isn't modal).
    pub(super) fn alert_press(&mut self, location: Point<f64, Logical>) -> bool {
        let Some(backend) = self.backend.as_ref() else { return false };
        if !backend.alert.rect().is_some_and(|r| r.to_f64().contains(location)) {
            return false;
        }
        if let Some(button) = backend.alert.button_at(location) {
            self.alert_answer(button);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countdown_text() {
        let now = Instant::now();
        let alert = Alert::power(Power::Restart, now);
        assert_eq!(alert.seconds_left(now), Some(60));
        assert_eq!(alert.seconds_left(now + Duration::from_millis(59_500)), Some(1));
        assert_eq!(alert.seconds_left(now + COUNTDOWN), Some(0));
        assert!(alert.body(now).ends_with("in 60 seconds."));
        assert_eq!(Alert::power(Power::LogOut, now).confirm(), "Log Out");
        let button = Alert { kind: Kind::PowerButton, deadline: None };
        assert_eq!((button.confirm(), button.cancel(), button.extra().len()), ("Shut Down", "Cancel", 2));
        assert!(Alert::power(Power::ShutDown, now).extra().is_empty());
    }

    #[test]
    fn pairing_text() {
        let pair =
            |code: Option<&str>, answer| Alert { kind: Kind::Pair(Pairing { device: "Keyboard".into(), code: code.map(Into::into), answer }), deadline: None };
        let now = Instant::now();
        assert_eq!(pair(Some("012345"), true).body(now), "Make sure “Keyboard” shows the code 012345.");
        assert_eq!(pair(Some("012345"), false).body(now), "Type 012345 on “Keyboard”, then press Return.");
        assert_eq!(pair(None, true).title(), "Pair with “Keyboard”?");
        assert!(!pair(Some("1"), false).cancellable() && pair(None, true).cancellable());
        assert_eq!(pair(None, true).seconds_left(now), None);
    }

    #[test]
    fn wifi_text() {
        let alert = Alert { kind: Kind::Wifi { ssid: "Home".into(), join: None, field: TextField::secure(), secret: Secret::Psk }, deadline: None };
        assert_eq!((alert.title().as_str(), alert.confirm(), alert.cancel()), ("Join “Home”?", "Join", "Cancel"));
        let wep = Alert { kind: Kind::Wifi { ssid: "Home".into(), join: None, field: TextField::secure(), secret: Secret::Wep }, deadline: None };
        assert!(wep.body(Instant::now()).contains("WEP key"));
        assert!(alert.field().is_some_and(|f| f.secure) && alert.cancellable());
    }

    #[test]
    fn polkit_text() {
        let auth = Auth { cookie: "c".into(), message: "Authentication is required.".into(), users: vec!["ann".into(), "root".into()] };
        let mut alert = Alert { kind: Kind::Polkit { auth, user: 1, field: TextField::secure(), checking: false, wrong: None }, deadline: None };
        let now = Instant::now();
        assert_eq!(alert.title(), "Authentication is required.");
        assert_eq!(alert.body(now), "Enter the password for “root” to allow this. Press Tab for another administrator.");
        assert!(alert.field().is_some_and(|f| f.secure) && alert.shake().is_none());
        if let Kind::Polkit { wrong, .. } = &mut alert.kind {
            *wrong = Some(now);
        }
        assert_eq!(alert.body(now), "The password is incorrect. Try again.");
        assert!(alert.shake().is_some());
    }

    #[test]
    fn thunderbolt_text() {
        let alert = Alert { kind: Kind::Thunderbolt(Accessory { uid: "u".into(), name: "Dell WD19TB".into() }), deadline: None };
        assert_eq!((alert.title().as_str(), alert.cancel(), alert.confirm()), ("Allow “Dell WD19TB” to connect?", "Don't Allow", "Allow"));
        assert!(alert.cancellable() && alert.field().is_none());
    }
}
