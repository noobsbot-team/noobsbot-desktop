//! Alerts (D8). Power (E2.2): Restart / Shut Down / Log Out ask first and go ahead by themselves after 60 s, as on
//! macOS. Pairing (E2.3): a Bluetooth device asks to pair, through our BlueZ agent. A frozen app asks
//! Wait / Force Quit (E4.7).
//! Drawn by render/alert.rs, which keeps the button rects of the last frame.

use std::time::{Duration, Instant};

use smithay::{
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_server::backend::ClientId,
    },
    utils::{Logical, Point},
};

use crate::{
    services::{Command, bluetooth::Pairing},
    state::State,
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
    Pair(Pairing),
    /// A frozen app's window was closed (E4.7): Wait / Force Quit.
    NotResponding { app: String, pid: i32, client: ClientId },
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
            Kind::Power(Power::ShutDown) => "Are you sure you want to shut down your computer now?".into(),
            Kind::Power(Power::LogOut) => "Are you sure you want to quit all applications and log out now?".into(),
            Kind::Pair(p) if p.answer => format!("Pair with “{}”?", p.device),
            Kind::Pair(p) => format!("Pair with “{}”", p.device),
            Kind::NotResponding { app, .. } => format!("{app} is not responding."),
        }
    }

    pub fn body(&self, now: Instant) -> String {
        let n = self.seconds_left(now).unwrap_or(0);
        match &self.kind {
            Kind::Power(Power::Restart) => format!("If you do nothing, the computer will restart automatically in {n} seconds."),
            Kind::Power(Power::ShutDown) => format!("If you do nothing, the computer will shut down automatically in {n} seconds."),
            Kind::Power(Power::LogOut) => format!("If you do nothing, you will be logged out automatically in {n} seconds."),
            Kind::Pair(Pairing { device, code: Some(code), answer: true }) => format!("Make sure “{device}” shows the code {code}."),
            Kind::Pair(Pairing { device, code: None, .. }) => format!("“{device}” wants to pair with this computer."),
            Kind::Pair(Pairing { device, code: Some(code), answer: false }) => format!("Type {code} on “{device}”, then press Return."),
            Kind::NotResponding { .. } => "You can wait to see if it responds, or force it to quit and lose any unsaved changes.".into(),
        }
    }

    pub fn cancel(&self) -> &'static str {
        match &self.kind {
            Kind::NotResponding { .. } => "Wait",
            _ => "Cancel",
        }
    }

    pub fn confirm(&self) -> &'static str {
        match &self.kind {
            Kind::Power(Power::Restart) => "Restart",
            Kind::Power(Power::ShutDown) => "Shut Down",
            Kind::Power(Power::LogOut) => "Log Out",
            Kind::Pair(p) if p.answer => "Pair",
            Kind::Pair(_) => "OK",
            Kind::NotResponding { .. } => "Force Quit",
        }
    }

    /// `false`: one button only (a code to type on the device needs no answer).
    pub fn cancellable(&self) -> bool {
        !matches!(&self.kind, Kind::Pair(Pairing { answer: false, .. }))
    }
}

/// Which alert button a click hit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Button {
    Cancel,
    Confirm,
}

impl State {
    /// Asks before `power`; the countdown redraws once a second while the alert is up.
    pub fn open_alert(&mut self, power: Power) {
        self.open_ask(Kind::Power(power));
    }

    pub fn open_ask(&mut self, kind: Kind) {
        self.close_alert();
        self.popup = None;
        let power = matches!(kind, Kind::Power(_));
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

    /// BlueZ asks (`Some`) or gives up (`None`, which closes only a pairing alert).
    pub fn show_pairing(&mut self, pairing: Option<Pairing>) {
        match pairing {
            Some(pairing) => {
                self.close_alert();
                self.popup = None;
                self.alert = Some(Alert { kind: Kind::Pair(pairing), deadline: None });
                self.schedule_redraw();
            }
            None if matches!(&self.alert, Some(Alert { kind: Kind::Pair(_), .. })) => self.close_alert(),
            None => {}
        }
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
        self.close_alert();
        let power = match alert.kind {
            Kind::Pair(pairing) => {
                if pairing.answer {
                    self.services.send(Command::PairingAnswer(button == Button::Confirm));
                }
                return;
            }
            Kind::Power(_) if button == Button::Cancel => return,
            Kind::Power(power) => power,
            Kind::NotResponding { .. } if button == Button::Cancel => return,
            Kind::NotResponding { pid, client, .. } => return self.kill(pid, client),
        };
        match (power, self.nested) {
            (Power::LogOut, _) => self.loop_signal.stop(),
            // A nested session is a window on another desktop: never restart or power off the machine under it.
            (power, true) => tracing::warn!("{power:?}: not done in a nested session"),
            (Power::Restart, false) => self.services.send(Command::Reboot),
            (Power::ShutDown, false) => self.services.send(Command::PowerOff),
        }
    }

    /// A press at `location` (output-relative): `true` if the alert took it. Outside, the click goes on (the alert
    /// isn't modal, as on macOS).
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
    }

    #[test]
    fn pairing_text() {
        let pair = |code: Option<&str>, answer| Alert { kind: Kind::Pair(Pairing { device: "Keyboard".into(), code: code.map(Into::into), answer }), deadline: None };
        let now = Instant::now();
        assert_eq!(pair(Some("012345"), true).body(now), "Make sure “Keyboard” shows the code 012345.");
        assert_eq!(pair(Some("012345"), false).body(now), "Type 012345 on “Keyboard”, then press Return.");
        assert_eq!(pair(None, true).title(), "Pair with “Keyboard”?");
        assert!(!pair(Some("1"), false).cancellable() && pair(None, true).cancellable());
        assert_eq!(pair(None, true).seconds_left(now), None);
    }
}
