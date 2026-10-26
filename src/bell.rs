//! The system bell (E8.6, `xdg_system_bell`): `[accessibility] bell` plays the theme's bell, flashes the screen, both
//! or neither.

use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use smithay::{delegate_xdg_system_bell, reexports::wayland_server::protocol::wl_surface::WlSurface, wayland::xdg_system_bell::XdgSystemBellHandler};

use crate::{config::Bell, state::State};

/// How long the flash fades.
const FLASH: Duration = Duration::from_millis(300);
/// Rings closer together than this (a terminal's run of bells) count once.
const QUIET: Duration = Duration::from_millis(150);

/// The flash's white at `elapsed` after the ring; none once it has faded.
pub fn flash_alpha(elapsed: Duration) -> Option<f32> {
    (elapsed < FLASH).then(|| 0.4 * (1.0 - elapsed.as_secs_f32() / FLASH.as_secs_f32()))
}

/// The players' volume options at `percent` (1 … 100): canberra-gtk-play's gain in dB, pw-play's factor.
pub fn volume_args(percent: u8) -> (String, String) {
    let factor = f32::from(percent.clamp(1, 100)) / 100.0;
    (format!("--volume={:.1}", 20.0 * factor.log10()), format!("--volume={factor:.2}"))
}

/// Plays sound `name` from the sound theme, else the freedesktop one straight to PipeWire, at `volume` percent; off
/// the calling thread. Silent at 0.
pub fn play(name: &str, volume: u8) {
    // A theme name only: the config file's value goes into a path.
    if volume == 0 || name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return;
    }
    let name = name.to_owned();
    std::thread::spawn(move || {
        let (db, factor) = volume_args(volume);
        let quiet = |c: &mut Command| c.stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
        if !quiet(Command::new("canberra-gtk-play").args(["-i", &name, &db])) {
            quiet(Command::new("pw-play").args([&factor, &format!("/usr/share/sounds/freedesktop/stereo/{name}.oga")]));
        }
    });
}

impl State {
    pub fn ring_bell(&mut self) {
        let now = Instant::now();
        if self.bell.is_some_and(|t| now.saturating_duration_since(t) < QUIET) {
            return;
        }
        let mode = self.config.accessibility.bell;
        self.bell = Some(now);
        if matches!(mode, Bell::Flash | Bell::Both) {
            self.schedule_redraw();
        }
        if matches!(mode, Bell::Sound | Bell::Both) {
            play(&self.config.sound.alert, self.config.sound.alert_volume);
        }
    }

    /// A user interface sound (`screen-capture`, `trash-empty`), unless `[sound] ui_sounds` is off.
    pub fn ui_sound(&self, name: &str) {
        if self.config.sound.ui_sounds {
            play(name, 100);
        }
    }

    /// This frame's flash, if the bell rang a moment ago.
    pub fn bell_flash(&self) -> Option<f32> {
        matches!(self.config.accessibility.bell, Bell::Flash | Bell::Both).then(|| flash_alpha(self.bell?.elapsed())).flatten()
    }
}

impl XdgSystemBellHandler for State {
    fn ring(&mut self, _surface: Option<WlSurface>) {
        self.ring_bell();
    }
}

delegate_xdg_system_bell!(State);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flash_fades_out() {
        assert_eq!(flash_alpha(Duration::ZERO), Some(0.4));
        assert!(flash_alpha(Duration::from_millis(150)).is_some_and(|a| (a - 0.2).abs() < 1e-6));
        assert_eq!(flash_alpha(FLASH), None);
    }

    #[test]
    fn volume_options() {
        assert_eq!(volume_args(100), ("--volume=0.0".into(), "--volume=1.00".into()));
        assert_eq!(volume_args(50), ("--volume=-6.0".into(), "--volume=0.50".into()));
        assert_eq!(volume_args(10), ("--volume=-20.0".into(), "--volume=0.10".into()));
        assert_eq!(volume_args(200).1, "--volume=1.00");
    }
}
