//! Control Center (E2.3): what it shows, where each module sits, and what clicks and slider drags do. Drawn by
//! render/control.rs from the same layout.

use crate::{services::network::Link, symbols::Symbol};
use smithay::utils::{Logical, Point, Rectangle, Size};

use super::{DROP, MARGIN, Popup};
use crate::{config::Mode, render::menubar, services::Command, shortcuts::Action, state::State, text::ts, wm::Rect};

pub const WIDTH: i32 = 320;
const PAD: i32 = 10;
const GAP: i32 = 10;
const INNER: i32 = WIDTH - 2 * PAD;
const HALF: i32 = (INNER - GAP) / 2;
pub const TOGGLE: i32 = 28;
const TOP: i32 = 104;
const MEDIA_CARD: i32 = 56;
const POWER_CARD: i32 = 44;
/// The Keep Awake tile, closed.
const AWAKE_CARD: i32 = 48;
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
    /// Night Light is warming the screen now.
    pub night: bool,
    /// Percent; `None` without a backlight.
    pub brightness: Option<u8>,
    /// Percent and muted; `None` without PipeWire.
    pub volume: Option<(u8, bool)>,
    /// Title, artist, playing.
    pub media: Option<(String, String, bool)>,
    /// Percent and charging.
    pub battery: Option<(u8, bool)>,
    /// The active power-profiles-daemon profile, and the ones it offers.
    pub profile: Option<String>,
    pub profiles: Vec<String>,
    /// Sound outputs: PipeWire id, short name, current.
    pub outputs: Vec<(u32, String, bool)>,
    /// Apps playing sound, under the Sound slider.
    pub apps: Vec<AppVolume>,
    /// The rows of an open Wi-Fi or Bluetooth list.
    pub rows: Vec<ListRow>,
    /// The first row the list shows (scrolled past the ones above); `layout` clamps it.
    pub scroll: usize,
    /// The module showing its list.
    pub expanded: Option<Module>,
    /// Keep Awake is on.
    pub awake: bool,
    /// Apps keeping the machine awake: who, and why (may be empty).
    pub blockers: Vec<(String, String)>,
    /// Opened from its menu bar item: only this module, open, with its links under it, the panel's left edge at x.
    pub bar: Option<(Module, i32)>,
    /// The captive portal's check URL, which leads to the sign-in page.
    pub portal: Option<String>,
}

/// A module that opens a list in place (one at a time).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Module {
    Wifi,
    Bluetooth,
    Sound,
    /// Keep Awake: the apps blocking sleep.
    Awake,
    /// The power mode's ▾: the profiles to pick from.
    Power,
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

/// An app's own volume row.
#[derive(Debug, Clone, PartialEq)]
pub struct AppVolume {
    /// PipeWire node id.
    pub id: u32,
    pub name: String,
    /// Its theme icon, if found.
    pub icon: Option<String>,
    /// Percent; 0 when muted.
    pub volume: u8,
}

/// A clickable or draggable part.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Control {
    Wifi,
    Bluetooth,
    Dnd,
    Dark,
    NightLight,
    Brightness,
    Volume,
    Previous,
    PlayPause,
    Next,
    Power,
    /// A power profile of the open power mode list, by its index in `Status::profiles`.
    Profile(usize),
    /// The Sound module's title row: shows or hides the outputs.
    SoundMore,
    /// A sound output, by PipeWire id.
    Output(u32),
    /// An app's volume slider, by PipeWire node id.
    AppVolume(u32),
    /// The Wi-Fi label: shows or hides the networks.
    WifiMore,
    /// The Bluetooth label: shows or hides the devices.
    BluetoothMore,
    /// A row of the open list, by its index in `Status::rows`.
    Row(usize),
    KeepAwake,
    /// The Keep Awake label: shows or hides the apps blocking sleep.
    AwakeMore,
    /// A row of `links`, by its index.
    Link(usize),
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
    /// The Sound card's app rows, one per `Status::apps`.
    pub apps: Vec<Rect>,
    /// The list under the top row while Wi-Fi or Bluetooth is open, and its rows: as many as fit above the
    /// bottom of the screen, from `Status::rows[scroll]`.
    pub list: Option<(Rect, Vec<Rect>)>,
    pub scroll: usize,
    /// The Keep Awake tile, and its rows while open (at least one: "nothing blocks").
    pub awake: Rect,
    pub blockers: Vec<Rect>,
    pub media: Rect,
    pub power: Option<Rect>,
    /// The open power mode list's rows, one per `Status::profiles`.
    pub profiles: Vec<Rect>,
    /// A bar module's `links` rows.
    pub links: Vec<Rect>,
}

impl Layout {
    pub fn hit(&self, location: Point<f64, Logical>) -> Option<Control> {
        self.hits.iter().find(|(_, r)| r.to_f64().contains(location)).map(|(c, _)| *c)
    }

    pub fn rect_of(&self, control: Control) -> Option<Rect> {
        self.hits.iter().find(|(c, _)| *c == control).map(|(_, r)| *r)
    }
}

/// The Wi-Fi or Bluetooth toggle in the network card: row 0 or 1. Rows of text grow with the text size (E8.4);
/// toggles, icons and widths stay.
pub fn toggle_in(card: Rect, row: i32) -> Rect {
    Rectangle::new((card.loc.x + 10, card.loc.y + ts(12) + row * ts(50)).into(), (TOGGLE, TOGGLE).into())
}

/// The top of a toggle's title and subtitle (16 + 14 px of text), centred on the toggle.
pub fn label_top(toggle: Rect) -> i32 {
    toggle.loc.y + (TOGGLE - ts(30)) / 2
}

/// A slider card's height (62 at 1.0): title, track, padding; without an open list.
fn slider_h() -> i32 {
    ts(28) + TOGGLE + 6
}

/// The round toggle of the Do Not Disturb or Dark Mode tile: at its left, centred.
pub fn tile_toggle(card: Rect) -> Rect {
    Rectangle::new((card.loc.x + 10, card.loc.y + (card.size.h - TOGGLE) / 2).into(), (TOGGLE, TOGGLE).into())
}

/// Night Light's small round toggle in the Display card: right of its title.
pub fn night_toggle(card: Rect) -> Rect {
    Rectangle::new((card.loc.x + card.size.w - 34, card.loc.y + ts(4)).into(), (22, 22).into())
}

/// A slider card's track: under its title.
pub fn track_in(card: Rect) -> Rect {
    Rectangle::new((card.loc.x + 12, card.loc.y + ts(28)).into(), (card.size.w - 24, TOGGLE).into())
}

/// An app row's height: its slider and a little air.
fn app_row() -> i32 {
    ts(LIST_ROW) + 6
}

/// The rows the Sound card adds under its slider: the apps, then the open outputs list.
fn sound_extra(status: &Status, open: bool) -> i32 {
    let apps = status.apps.len() as i32 * app_row();
    let outputs = if open { 2 * LIST_PAD + status.outputs.len() as i32 * ts(LIST_ROW) } else { 0 };
    apps + outputs
}

/// An app row's slider: right of its icon and name.
pub fn app_track(row: Rect) -> Rect {
    let x = row.loc.x + 120;
    Rectangle::new((x, row.loc.y + (row.size.h - TOGGLE) / 2).into(), (row.loc.x + row.size.w - 6 - x, TOGGLE).into())
}

/// The Now Playing buttons, right to left from the card's edge: previous, play/pause, next.
pub fn media_buttons(card: Rect) -> [Rect; 3] {
    let at = |i: i32| {
        Rectangle::new(
            (card.loc.x + card.size.w - 12 - TOGGLE - (2 - i) * (TOGGLE + 8), card.loc.y + (card.size.h - TOGGLE) / 2).into(),
            (TOGGLE, TOGGLE).into(),
        )
    };
    [at(0), at(1), at(2)]
}

/// The rows under a bar item's module: sign in behind a captive portal, then the module's Settings page.
pub fn links(status: &Status) -> Vec<(&'static str, Action)> {
    let settings = |label, page| (label, Action::Spawn(format!("noobsbot-desktop-settings --page {page}")));
    match status.bar {
        Some((Module::Wifi, _)) => {
            let portal = status.portal.iter().map(|url| ("Sign in to network…", Action::Spawn(format!("xdg-open {url}"))));
            portal.chain([settings("Network Settings…", "network")]).collect()
        }
        Some((Module::Bluetooth, _)) => vec![settings("Bluetooth Settings…", "bluetooth")],
        Some((Module::Sound, _)) => vec![settings("Sound Settings…", "sound")],
        Some((Module::Awake | Module::Power, _)) | None => Vec::new(),
    }
}

/// The whole panel at the top right, or with `Status::bar` one module under its bar item.
pub fn layout(output: Size<i32, Logical>, status: &Status) -> Layout {
    let only = status.bar.map(|(module, _)| module);
    let full = only.is_none();
    let x = status.bar.map_or(output.w, |(_, left)| left).min(output.w - MARGIN - WIDTH).max(MARGIN);
    let top = menubar::height() + DROP;
    let card = |dx: i32, y: i32, w: i32, h: i32| Rectangle::new((x + dx, y).into(), (w, h).into());
    let mut y = top + PAD;
    let (top_h, list_row, awake_h, media_h, power_h) = (ts(TOP), ts(LIST_ROW), ts(AWAKE_CARD), ts(MEDIA_CARD), ts(POWER_CARD));
    let (mut hits, mut cards) = (Vec::new(), Vec::new());
    let (mut network, mut dnd, mut dark) = Default::default();
    if full {
        network = card(PAD, y, HALF, top_h);
        let small = (top_h - GAP) / 2;
        dnd = card(PAD + HALF + GAP, y, HALF, small);
        dark = card(PAD + HALF + GAP, y + small + GAP, HALF, small);
        // Right of a toggle: its title and subtitle.
        let label_of = |t: Rect| {
            Rectangle::new(
                (t.loc.x + TOGGLE + 4, t.loc.y + (TOGGLE - ts(36)) / 2).into(),
                (network.loc.x + network.size.w - t.loc.x - TOGGLE - 8, ts(36)).into(),
            )
        };
        hits = vec![
            (Control::Wifi, toggle_in(network, 0)),
            (Control::WifiMore, label_of(toggle_in(network, 0))),
            (Control::Bluetooth, toggle_in(network, 1)),
            (Control::BluetoothMore, label_of(toggle_in(network, 1))),
            (Control::Dnd, dnd),
            (Control::Dark, dark),
        ];
        cards = vec![network, dnd, dark];
        y += top_h + GAP;
    } else if let Some(module @ (Module::Wifi | Module::Bluetooth)) = only {
        // Its toggle alone, full width.
        network = card(PAD, y, INNER, ts(24) + TOGGLE);
        hits.push((if module == Module::Wifi { Control::Wifi } else { Control::Bluetooth }, toggle_in(network, 0)));
        cards.push(network);
        y += network.size.h + GAP;
    }
    let links_n = links(status).len() as i32;
    let links_h = if links_n > 0 { 2 * LIST_PAD + links_n * list_row + GAP } else { 0 };
    // An open Wi-Fi or Bluetooth module lists its networks or devices full width under the top row; with none, one
    // empty row says so.
    let on = |module: &Option<(bool, String)>| module.as_ref().is_some_and(|m| m.0);
    let open = match status.expanded {
        Some(Module::Wifi) => on(&status.wifi),
        Some(Module::Bluetooth) => on(&status.bluetooth),
        _ => false,
    };
    let mut scroll = 0;
    let list = open.then(|| {
        // The cards under it must stay on screen: the list shows the rows that fit and scrolls the rest.
        let after = links_h
            + [
                (status.brightness.is_some(), slider_h()),
                (status.volume.is_some(), slider_h() + sound_extra(status, status.expanded == Some(Module::Sound) && !status.outputs.is_empty())),
                (true, awake_h),
                (true, media_h),
                (status.battery.is_some() || status.profile.is_some(), power_h),
            ]
            .into_iter()
            .filter(|(shown, _)| *shown && full)
            .map(|(_, h)| h + GAP)
            .sum::<i32>();
        let fit = ((output.h - MARGIN - PAD - after - y - 2 * LIST_PAD) / list_row).max(1) as usize;
        let shown = status.rows.len().clamp(1, fit);
        scroll = status.scroll.min(status.rows.len().saturating_sub(shown));
        let rows = shown as i32;
        let list = card(PAD, y, INNER, 2 * LIST_PAD + rows * list_row);
        let cells: Vec<Rect> = (0..rows)
            .map(|i| Rectangle::new((list.loc.x + LIST_PAD, list.loc.y + LIST_PAD + i * list_row).into(), (list.size.w - 2 * LIST_PAD, list_row).into()))
            .collect();
        hits.extend(cells.iter().take(status.rows.len()).enumerate().map(|(i, r)| (Control::Row(scroll + i), *r)));
        cards.push(list);
        y += list.size.h + GAP;
        (list, cells)
    });
    let (mut awake, mut blockers) = (Rect::default(), Vec::new());
    if full {
        let rows = if status.expanded == Some(Module::Awake) { status.blockers.len().max(1) as i32 } else { 0 };
        awake = card(PAD, y, INNER, awake_h + if rows > 0 { 2 * LIST_PAD + rows * list_row } else { 0 });
        blockers = (0..rows)
            .map(|i| {
                Rectangle::new((awake.loc.x + LIST_PAD, awake.loc.y + awake_h + LIST_PAD + i * list_row).into(), (awake.size.w - 2 * LIST_PAD, list_row).into())
            })
            .collect();
        let head = Rectangle::new(awake.loc, (awake.size.w, awake_h).into());
        let toggle = tile_toggle(head);
        let more = toggle.loc.x + TOGGLE + 4;
        hits.extend([
            (Control::KeepAwake, toggle),
            (Control::AwakeMore, Rectangle::new((more, head.loc.y).into(), (head.loc.x + head.size.w - more, awake_h).into())),
        ]);
        cards.push(awake);
        y += awake.size.h + GAP;
    }
    let mut slider = |control, present: bool, extra: i32, y: &mut i32| {
        present.then(|| {
            let c = card(PAD, *y, INNER, slider_h() + extra);
            hits.push((control, track_in(c)));
            cards.push(c);
            *y += c.size.h + GAP;
            c
        })
    };
    let display = slider(Control::Brightness, full && status.brightness.is_some(), 0, &mut y);
    let open = status.expanded == Some(Module::Sound) && !status.outputs.is_empty();
    let sound = slider(Control::Volume, matches!(only, None | Some(Module::Sound)) && status.volume.is_some(), sound_extra(status, open), &mut y);
    hits.extend(display.map(|c| (Control::NightLight, night_toggle(c))));
    let (mut outputs, mut apps) = (Vec::new(), Vec::new());
    if let Some(c) = sound {
        if full {
            hits.push((Control::SoundMore, Rectangle::new(c.loc, (c.size.w, ts(28)).into())));
        }
        for (i, app) in status.apps.iter().enumerate() {
            let row = Rectangle::new((c.loc.x + LIST_PAD, c.loc.y + slider_h() + i as i32 * app_row()).into(), (c.size.w - 2 * LIST_PAD, app_row()).into());
            hits.push((Control::AppVolume(app.id), app_track(row)));
            apps.push(row);
        }
        let below = c.loc.y + slider_h() + status.apps.len() as i32 * app_row();
        if open {
            for (i, (id, _, _)) in status.outputs.iter().enumerate() {
                let row = Rectangle::new((c.loc.x + LIST_PAD, below + LIST_PAD + i as i32 * list_row).into(), (c.size.w - 2 * LIST_PAD, list_row).into());
                hits.push((Control::Output(*id), row));
                outputs.push(row);
            }
        }
    }
    let mut media = Rect::default();
    if full {
        media = card(PAD, y, INNER, media_h);
        cards.push(media);
        if status.media.is_some() {
            hits.extend([Control::Previous, Control::PlayPause, Control::Next].into_iter().zip(media_buttons(media)));
        }
        y += media_h + GAP;
    }
    let mut profiles = Vec::new();
    let power = (full && (status.battery.is_some() || status.profile.is_some())).then(|| {
        let rows = if status.expanded == Some(Module::Power) && status.profile.is_some() { status.profiles.len() as i32 } else { 0 };
        let c = card(PAD, y, INNER, power_h + if rows > 0 { 2 * LIST_PAD + rows * list_row } else { 0 });
        if status.profile.is_some() {
            hits.push((Control::Power, Rectangle::new(c.loc, (c.size.w, power_h).into())));
        }
        profiles = (0..rows)
            .map(|i| Rectangle::new((c.loc.x + LIST_PAD, c.loc.y + power_h + LIST_PAD + i * list_row).into(), (c.size.w - 2 * LIST_PAD, list_row).into()))
            .collect();
        hits.extend(profiles.iter().enumerate().map(|(i, r)| (Control::Profile(i), *r)));
        cards.push(c);
        y += c.size.h + GAP;
        c
    });
    let mut links = Vec::new();
    if links_n > 0 {
        let c = card(PAD, y, INNER, links_h - GAP);
        links = (0..links_n)
            .map(|i| Rectangle::new((c.loc.x + LIST_PAD, c.loc.y + LIST_PAD + i * list_row).into(), (c.size.w - 2 * LIST_PAD, list_row).into()))
            .collect();
        hits.extend(links.iter().enumerate().map(|(i, r)| (Control::Link(i), *r)));
        cards.push(c);
        y += links_h;
    }
    // The last gap becomes the bottom padding.
    Layout {
        rect: Rectangle::new((x, top).into(), (WIDTH, y - GAP + PAD - top).into()),
        cards,
        hits,
        network,
        dnd,
        dark,
        display,
        sound,
        outputs,
        apps,
        list,
        scroll,
        awake,
        blockers,
        media,
        power,
        profiles,
        links,
    }
}

/// The percent a slider shows with the pointer at `x`: the knob end follows the pointer.
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
        // A bar item's module: always open, under its item.
        let bar = match self.popup {
            Some(Popup::Module(module, i)) => {
                let pill = self.popup_output().and_then(|o| self.bar_view(&o)?.pill(menubar::BarItem::Status(i)));
                Some((module, pill.map_or(0, |r| r.loc.x)))
            }
            _ => None,
        };
        let expanded = bar.map(|(module, _)| module).or(self.expanded);
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
            dnd: self.config.notifications.dnd,
            dark: crate::render::is_dark(&self.config.appearance),
            night: self.night_light.on(&self.config.night_light),
            brightness: self.brightness,
            volume: self.sound.as_ref().map(|s| (s.output.volume, s.output.muted)),
            media: self.media.as_ref().map(|m| (m.title.clone(), m.artist.clone(), m.playing)),
            battery: self.battery.as_ref().map(|b| (b.percent, b.charging)),
            profile: self.profiles.as_ref().map(|p| p.active.clone()),
            profiles: self.profiles.as_ref().map_or_else(Vec::new, |p| p.available.clone()),
            outputs: self.sound.as_ref().map_or_else(Vec::new, |s| {
                let names = short_names(&s.outputs.iter().map(|o| o.name.clone()).collect::<Vec<_>>());
                s.outputs.iter().zip(names).map(|(o, name)| (o.id, name, o.current)).collect()
            }),
            apps: self.sound.as_ref().map_or_else(Vec::new, |s| {
                s.streams
                    .iter()
                    .map(|st| AppVolume {
                        id: st.id,
                        // The installed app's name and icon (`firefox` → Firefox's), else what the stream says.
                        name: self.apps.find(&st.name).map_or_else(|| st.name.clone(), |a| a.name.clone()),
                        icon: self.apps.find(&st.name).map(|a| a.icon.clone()).or_else(|| st.icon.clone()),
                        volume: if st.muted { 0 } else { st.volume },
                    })
                    .collect()
            }),
            rows: match expanded {
                Some(Module::Wifi) => self.network.as_ref().map_or_else(Vec::new, |n| {
                    let current = match &n.link {
                        Link::Wifi { ssid, .. } => Some(ssid.as_str()),
                        _ => None,
                    };
                    let networks = n.networks.iter().map(|w| ListRow {
                        symbol: wifi_symbol(w.bars),
                        name: w.ssid.clone(),
                        detail: None,
                        lock: w.security.secure(),
                        on: current == Some(w.ssid.as_str()),
                    });
                    // The VPNs follow the networks; a click brings one up or down.
                    let vpns = n.vpns.iter().map(|v| ListRow {
                        symbol: Symbol::Lock,
                        name: v.name.clone(),
                        detail: Some("VPN".into()),
                        lock: false,
                        on: v.active.is_some(),
                    });
                    networks.chain(vpns).collect()
                }),
                Some(Module::Bluetooth) => self.bluetooth.as_ref().map_or_else(Vec::new, |b| {
                    b.devices
                        .iter()
                        .map(|d| ListRow {
                            symbol: device_symbol(&d.name),
                            name: d.name.clone(),
                            detail: d.battery.map(|p| format!("{p}%")),
                            lock: false,
                            on: d.connected,
                        })
                        .collect()
                }),
                _ => Vec::new(),
            },
            scroll: (self.control_scroll / f64::from(ts(LIST_ROW))).round() as usize,
            expanded,
            bar,
            portal: self.network.as_ref().and_then(|n| n.portal.clone()),
            awake: self.keep_awake,
            blockers: self
                .idle_blockers()
                .into_iter()
                .map(|who| (who, String::new()))
                .chain(self.sleep_blockers.iter().map(|i| (i.who.clone(), i.why.clone())))
                .collect(),
        }
    }

    pub fn control_layout(&self) -> Layout {
        layout(self.popup_size(), &self.control_status())
    }

    /// The wheel or a two-finger swipe over the panel scrolls its open list. `true`: the panel took it.
    pub fn control_scroll(&mut self, location: Point<f64, Logical>, dy: f64) -> bool {
        if !matches!(self.popup, Some(Popup::Control | Popup::Module(..))) {
            return false;
        }
        let status = self.control_status();
        let layout = layout(self.popup_size(), &status);
        if !layout.rect.to_f64().contains(self.to_popup(location)) {
            return false;
        }
        let Some((_, cells)) = &layout.list else { return true };
        let max = f64::from(ts(LIST_ROW)) * status.rows.len().saturating_sub(cells.len()) as f64;
        let scroll = (self.control_scroll + dy).clamp(0.0, max);
        if scroll != self.control_scroll {
            self.control_scroll = scroll;
            self.schedule_redraw();
        }
        true
    }

    /// A press inside the panel. Each change shows at once; the service's echo confirms it.
    pub(crate) fn control_press(&mut self, location: Point<f64, Logical>) {
        let status = self.control_status();
        let Some(control) = layout(self.popup_size(), &status).hit(location) else { return };
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
            Control::Dnd => {
                let dnd = !self.config.notifications.dnd;
                self.config.notifications.dnd = dnd;
                crate::config::save_key("notifications", "dnd", if dnd { "true" } else { "false" });
            }
            Control::Dark => {
                let nested = self.nested;
                let appearance = &mut self.config.appearance;
                appearance.mode = if crate::render::is_dark(appearance) { Mode::Light } else { Mode::Dark };
                // Saved, so the next live reload (System Settings saving anything) keeps it.
                crate::config::save_key("appearance", "mode", if appearance.mode == Mode::Dark { "\"dark\"" } else { "\"light\"" });
                if appearance.theme_apps {
                    crate::theme::write_in_background(appearance.clone(), !nested);
                }
            }
            Control::NightLight => self.night_light.toggle(&self.config.night_light),
            Control::Brightness | Control::Volume | Control::AppVolume(_) => {
                self.shell_drag = Some(control);
                self.control_drag(location);
            }
            Control::Previous => self.run_action(Action::Media(crate::services::media::Action::Previous)),
            Control::PlayPause => self.run_action(Action::Media(crate::services::media::Action::PlayPause)),
            Control::Next => self.run_action(Action::Media(crate::services::media::Action::Next)),
            Control::Power => self.expanded = if self.expanded == Some(Module::Power) { None } else { Some(Module::Power) },
            Control::Profile(i) => {
                if let Some(profiles) = self.profiles.as_mut()
                    && let Some(profile) = profiles.available.get(i).cloned()
                {
                    profiles.active = profile.clone();
                    self.services.send(Command::PowerProfile(profile));
                }
                self.expanded = None;
            }
            // ponytail: opens at once; the in-place `smooth` spring comes with the animation module.
            Control::SoundMore => self.expanded = if self.expanded == Some(Module::Sound) { None } else { Some(Module::Sound) },
            Control::WifiMore => {
                self.expanded = if self.expanded == Some(Module::Wifi) { None } else { Some(Module::Wifi) };
                self.control_scroll = 0.0;
                if self.expanded.is_some() {
                    self.services.send(Command::WifiScan);
                }
            }
            Control::BluetoothMore => {
                self.expanded = if self.expanded == Some(Module::Bluetooth) { None } else { Some(Module::Bluetooth) };
                self.control_scroll = 0.0;
            }
            // ponytail: no "Connecting…" state; the row flips when the service reports the change.
            Control::Row(i) => match status.expanded {
                Some(Module::Bluetooth) => {
                    if let Some(device) = self.bluetooth.as_ref().and_then(|b| b.devices.get(i)) {
                        self.services.send(Command::BluetoothDevice { path: device.path.clone(), connect: !device.connected });
                    }
                }
                Some(Module::Wifi) => {
                    let vpn = self.network.as_ref().and_then(|n| n.vpns.get(i.checked_sub(n.networks.len())?));
                    if let Some(vpn) = vpn {
                        self.services.send(Command::Vpn { path: vpn.path.clone(), active: vpn.active.clone() });
                    } else if let Some(network) = self.network.as_ref().and_then(|n| n.networks.get(i)) {
                        let joined = matches!(&self.network.as_ref().map(|n| &n.link), Some(Link::Wifi { ssid, .. }) if *ssid == network.ssid);
                        let (ssid, ap, device) = (network.ssid.clone(), network.ap.clone(), network.device.clone());
                        if joined {
                        } else if let Some(secret) = network.security.asks().filter(|_| !network.known) {
                            let field = crate::ui::field::TextField::secure();
                            self.open_ask(crate::shell::alert::Kind::Wifi { ssid, join: Some((ap, device)), field, secret });
                        } else {
                            self.services.send(Command::WifiJoin { ssid, ap, device, password: None });
                        }
                    }
                }
                _ => {}
            },
            Control::Link(i) => {
                if let Some((_, action)) = links(&status).into_iter().nth(i) {
                    self.popup = None;
                    self.run_action(action);
                }
            }
            Control::KeepAwake => self.set_keep_awake(!self.keep_awake),
            Control::AwakeMore => self.expanded = if self.expanded == Some(Module::Awake) { None } else { Some(Module::Awake) },
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
            Control::AppVolume(id) => {
                let Some(stream) = self.sound.as_mut().and_then(|s| s.streams.iter_mut().find(|s| s.id == id)) else { return };
                if stream.volume != percent {
                    stream.volume = percent;
                    self.services.send(Command::StreamVolume { id, percent });
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
    fn power_mode_opens_its_profiles_under_it() {
        let profiles = vec!["power-saver".to_owned(), "balanced".to_owned(), "performance".to_owned()];
        let shut = layout((1280, 800).into(), &Status { profiles: profiles.clone(), ..full() });
        let open = layout((1280, 800).into(), &Status { profiles, expanded: Some(Module::Power), ..full() });
        assert!(shut.profiles.is_empty());
        assert_eq!(open.profiles.len(), 3);
        let (card, head) = (open.power.unwrap(), open.rect_of(Control::Power).unwrap());
        assert_eq!(head.size.h, ts(POWER_CARD), "the ▾ row stays the card's top");
        assert_eq!(open.rect.size.h - shut.rect.size.h, card.size.h - shut.power.unwrap().size.h);
        for (i, row) in open.profiles.iter().enumerate() {
            assert!(card.contains_rect(*row) && !row.overlaps(head));
            assert_eq!(open.hit(row.loc.to_f64() + Point::from((4.0, 4.0))), Some(Control::Profile(i)));
        }
    }

    #[test]
    fn modules_stack_inside_the_panel() {
        let l = layout((1280, 800).into(), &full());
        assert_eq!(l.rect.loc, (1280 - 8 - 320, 28).into());
        // 10 + 104 + 10 + 48 + 10 + 62 + 10 + 62 + 10 + 56 + 10 + 44 + 10
        assert_eq!(l.rect.size.h, 446);
        assert!(l.cards.iter().all(|c| l.rect.contains_rect(*c)));
        assert!(l.hits.iter().all(|(_, r)| l.rect.contains_rect(*r)));
        for (i, a) in l.cards.iter().enumerate() {
            assert!(l.cards[i + 1..].iter().all(|b| !a.overlaps(*b)), "card {i} overlaps");
        }
    }

    #[test]
    fn text_rows_grow_with_the_text_size() {
        let status = Status { expanded: Some(Module::Sound), outputs: vec![(1, "Speaker".into(), true)], ..full() };
        let small = layout((1280, 1400).into(), &status);
        let l = crate::text::with_scale(1.5, || layout((1280, 1400).into(), &status));
        assert!(l.rect.size.h > small.rect.size.h && l.rect.size.w == small.rect.size.w);
        assert!(l.cards.iter().chain(l.hits.iter().map(|(_, r)| r)).all(|c| l.rect.contains_rect(*c)));
        for (i, a) in l.cards.iter().enumerate() {
            assert!(l.cards[i + 1..].iter().all(|b| !a.overlaps(*b)), "card {i} overlaps");
        }
        // Title (24) and subtitle (21) beside each toggle stay in the card; toggles keep their size.
        for t in [toggle_in(l.network, 0), toggle_in(l.network, 1)] {
            let top = crate::text::with_scale(1.5, || label_top(t));
            assert!(top >= l.network.loc.y && top + 45 <= l.network.loc.y + l.network.size.h && t.size.h == TOGGLE);
        }
        // "Do Not / Disturb": two body lines in the small tile.
        assert!(l.dnd.size.h >= 48 && l.awake.size.h >= 45 && l.media.size.h >= 24 + 21 && l.power.unwrap().size.h >= 24);
        let sound = l.sound.unwrap();
        assert!(
            crate::text::with_scale(1.5, || track_in(sound)).loc.y >= sound.loc.y + 12 + 24
                && l.outputs.iter().all(|r| r.size.h >= 24 && sound.contains_rect(*r))
        );
    }

    #[test]
    fn missing_services_drop_their_modules() {
        let l = layout((1280, 800).into(), &Status::default());
        assert_eq!((l.display, l.sound, l.power), (None, None, None));
        // Top row, Keep Awake, Now Playing: 10 + 104 + 10 + 48 + 10 + 56 + 10.
        assert_eq!(l.rect.size.h, 248);
        // Nothing playing: no buttons.
        assert_eq!(l.hit(media_buttons(l.media)[1].to_f64().loc + Point::from((14.0, 14.0))), None);
    }

    #[test]
    fn hits_find_their_control() {
        let l = layout((1280, 800).into(), &full());
        let centre = |r: Rect| r.to_f64().loc + r.to_f64().size.downscale(2.0).to_point();
        assert_eq!(l.hit(centre(toggle_in(l.network, 1))), Some(Control::Bluetooth));
        assert_eq!(l.hit(centre(l.dark)), Some(Control::Dark));
        assert_eq!(l.hit(centre(night_toggle(l.display.unwrap()))), Some(Control::NightLight));
        assert_eq!(l.hit(centre(media_buttons(l.media)[1])), Some(Control::PlayPause));
        assert_eq!(l.hit(centre(track_in(l.sound.unwrap()))), Some(Control::Volume));
    }

    #[test]
    fn open_sound_lists_outputs() {
        let mut status = full();
        status.outputs = vec![(51, "Speaker".into(), true), (52, "HDMI".into(), false)];
        let closed = layout((1280, 800).into(), &status);
        status.expanded = Some(Module::Sound);
        let open = layout((1280, 800).into(), &status);
        assert_eq!(open.rect.size.h - closed.rect.size.h, 2 * LIST_PAD + 2 * LIST_ROW);
        assert_eq!(open.outputs.len(), 2);
        let centre = |r: Rect| r.to_f64().loc + r.to_f64().size.downscale(2.0).to_point();
        assert_eq!(open.hit(centre(open.outputs[1])), Some(Control::Output(52)));
        assert!(open.outputs.iter().all(|r| open.sound.unwrap().contains_rect(*r)));
        assert_eq!(closed.hit(closed.sound.unwrap().loc.to_f64() + Point::from((100.0, 10.0))), Some(Control::SoundMore));
    }

    #[test]
    fn apps_get_rows_under_the_sound_slider() {
        let mut status = full();
        status.outputs = vec![(51, "Speaker".into(), true)];
        let app = |id, name: &str| AppVolume { id, name: name.into(), icon: None, volume: 50 };
        let without = layout((1280, 800).into(), &status);
        status.apps = vec![app(89, "Firefox"), app(95, "Music")];
        let l = layout((1280, 800).into(), &status);
        assert_eq!(l.rect.size.h - without.rect.size.h, 2 * app_row());
        let sound = l.sound.unwrap();
        assert_eq!(l.apps.len(), 2);
        assert!(l.apps.iter().all(|r| sound.contains_rect(*r) && sound.contains_rect(app_track(*r))));
        assert!(l.apps[0].loc.y >= track_in(sound).loc.y + TOGGLE && l.apps[1].loc.y == l.apps[0].loc.y + app_row());
        let centre = |r: Rect| r.to_f64().loc + r.to_f64().size.downscale(2.0).to_point();
        assert_eq!(l.hit(centre(app_track(l.apps[1]))), Some(Control::AppVolume(95)));
        // The name is not a button; the main slider still is.
        assert_eq!(l.hit(l.apps[0].loc.to_f64() + Point::from((20.0, 10.0))), None);
        assert_eq!(l.hit(centre(track_in(sound))), Some(Control::Volume));
        // Open, the outputs follow the apps.
        status.expanded = Some(Module::Sound);
        let open = layout((1280, 800).into(), &status);
        assert!(open.outputs[0].loc.y >= open.apps[1].loc.y + app_row());
        assert_eq!(open.hit(centre(open.outputs[0])), Some(Control::Output(51)));
    }

    #[test]
    fn open_bluetooth_lists_devices_under_the_top_row() {
        let mut status = full();
        let row = |name: &str| ListRow { symbol: Symbol::Bluetooth, name: name.into(), detail: None, lock: false, on: false };
        status.rows = vec![row("AirPods"), row("Keyboard")];
        status.expanded = Some(Module::Bluetooth);
        let l = layout((1280, 800).into(), &status);
        let (list, rows) = l.list.clone().unwrap();
        assert_eq!(list.loc.y, l.network.loc.y + 104 + 10);
        assert_eq!(rows.len(), 2);
        assert!(l.display.unwrap().loc.y > list.loc.y + list.size.h);
        let centre = |r: Rect| r.to_f64().loc + r.to_f64().size.downscale(2.0).to_point();
        assert_eq!(l.hit(centre(rows[1])), Some(Control::Row(1)));
        assert_eq!(l.hit(centre(l.network) + Point::from((0.0, 30.0))), Some(Control::BluetoothMore));
        // Off: no list. Wi-Fi on with no networks: one empty row.
        status.bluetooth = Some((false, "Off".into()));
        assert_eq!(layout((1280, 800).into(), &status).list, None);
        status.expanded = Some(Module::Wifi);
        status.rows.clear();
        assert_eq!(layout((1280, 800).into(), &status).list.unwrap().1.len(), 1);
    }

    #[test]
    fn long_lists_fit_the_screen_and_scroll() {
        let mut status = full();
        status.rows = (0..20).map(|i| ListRow { symbol: Symbol::WifiHigh, name: format!("net {i}"), detail: None, lock: true, on: false }).collect();
        status.expanded = Some(Module::Wifi);
        let short = layout((1366, 768).into(), &status);
        let (_, rows) = short.list.clone().unwrap();
        assert!(short.rect.loc.y + short.rect.size.h <= 768 - 8, "the panel ends above the screen's bottom");
        assert!(rows.len() < 20 && short.power.unwrap().loc.y > rows[rows.len() - 1].loc.y);
        assert_eq!(short.scroll, 0);
        // Scrolled past the end: held at the last page, rows hit by their index in `Status::rows`.
        status.scroll = 100;
        let scrolled = layout((1366, 768).into(), &status);
        assert_eq!(scrolled.scroll, 20 - rows.len());
        let centre = |r: Rect| r.to_f64().loc + r.to_f64().size.downscale(2.0).to_point();
        assert_eq!(scrolled.hit(centre(rows[0])), Some(Control::Row(scrolled.scroll)));
        // A tall screen shows them all.
        assert_eq!(layout((1366, 1600).into(), &status).list.unwrap().1.len(), 20);
    }

    #[test]
    fn keep_awake_toggles_and_lists_blockers() {
        let mut status = full();
        status.blockers = vec![("Videos".into(), String::new()), ("Transmission".into(), "Downloading".into())];
        let closed = layout((1280, 800).into(), &status);
        let centre = |r: Rect| r.to_f64().loc + r.to_f64().size.downscale(2.0).to_point();
        assert_eq!(closed.hit(centre(tile_toggle(closed.awake))), Some(Control::KeepAwake));
        assert_eq!(closed.hit(centre(closed.awake) + Point::from((60.0, 0.0))), Some(Control::AwakeMore));
        assert!(closed.blockers.is_empty());
        status.expanded = Some(Module::Awake);
        let open = layout((1280, 800).into(), &status);
        assert_eq!(open.rect.size.h - closed.rect.size.h, 2 * LIST_PAD + 2 * LIST_ROW);
        assert!(open.blockers.iter().all(|r| open.awake.contains_rect(*r)));
        assert_eq!(open.hit(centre(open.awake)), None, "rows are not buttons");
        // Nothing blocks: one row says so.
        status.blockers.clear();
        assert_eq!(layout((1280, 800).into(), &status).blockers.len(), 1);
    }

    #[test]
    fn bar_modules_show_one_module_and_its_links() {
        let row = |name: &str| ListRow { symbol: Symbol::WifiHigh, name: name.into(), detail: None, lock: true, on: false };
        let centre = |r: Rect| r.to_f64().loc + r.to_f64().size.downscale(2.0).to_point();
        let wifi = Status { bar: Some((Module::Wifi, 900)), expanded: Some(Module::Wifi), rows: vec![row("Home"), row("Cafe")], ..full() };
        let l = layout((1280, 800).into(), &wifi);
        assert_eq!(l.rect.loc.x, 900);
        assert_eq!((l.display, l.sound, l.power, l.blockers.len()), (None, None, None, 0));
        assert_eq!(l.hits.iter().map(|(c, _)| *c).collect::<Vec<_>>(), [Control::Wifi, Control::Row(0), Control::Row(1), Control::Link(0)]);
        assert!(l.cards.iter().all(|c| l.rect.contains_rect(*c)));
        assert_eq!(links(&wifi).iter().map(|(name, _)| *name).collect::<Vec<_>>(), ["Network Settings…"]);
        // Behind a captive portal, signing in comes first.
        let portal = Status { portal: Some("http://check.example/".into()), ..wifi.clone() };
        assert_eq!(links(&portal)[0], ("Sign in to network…", Action::Spawn("xdg-open http://check.example/".into())));
        assert_eq!(layout((1280, 800).into(), &portal).links.len(), 2);
        // Near the right edge it stays on screen; Bluetooth's toggle hits as Bluetooth.
        let bt = Status { bar: Some((Module::Bluetooth, 1200)), expanded: Some(Module::Bluetooth), ..full() };
        let l = layout((1280, 800).into(), &bt);
        assert_eq!(l.rect.loc.x, 1280 - 8 - WIDTH);
        assert_eq!(l.hit(centre(toggle_in(l.network, 0))), Some(Control::Bluetooth));
        assert_eq!(links(&bt)[0].1, Action::Spawn("noobsbot-desktop-settings --page bluetooth".into()));
        // Sound: the slider with its outputs open, no title toggle.
        let sound = Status {
            bar: Some((Module::Sound, 900)),
            expanded: Some(Module::Sound),
            outputs: vec![(51, "Speaker".into(), true), (52, "HDMI".into(), false)],
            ..full()
        };
        let l = layout((1280, 800).into(), &sound);
        assert_eq!(l.outputs.len(), 2);
        assert_eq!(l.hit(centre(track_in(l.sound.unwrap()))), Some(Control::Volume));
        assert_eq!(l.hit(centre(l.links[0])), Some(Control::Link(0)));
        assert!(l.rect_of(Control::SoundMore).is_none() && l.rect_of(Control::Wifi).is_none());
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
