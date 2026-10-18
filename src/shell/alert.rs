//! Power alerts (E2.2, D8): Restart / Shut Down / Log Out ask first and go ahead by themselves after 60 s, as on
//! macOS. Drawn by render/alert.rs, which keeps the button rects of the last frame.

use std::time::{Duration, Instant};

use smithay::{
    reexports::calloop::timer::{TimeoutAction, Timer},
    utils::{Logical, Point},
};

use crate::{services::Command, state::State};

const COUNTDOWN: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Power {
    Restart,
    ShutDown,
    LogOut,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Alert {
    pub power: Power,
    pub deadline: Instant,
}

impl Alert {
    /// Whole seconds left, rounded up, so it shows 60 first and never 0 before it acts.
    pub fn seconds_left(&self, now: Instant) -> u64 {
        self.deadline.saturating_duration_since(now).as_millis().div_ceil(1000) as u64
    }

    pub fn title(&self) -> &'static str {
        match self.power {
            Power::Restart => "Are you sure you want to restart your computer now?",
            Power::ShutDown => "Are you sure you want to shut down your computer now?",
            Power::LogOut => "Are you sure you want to quit all applications and log out now?",
        }
    }

    pub fn body(&self, now: Instant) -> String {
        let n = self.seconds_left(now);
        match self.power {
            Power::Restart => format!("If you do nothing, the computer will restart automatically in {n} seconds."),
            Power::ShutDown => format!("If you do nothing, the computer will shut down automatically in {n} seconds."),
            Power::LogOut => format!("If you do nothing, you will be logged out automatically in {n} seconds."),
        }
    }

    pub fn confirm(&self) -> &'static str {
        match self.power {
            Power::Restart => "Restart",
            Power::ShutDown => "Shut Down",
            Power::LogOut => "Log Out",
        }
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
        self.close_alert();
        self.popup = None;
        self.alert = Some(Alert { power, deadline: Instant::now() + COUNTDOWN });
        let tick = self.loop_handle.insert_source(Timer::from_duration(Duration::from_secs(1)), |_, _, state| {
            let Some(alert) = state.alert else { return TimeoutAction::Drop };
            state.schedule_redraw();
            if alert.seconds_left(Instant::now()) == 0 {
                state.alert_timer = None;
                state.alert_answer(Button::Confirm);
                return TimeoutAction::Drop;
            }
            TimeoutAction::ToDuration(Duration::from_secs(1))
        });
        self.alert_timer = tick.inspect_err(|err| tracing::warn!("alert timer: {err}")).ok();
        self.schedule_redraw();
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
        let Some(alert) = self.alert else { return };
        self.close_alert();
        if button == Button::Cancel {
            return;
        }
        match (alert.power, self.nested) {
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
        let alert = Alert { power: Power::Restart, deadline: now + COUNTDOWN };
        assert_eq!(alert.seconds_left(now), 60);
        assert_eq!(alert.seconds_left(now + Duration::from_millis(59_500)), 1);
        assert_eq!(alert.seconds_left(now + COUNTDOWN), 0);
        assert!(alert.body(now).ends_with("in 60 seconds."));
        assert_eq!(Alert { power: Power::LogOut, deadline: now }.confirm(), "Log Out");
    }
}
