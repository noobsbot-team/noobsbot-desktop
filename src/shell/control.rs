//! Control Center (E2.3): what it shows, where each module sits, and what clicks and slider drags do. Drawn by
//! render/control.rs from the same layout.

use smithay::utils::{Logical, Point, Rectangle};

use super::{DROP, MARGIN};
use crate::{config::Mode, render::menubar, services::Command, shortcuts::Action, state::State, wm::Rect};

pub const WIDTH: i32 = 320;
const PAD: i32 = 10;
const GAP: i32 = 10;
const INNER: i32 = WIDTH - 2 * PAD;
const HALF: i32 = (INNER - GAP) / 2;
pub const TOGGLE: i32 = 28;
const TOP: i32 = 104;
const SLIDER_CARD: i32 = 62;
const MEDIA_CARD: i32 = 56;
const POWER_CARD: i32 = 44;

/// Everything Control Center shows, read from the services and the theme.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// On, and the network's name (or "Off"/"Not Connected"); `None` without NetworkManager.
    pub wifi: Option<(bool, String)>,
    pub bluetooth: Option<(bool, String)>,
    pub dnd: bool,
    pub dark: bool,
    /// Percent; `None` without a backlight.
    pub brightness: Option<u8>,
    /// Percent and muted; `None` without PipeWire.
    pub volume: Option<(u8, bool)>,
    /// Title, artist, playing.
    pub media: Option<(String, String, bool)>,
    /// Percent and charging.
    pub battery: Option<(u8, bool)>,
    /// The active power-profiles-daemon profile.
    pub profile: Option<String>,
}

/// A clickable or draggable part.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Control {
    Wifi,
    Bluetooth,
    Dnd,
    Dark,
    Brightness,
    Volume,
    Previous,
    PlayPause,
    Next,
    Power,
}

/// Every module's place, output-relative.
#[derive(Debug, PartialEq)]
pub struct Layout {
    pub rect: Rect,
    pub cards: Vec<Rect>,
    /// Round toggles, slider tracks, media buttons and whole-card buttons.
    pub hits: Vec<(Control, Rect)>,
    pub network: Rect,
    pub dnd: Rect,
    pub dark: Rect,
    pub display: Option<Rect>,
    pub sound: Option<Rect>,
    pub media: Rect,
    pub power: Option<Rect>,
}

impl Layout {
    pub fn hit(&self, location: Point<f64, Logical>) -> Option<Control> {
        self.hits.iter().find(|(_, r)| r.to_f64().contains(location)).map(|(c, _)| *c)
    }

    pub fn rect_of(&self, control: Control) -> Option<Rect> {
        self.hits.iter().find(|(c, _)| *c == control).map(|(_, r)| *r)
    }
}

/// The Wi-Fi or Bluetooth toggle in the network card: row 0 or 1.
pub fn toggle_in(card: Rect, row: i32) -> Rect {
    Rectangle::new((card.loc.x + 10, card.loc.y + 12 + row * 50).into(), (TOGGLE, TOGGLE).into())
}

/// The round toggle of the Do Not Disturb or Dark Mode tile: at its left, centred.
pub fn tile_toggle(card: Rect) -> Rect {
    Rectangle::new((card.loc.x + 10, card.loc.y + (card.size.h - TOGGLE) / 2).into(), (TOGGLE, TOGGLE).into())
}

/// A slider card's track: under its title.
pub fn track_in(card: Rect) -> Rect {
    Rectangle::new((card.loc.x + 12, card.loc.y + 28).into(), (card.size.w - 24, TOGGLE).into())
}

/// The Now Playing buttons, right to left from the card's edge: previous, play/pause, next.
pub fn media_buttons(card: Rect) -> [Rect; 3] {
    let at = |i: i32| Rectangle::new((card.loc.x + card.size.w - 12 - TOGGLE - (2 - i) * (TOGGLE + 8), card.loc.y + (card.size.h - TOGGLE) / 2).into(), (TOGGLE, TOGGLE).into());
    [at(0), at(1), at(2)]
}

pub fn layout(output_width: i32, status: &Status) -> Layout {
    let x = (output_width - MARGIN - WIDTH).max(MARGIN);
    let top = menubar::HEIGHT + DROP;
    let card = |dx: i32, y: i32, w: i32, h: i32| Rectangle::new((x + dx, y).into(), (w, h).into());
    let mut y = top + PAD;
    let network = card(PAD, y, HALF, TOP);
    let small = (TOP - GAP) / 2;
    let dnd = card(PAD + HALF + GAP, y, HALF, small);
    let dark = card(PAD + HALF + GAP, y + small + GAP, HALF, small);
    let mut hits = vec![(Control::Wifi, toggle_in(network, 0)), (Control::Bluetooth, toggle_in(network, 1)), (Control::Dnd, dnd), (Control::Dark, dark)];
    let mut cards = vec![network, dnd, dark];
    y += TOP + GAP;
    let mut slider = |control, present: bool, y: &mut i32| {
        present.then(|| {
            let c = card(PAD, *y, INNER, SLIDER_CARD);
            hits.push((control, track_in(c)));
            cards.push(c);
            *y += SLIDER_CARD + GAP;
            c
        })
    };
    let display = slider(Control::Brightness, status.brightness.is_some(), &mut y);
    let sound = slider(Control::Volume, status.volume.is_some(), &mut y);
    let media = card(PAD, y, INNER, MEDIA_CARD);
    cards.push(media);
    if status.media.is_some() {
        hits.extend([Control::Previous, Control::PlayPause, Control::Next].into_iter().zip(media_buttons(media)));
    }
    y += MEDIA_CARD + GAP;
    let power = (status.battery.is_some() || status.profile.is_some()).then(|| {
        let c = card(PAD, y, INNER, POWER_CARD);
        if status.profile.is_some() {
            hits.push((Control::Power, c));
        }
        cards.push(c);
        y += POWER_CARD + GAP;
        c
    });
    // The last gap becomes the bottom padding.
    Layout { rect: Rectangle::new((x, top).into(), (WIDTH, y - GAP + PAD - top).into()), cards, hits, network, dnd, dark, display, sound, media, power }
}

/// The percent a slider shows with the pointer at `x`: the knob end follows the pointer, as on macOS.
pub fn slider_value(track: Rect, x: f64) -> u8 {
    let span = f64::from(track.size.w - TOGGLE).max(1.0);
    ((x - f64::from(track.loc.x) - f64::from(TOGGLE) / 2.0) / span * 100.0).round().clamp(0.0, 100.0) as u8
}

/// The filled width of a slider at `percent`: never less than its height, so the glyph always sits on the fill.
pub fn slider_fill(track: Rect, percent: u8) -> i32 {
    TOGGLE + (track.size.w - TOGGLE) * i32::from(percent.min(100)) / 100
}

/// Power-profiles-daemon's names, as Control Center shows them.
pub fn profile_name(profile: &str) -> &str {
    match profile {
        "power-saver" => "Power Saver",
        "balanced" => "Balanced",
        "performance" => "Performance",
        other => other,
    }
}

impl State {
    pub fn control_status(&self) -> Status {
        use crate::services::network::Link;
        let wifi = self.network.as_ref().map(|n| {
            let name = match &n.link {
                Link::Wifi { ssid, .. } => ssid.clone(),
                _ if !n.wifi_enabled => "Off".into(),
                Link::Ethernet => "Ethernet".into(),
                _ => "Not Connected".into(),
            };
            (n.wifi_enabled, name)
        });
        let bluetooth = self.bluetooth.as_ref().map(|b| {
            let name = match (b.powered, b.devices.first()) {
                (false, _) => "Off".into(),
                (true, Some(device)) => device.name.clone(),
                (true, None) => "On".into(),
            };
            (b.powered, name)
        });
        Status {
            wifi,
            bluetooth,
            dnd: self.dnd,
            dark: crate::render::is_dark(&self.config.appearance),
            brightness: self.brightness,
            volume: self.sound.as_ref().map(|s| (s.output.volume, s.output.muted)),
            media: self.media.as_ref().map(|m| (m.title.clone(), m.artist.clone(), m.playing)),
            battery: self.battery.as_ref().map(|b| (b.percent, b.charging)),
            profile: self.profiles.as_ref().map(|p| p.active.clone()),
        }
    }

    pub fn control_layout(&self) -> Layout {
        let width = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map_or(0, |g| g.size.w);
        layout(width, &self.control_status())
    }

    /// A press inside the panel. Each change shows at once; the service's echo confirms it.
    pub(super) fn control_press(&mut self, location: Point<f64, Logical>) {
        let Some(control) = self.control_layout().hit(location) else { return };
        match control {
            Control::Wifi => {
                if let Some(network) = self.network.as_mut() {
                    network.wifi_enabled = !network.wifi_enabled;
                    self.services.send(Command::Wifi(network.wifi_enabled));
                }
            }
            Control::Bluetooth => {
                if let Some(bluetooth) = self.bluetooth.as_mut() {
                    bluetooth.powered = !bluetooth.powered;
                    self.services.send(Command::Bluetooth(bluetooth.powered));
                }
            }
            // ponytail: stored for the notification server (M4), and the theme only in memory; saving both to
            // config comes with System Settings (M7).
            Control::Dnd => self.dnd = !self.dnd,
            Control::Dark => {
                let appearance = &mut self.config.appearance;
                appearance.mode = if crate::render::is_dark(appearance) { Mode::Light } else { Mode::Dark };
            }
            Control::Brightness | Control::Volume => {
                self.shell_drag = Some(control);
                self.control_drag(location);
            }
            Control::Previous => self.run_action(Action::Media(crate::services::media::Action::Previous)),
            Control::PlayPause => self.run_action(Action::Media(crate::services::media::Action::PlayPause)),
            Control::Next => self.run_action(Action::Media(crate::services::media::Action::Next)),
            Control::Power => {
                // ponytail: cycles through the profiles; the ▾ dropdown comes with module expansion.
                if let Some(profiles) = self.profiles.as_mut().filter(|p| !p.available.is_empty()) {
                    let next = profiles.available.iter().position(|p| *p == profiles.active).map_or(0, |i| (i + 1) % profiles.available.len());
                    profiles.active = profiles.available[next].clone();
                    self.services.send(Command::PowerProfile(profiles.active.clone()));
                }
            }
        }
        self.schedule_redraw();
    }

    /// The pointer at `location` while a slider is held.
    pub(super) fn control_drag(&mut self, location: Point<f64, Logical>) {
        let Some(control) = self.shell_drag else { return };
        let Some(track) = self.control_layout().rect_of(control) else { return };
        let percent = slider_value(track, location.x);
        match control {
            Control::Brightness => {
                // Never to 0: a black screen leaves no way to see what happened.
                let percent = percent.max(1);
                if self.brightness.is_some_and(|b| b != percent) {
                    self.brightness = Some(percent);
                    self.services.send(Command::Brightness(percent));
                }
            }
            Control::Volume => {
                let Some(output) = self.sound.as_mut().map(|s| &mut s.output) else { return };
                if output.volume != percent {
                    output.volume = percent;
                    self.services.send(Command::Volume { output: true, percent });
                }
                if output.muted && percent > 0 {
                    output.muted = false;
                    self.services.send(Command::Mute { output: true, muted: false });
                }
            }
            _ => return,
        }
        self.schedule_redraw();
    }

    /// The button went up: a slider drag ends.
    pub fn shell_release(&mut self) {
        self.shell_drag = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> Status {
        Status {
            wifi: Some((true, "Home".into())),
            bluetooth: Some((true, "On".into())),
            brightness: Some(50),
            volume: Some((40, false)),
            media: Some(("Song".into(), "Band".into(), true)),
            battery: Some((85, false)),
            profile: Some("balanced".into()),
            ..Status::default()
        }
    }

    #[test]
    fn modules_stack_inside_the_panel() {
        let l = layout(1280, &full());
        assert_eq!(l.rect.loc, (1280 - 8 - 320, 28).into());
        // 10 + 104 + 10 + 62 + 10 + 62 + 10 + 56 + 10 + 44 + 10
        assert_eq!(l.rect.size.h, 388);
        assert!(l.cards.iter().all(|c| l.rect.contains_rect(*c)));
        assert!(l.hits.iter().all(|(_, r)| l.rect.contains_rect(*r)));
        for (i, a) in l.cards.iter().enumerate() {
            assert!(l.cards[i + 1..].iter().all(|b| !a.overlaps(*b)), "card {i} overlaps");
        }
    }

    #[test]
    fn missing_services_drop_their_modules() {
        let l = layout(1280, &Status::default());
        assert_eq!((l.display, l.sound, l.power), (None, None, None));
        // Top row + Now Playing: 10 + 104 + 10 + 56 + 10.
        assert_eq!(l.rect.size.h, 190);
        // Nothing playing: no buttons.
        assert_eq!(l.hit(media_buttons(l.media)[1].to_f64().loc + Point::from((14.0, 14.0))), None);
    }

    #[test]
    fn hits_find_their_control() {
        let l = layout(1280, &full());
        let centre = |r: Rect| r.to_f64().loc + r.to_f64().size.downscale(2.0).to_point();
        assert_eq!(l.hit(centre(toggle_in(l.network, 1))), Some(Control::Bluetooth));
        assert_eq!(l.hit(centre(l.dark)), Some(Control::Dark));
        assert_eq!(l.hit(centre(media_buttons(l.media)[1])), Some(Control::PlayPause));
        assert_eq!(l.hit(centre(track_in(l.sound.unwrap()))), Some(Control::Volume));
    }

    #[test]
    fn slider_math() {
        let track = Rectangle::new((100, 0).into(), (128, 28).into());
        assert_eq!(slider_value(track, 100.0), 0);
        assert_eq!(slider_value(track, 114.0 + 50.0), 50);
        assert_eq!(slider_value(track, 400.0), 100);
        assert_eq!(slider_fill(track, 0), 28);
        assert_eq!(slider_fill(track, 100), 128);
        assert_eq!(profile_name("power-saver"), "Power Saver");
    }
}
