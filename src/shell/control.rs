//! Control Center (E2.3): what it shows, where each module sits, and what clicks and slider drags do. Drawn by
//! render/control.rs from the same layout.

use crate::{services::network::Link, symbols::Symbol};
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
/// D8 list row.
pub const LIST_ROW: i32 = 28;
/// Above and below an expanded list.
const LIST_PAD: i32 = 6;

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
    /// Sound outputs: PipeWire id, short name, current.
    pub outputs: Vec<(u32, String, bool)>,
    /// The rows of an open Wi-Fi or Bluetooth list.
    pub rows: Vec<ListRow>,
    /// The module showing its list.
    pub expanded: Option<Module>,
}

/// A module that opens a list in place (one at a time, as on macOS).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Module {
    Wifi,
    Bluetooth,
    Sound,
}

/// A network or device in an open list.
#[derive(Debug, Clone, PartialEq)]
pub struct ListRow {
    pub symbol: Symbol,
    pub name: String,
    /// Battery %, right-aligned.
    pub detail: Option<String>,
    /// Needs a password.
    pub lock: bool,
    /// Connected: the icon's circle takes the accent.
    pub on: bool,
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
    /// The Sound module's title row: shows or hides the outputs.
    SoundMore,
    /// A sound output, by PipeWire id.
    Output(u32),
    /// The Wi-Fi label: shows or hides the networks.
    WifiMore,
    /// The Bluetooth label: shows or hides the devices.
    BluetoothMore,
    /// A row of the open list, by its index in `Status::rows`.
    Row(usize),
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
    /// The output rows of an open Sound module.
    pub outputs: Vec<Rect>,
    /// The list under the top row while Wi-Fi or Bluetooth is open, and its rows.
    pub list: Option<(Rect, Vec<Rect>)>,
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
    // Right of a toggle: its title and subtitle.
    let label_of = |t: Rect| Rectangle::new((t.loc.x + TOGGLE + 4, t.loc.y - 4).into(), (network.loc.x + network.size.w - t.loc.x - TOGGLE - 8, TOGGLE + 8).into());
    let mut hits = vec![
        (Control::Wifi, toggle_in(network, 0)),
        (Control::WifiMore, label_of(toggle_in(network, 0))),
        (Control::Bluetooth, toggle_in(network, 1)),
        (Control::BluetoothMore, label_of(toggle_in(network, 1))),
        (Control::Dnd, dnd),
        (Control::Dark, dark),
    ];
    let mut cards = vec![network, dnd, dark];
    y += TOP + GAP;
    // An open Wi-Fi or Bluetooth module lists its networks or devices full width under the top row; with none, one
    // empty row says so.
    let on = |module: &Option<(bool, String)>| module.as_ref().is_some_and(|m| m.0);
    let open = match status.expanded {
        Some(Module::Wifi) => on(&status.wifi),
        Some(Module::Bluetooth) => on(&status.bluetooth),
        _ => false,
    };
    let list = open.then(|| {
        let rows = status.rows.len().max(1) as i32;
        let list = card(PAD, y, INNER, 2 * LIST_PAD + rows * LIST_ROW);
        let cells: Vec<Rect> = (0..rows).map(|i| Rectangle::new((list.loc.x + LIST_PAD, list.loc.y + LIST_PAD + i * LIST_ROW).into(), (list.size.w - 2 * LIST_PAD, LIST_ROW).into())).collect();
        hits.extend(cells.iter().take(status.rows.len()).enumerate().map(|(i, r)| (Control::Row(i), *r)));
        cards.push(list);
        y += list.size.h + GAP;
        (list, cells)
    });
    let mut slider = |control, present: bool, extra: i32, y: &mut i32| {
        present.then(|| {
            let c = card(PAD, *y, INNER, SLIDER_CARD + extra);
            hits.push((control, track_in(c)));
            cards.push(c);
            *y += SLIDER_CARD + extra + GAP;
            c
        })
    };
    let display = slider(Control::Brightness, status.brightness.is_some(), 0, &mut y);
    let open = status.expanded == Some(Module::Sound) && !status.outputs.is_empty();
    let extra = if open { 2 * LIST_PAD + status.outputs.len() as i32 * LIST_ROW } else { 0 };
    let sound = slider(Control::Volume, status.volume.is_some(), extra, &mut y);
    let mut outputs = Vec::new();
    if let Some(c) = sound {
        hits.push((Control::SoundMore, Rectangle::new(c.loc, (c.size.w, 28).into())));
        if open {
            for (i, (id, _, _)) in status.outputs.iter().enumerate() {
                let row = Rectangle::new((c.loc.x + LIST_PAD, c.loc.y + SLIDER_CARD + LIST_PAD + i as i32 * LIST_ROW).into(), (c.size.w - 2 * LIST_PAD, LIST_ROW).into());
                hits.push((Control::Output(*id), row));
                outputs.push(row);
            }
        }
    }
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
    Layout { rect: Rectangle::new((x, top).into(), (WIDTH, y - GAP + PAD - top).into()), cards, hits, network, dnd, dark, display, sound, outputs, list, media, power }
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

/// Output names without what they all share (the sound card, "Raptor Lake-P/U/H cAVS") or PipeWire's
/// "Output (Stereo)": `HDMI / DisplayPort 2`, `Speaker`.
pub fn short_names(names: &[String]) -> Vec<String> {
    let common = match names {
        [first, rest @ ..] if !rest.is_empty() => {
            let shared = rest.iter().fold(first.len(), |n, other| first.bytes().zip(other.bytes()).take(n).take_while(|(a, b)| a == b).count());
            // Back to a word boundary, so "Speaker" and "Spdif" keep their first letters.
            first[..shared].rfind(' ').map_or(0, |i| i + 1)
        }
        _ => 0,
    };
    names
        .iter()
        .map(|name| {
            let name = name.get(common..).unwrap_or(name);
            let name = name.trim_end_matches(" (Stereo)").trim_end_matches(" (Mono)").trim_end_matches(" Output").trim();
            if name.is_empty() { "Output".to_owned() } else { name.to_owned() }
        })
        .collect()
}

/// The Wi-Fi symbol for 1–3 bars.
pub fn wifi_symbol(bars: u8) -> Symbol {
    [Symbol::WifiLow, Symbol::WifiMedium, Symbol::WifiHigh][usize::from(bars.clamp(1, 3)) - 1]
}

/// A Bluetooth device's symbol, from its name.
fn device_symbol(name: &str) -> Symbol {
    let name = name.to_lowercase();
    if ["airpods", "buds", "tws", "headphone", "headset", "wh-", "wf-"].iter().any(|k| name.contains(k)) {
        Symbol::Headphones
    } else if name.contains("keyboard") {
        Symbol::Keyboard
    } else {
        Symbol::Bluetooth
    }
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
            let name = match (b.powered, b.connected().next()) {
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
            outputs: self.sound.as_ref().map_or_else(Vec::new, |s| {
                let names = short_names(&s.outputs.iter().map(|o| o.name.clone()).collect::<Vec<_>>());
                s.outputs.iter().zip(names).map(|(o, name)| (o.id, name, o.current)).collect()
            }),
            rows: match self.expanded {
                Some(Module::Wifi) => self.network.as_ref().map_or_else(Vec::new, |n| {
                    let current = match &n.link {
                        Link::Wifi { ssid, .. } => Some(ssid.as_str()),
                        _ => None,
                    };
                    n.networks
                        .iter()
                        .map(|w| ListRow { symbol: wifi_symbol(w.bars), name: w.ssid.clone(), detail: None, lock: w.secure, on: current == Some(w.ssid.as_str()) })
                        .collect()
                }),
                Some(Module::Bluetooth) => self.bluetooth.as_ref().map_or_else(Vec::new, |b| {
                    b.devices
                        .iter()
                        .map(|d| ListRow { symbol: device_symbol(&d.name), name: d.name.clone(), detail: d.battery.map(|p| format!("{p}%")), lock: false, on: d.connected })
                        .collect()
                }),
                _ => Vec::new(),
            },
            expanded: self.expanded,
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
                if appearance.theme_apps {
                    crate::theme::write_in_background(appearance.clone());
                }
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
            // ponytail: opens at once; the in-place `smooth` spring comes with the animation module.
            Control::SoundMore => self.expanded = if self.expanded == Some(Module::Sound) { None } else { Some(Module::Sound) },
            Control::WifiMore => {
                self.expanded = if self.expanded == Some(Module::Wifi) { None } else { Some(Module::Wifi) };
                if self.expanded.is_some() {
                    self.services.send(Command::WifiScan);
                }
            }
            Control::BluetoothMore => self.expanded = if self.expanded == Some(Module::Bluetooth) { None } else { Some(Module::Bluetooth) },
            // ponytail: no "Connecting…" state; the row flips when the service reports the change.
            Control::Row(i) => match self.expanded {
                Some(Module::Bluetooth) => {
                    if let Some(device) = self.bluetooth.as_ref().and_then(|b| b.devices.get(i)) {
                        self.services.send(Command::BluetoothDevice { path: device.path.clone(), connect: !device.connected });
                    }
                }
                Some(Module::Wifi) => {
                    if let Some(network) = self.network.as_ref().and_then(|n| n.networks.get(i)) {
                        let joined = matches!(&self.network.as_ref().map(|n| &n.link), Some(Link::Wifi { ssid, .. }) if *ssid == network.ssid);
                        let (ssid, ap, device) = (network.ssid.clone(), network.ap.clone(), network.device.clone());
                        if joined {
                        } else if network.secure && !network.known {
                            let field = crate::ui::field::TextField::secure();
                            self.open_ask(crate::shell::alert::Kind::Wifi { ssid, join: Some((ap, device)), field });
                        } else {
                            self.services.send(Command::WifiJoin { ssid, ap, device, password: None });
                        }
                    }
                }
                _ => {}
            },
            Control::Output(id) => {
                if let Some(sound) = self.sound.as_mut() {
                    sound.outputs.iter_mut().for_each(|o| o.current = o.id == id);
                    self.services.send(Command::Output(id));
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
    fn open_sound_lists_outputs() {
        let mut status = full();
        status.outputs = vec![(51, "Speaker".into(), true), (52, "HDMI".into(), false)];
        let closed = layout(1280, &status);
        status.expanded = Some(Module::Sound);
        let open = layout(1280, &status);
        assert_eq!(open.rect.size.h - closed.rect.size.h, 2 * LIST_PAD + 2 * LIST_ROW);
        assert_eq!(open.outputs.len(), 2);
        let centre = |r: Rect| r.to_f64().loc + r.to_f64().size.downscale(2.0).to_point();
        assert_eq!(open.hit(centre(open.outputs[1])), Some(Control::Output(52)));
        assert!(open.outputs.iter().all(|r| open.sound.unwrap().contains_rect(*r)));
        assert_eq!(closed.hit(closed.sound.unwrap().loc.to_f64() + Point::from((100.0, 10.0))), Some(Control::SoundMore));
    }

    #[test]
    fn open_bluetooth_lists_devices_under_the_top_row() {
        let mut status = full();
        let row = |name: &str| ListRow { symbol: Symbol::Bluetooth, name: name.into(), detail: None, lock: false, on: false };
        status.rows = vec![row("AirPods"), row("Keyboard")];
        status.expanded = Some(Module::Bluetooth);
        let l = layout(1280, &status);
        let (list, rows) = l.list.clone().unwrap();
        assert_eq!(list.loc.y, l.network.loc.y + 104 + 10);
        assert_eq!(rows.len(), 2);
        assert!(l.display.unwrap().loc.y > list.loc.y + list.size.h);
        let centre = |r: Rect| r.to_f64().loc + r.to_f64().size.downscale(2.0).to_point();
        assert_eq!(l.hit(centre(rows[1])), Some(Control::Row(1)));
        assert_eq!(l.hit(centre(l.network) + Point::from((0.0, 30.0))), Some(Control::BluetoothMore));
        // Off: no list. Wi-Fi on with no networks: one empty row.
        status.bluetooth = Some((false, "Off".into()));
        assert_eq!(layout(1280, &status).list, None);
        status.expanded = Some(Module::Wifi);
        status.rows.clear();
        assert_eq!(layout(1280, &status).list.unwrap().1.len(), 1);
    }

    #[test]
    fn output_names_drop_the_shared_card_name() {
        let names = ["Raptor Lake-P/U/H cAVS HDMI / DisplayPort 2 Output (Stereo)", "Raptor Lake-P/U/H cAVS Speaker"].map(String::from);
        assert_eq!(short_names(&names), ["HDMI / DisplayPort 2", "Speaker"]);
        assert_eq!(short_names(&["Built-in Audio Analog Stereo".into()]), ["Built-in Audio Analog Stereo"]);
        assert_eq!(short_names(&["USB Speaker".into(), "USB Spdif".into()]), ["Speaker", "Spdif"]);
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
