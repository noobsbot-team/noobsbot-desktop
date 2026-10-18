//! Display settings (E6.2): what each connected display is lit with and where it goes, from `[display]` and its
//! `[[display.outputs]]` entry (`plan`, pure), applied live by `State::apply_displays` on both backends, and written
//! to `displays.json` for System Settings.

use smithay::{
    desktop::layer_map_for_output,
    output::{Output, Scale},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::Transform,
    wayland::{
        compositor::{SurfaceData, send_surface_state},
        fractional_scale::with_fractional_scale,
    },
};

use crate::{
    backend::Device,
    config::{Displays, OutputSettings},
    display_info::{self, Display, Mode},
    state::State,
};

/// `displays` (their identity and modes filled in) with what each should be lit with: on or off, mode, scale,
/// rotation, position and primary. Entries are matched by connector name; a display without one gets its preferred
/// mode at the highest refresh, the `[display] scale` (or auto) and a place right of the others. At least one
/// display stays on, and exactly one on display is primary.
pub fn plan(mut displays: Vec<Display>, config: &Displays) -> Vec<Display> {
    let entry = |name: &str| config.outputs.iter().find(|o| o.name == name).cloned().unwrap_or_default();
    for d in &mut displays {
        let e = entry(&d.name);
        d.enabled = e.enabled;
        d.mode = configured_mode(&d.modes, &e).or_else(|| pick_mode(&d.modes).map(|i| d.modes[i])).unwrap_or(d.mode);
        d.rotation = if [0, 90, 180, 270].contains(&e.rotation) { e.rotation } else { 0 };
        d.auto_scale = pick_scale((d.mode.width, d.mode.height), (d.width_mm, d.height_mm), d.built_in);
        d.scale = [e.scale, config.scale].into_iter().find(|s| *s > 0.0).map_or(d.auto_scale, |s| s.clamp(1.0, 3.0));
        let (w, h) = if d.rotation % 180 == 90 { (d.mode.height, d.mode.width) } else { (d.mode.width, d.mode.height) };
        // As smithay's `Space::output_geometry` rounds, so neighbours meet exactly.
        (d.width, d.height) = ((f64::from(w) / d.scale).ceil() as i32, (f64::from(h) / d.scale).ceil() as i32);
        d.primary = false;
    }
    if !displays.iter().any(|d| d.enabled)
        && let Some(first) = displays.first_mut()
    {
        first.enabled = true;
    }
    // Placed ones first, then the rest in a row right of them, tops aligned.
    let mut right = i32::MIN;
    for d in displays.iter_mut().filter(|d| d.enabled) {
        if let Some([x, y]) = entry(&d.name).position {
            (d.x, d.y) = (x, y);
            right = right.max(x + d.width);
        }
    }
    let mut right = right.max(0);
    for d in displays.iter_mut().filter(|d| d.enabled && entry(&d.name).position.is_none()) {
        (d.x, d.y) = (right, 0);
        right += d.width;
    }
    let primary = displays.iter().position(|d| d.enabled && entry(&d.name).primary).or_else(|| displays.iter().position(|d| d.enabled));
    if let Some(i) = primary {
        displays[i].primary = true;
    }
    displays
}

/// The entry's `mode = "WxH"` at the refresh nearest its `refresh` (0: the highest); `None` when it names no mode the
/// display has.
fn configured_mode(modes: &[Mode], entry: &OutputSettings) -> Option<Mode> {
    let (w, h) = entry.mode.split_once('x')?;
    let (w, h) = (w.trim().parse::<i32>().ok()?, h.trim().parse::<i32>().ok()?);
    let same = modes.iter().filter(|m| (m.width, m.height) == (w, h));
    if entry.refresh > 0.0 {
        let want = (entry.refresh * 1000.0).round() as i32;
        same.min_by_key(|m| (m.refresh - want).abs()).copied()
    } else {
        same.max_by_key(|m| m.refresh).copied()
    }
}

/// The mode a display lights with by default (E6.2): its preferred resolution (else the largest) at the highest
/// refresh rate.
pub fn pick_mode(modes: &[Mode]) -> Option<usize> {
    let m = modes.iter().find(|m| m.preferred).or_else(|| modes.iter().max_by_key(|m| m.width * m.height))?;
    let size = (m.width, m.height);
    modes.iter().enumerate().filter(|(_, m)| (m.width, m.height) == size).max_by_key(|(_, m)| m.refresh).map(|(i, _)| i)
}

/// The auto scale (E6.2, D13), in 25% steps from 100 to 200%: the one nearest to drawing about 110 logical px per
/// inch on a monitor and 128 on a laptop's own panel, which sits closer to the eyes (macOS's densities: a 27-inch
/// 1440p or 5K monitor, a MacBook's default "looks like"). Never less than 1280 × 720 logical px; without a believable
/// physical size (projectors, TVs): 100%.
pub fn pick_scale((w, h): (i32, i32), (mm_w, _): (u32, u32), built_in: bool) -> f64 {
    if mm_w < 100 || w <= 0 {
        return 1.0;
    }
    let dpi = f64::from(w) * 25.4 / f64::from(mm_w);
    let per_100 = if built_in { 128.0 } else { 110.0 };
    let mut scale = ((dpi / per_100) * 4.0).round().clamp(4.0, 8.0) / 4.0;
    while scale > 1.0 && (f64::from(w) / scale < 1280.0 || f64::from(h) / scale < 720.0) {
        scale -= 0.25;
    }
    scale
}

/// The output transform for a clockwise rotation in degrees.
fn rotation(degrees: u16) -> Transform {
    match degrees {
        90 => Transform::_90,
        180 => Transform::_180,
        270 => Transform::_270,
        _ => Transform::Normal,
    }
}

/// Make (the PNP id: `BOE`, `DEL`) and model (the monitor-name descriptor) from an EDID; empty when missing.
// ponytail: the 3-letter PNP id, not the vendor's name; a small id → name table if people ask "who is AUO".
pub fn edid_names(edid: &[u8]) -> (String, String) {
    if edid.len() < 128 || edid[..8] != [0, 255, 255, 255, 255, 255, 255, 0] {
        return Default::default();
    }
    let id = u16::from_be_bytes([edid[8], edid[9]]);
    let make: String = [10, 5, 0].iter().map(|shift| char::from(b'@' + ((id >> shift) & 0x1f) as u8)).collect();
    let model = (54..126).step_by(18).find(|&d| edid[d..d + 3] == [0, 0, 0] && edid[d + 3] == 0xFC).map(|d| {
        let name = &edid[d + 5..d + 18];
        let end = name.iter().position(|&b| b == b'\n').unwrap_or(name.len());
        String::from_utf8_lossy(&name[..end]).trim().to_owned()
    });
    (make, model.unwrap_or_default())
}

impl State {
    /// Applies the display settings to every display, live: lights, darkens and modesets them (real screen), sets
    /// scale, rotation and position, puts the primary first (the shell draws on the first output), tells every
    /// window and panel its new scale, and rewrites `displays.json`. At start, on hotplug and on config reload.
    // ponytail: windows keep their logical size (maximised ones aren't refitted), and every surface gets the primary
    // display's scale; per-window scale when displays with different scales are both in use.
    pub fn apply_displays(&mut self) {
        let shown = match &self.device {
            Some(Device::Udev(_)) => self.udev_displays(),
            Some(Device::Winit(winit)) => {
                let output = winit.output().clone();
                let mode = output.current_mode().map(|m| Mode { width: m.size.w, height: m.size.h, refresh: m.refresh, preferred: true });
                let props = output.physical_properties();
                let nested = Display {
                    name: output.name(),
                    make: props.make,
                    model: props.model,
                    modes: mode.into_iter().collect(),
                    nested: true,
                    ..Default::default()
                };
                plan(vec![nested], &self.config.display).into_iter().map(|d| (d, Some(output.clone()))).collect()
            }
            None => return,
        };
        let mut lit: Vec<(&Display, &Output)> = shown.iter().filter_map(|(d, o)| Some((d, o.as_ref()?))).collect();
        lit.sort_by_key(|(d, _)| !d.primary);
        for (_, output) in &lit {
            self.space.unmap_output(output);
        }
        for (d, output) in &lit {
            // winit's GL surface is upside down relative to Wayland; Flipped180 corrects it.
            let base = if d.nested { Transform::Flipped180 } else { Transform::Normal };
            output.change_current_state(None, Some(base + rotation(d.rotation)), Some(Scale::Fractional(d.scale)), Some((d.x, d.y).into()));
            self.space.map_output(output, (d.x, d.y));
            layer_map_for_output(output).arrange();
        }
        let scale = lit.first().map_or(1.0, |(d, _)| d.scale);
        let tell = |surface: &WlSurface, states: &SurfaceData| {
            with_fractional_scale(states, |fractional| fractional.set_preferred_scale(scale));
            send_surface_state(surface, states, scale.ceil() as i32, Transform::Normal);
        };
        for window in self.space.elements() {
            window.with_surfaces(tell);
        }
        for (_, output) in &lit {
            for layer in layer_map_for_output(output).layers() {
                layer.with_surfaces(tell);
            }
        }
        write_info(&shown.iter().map(|(d, _)| d.clone()).collect::<Vec<_>>());
        self.schedule_redraw();
    }
}

/// Rewrites `displays.json` when it changed.
fn write_info(displays: &[Display]) {
    let Some(path) = display_info::path() else { return };
    let Ok(json) = serde_json::to_string_pretty(displays) else { return };
    if std::fs::read_to_string(&path).is_ok_and(|old| old == json) {
        return;
    }
    if let Err(err) = crate::config::save_atomic(&path, &json) {
        tracing::warn!("{}: {err}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: i32, height: i32, hz: i32, preferred: bool) -> Mode {
        Mode { width, height, refresh: hz * 1000, preferred }
    }

    fn display(name: &str, modes: Vec<Mode>, mm: (u32, u32)) -> Display {
        Display { name: name.into(), built_in: display_info::built_in(name), modes, width_mm: mm.0, height_mm: mm.1, ..Default::default() }
    }

    fn laptop_and_monitor() -> Vec<Display> {
        vec![
            display("eDP-1", vec![mode(1920, 1200, 60, true), mode(1280, 800, 60, false)], (300, 190)),
            display(
                "HDMI-A-1",
                vec![mode(2560, 1440, 60, true), mode(2560, 1440, 144, false), mode(1920, 1080, 60, false), mode(1920, 1080, 50, false)],
                (597, 336),
            ),
        ]
    }

    fn output(name: &str) -> OutputSettings {
        OutputSettings { name: name.into(), ..Default::default() }
    }

    #[test]
    fn mode_is_preferred_size_at_highest_refresh() {
        let modes = [mode(1920, 1080, 60, false), mode(2560, 1440, 59, true), mode(2560, 1440, 143, false), mode(3840, 2160, 30, false)];
        assert_eq!(pick_mode(&modes), Some(2));
        // No preferred mode: the largest.
        assert_eq!(pick_mode(&modes.map(|m| Mode { preferred: false, ..m })), Some(3));
        assert_eq!(pick_mode(&[]), None);
    }

    #[test]
    fn scale_follows_pixel_density() {
        // Monitors: about 110 px per inch per 100%.
        assert_eq!(pick_scale((1920, 1080), (531, 299), false), 1.0, "24-inch 1080p");
        assert_eq!(pick_scale((2560, 1440), (597, 336), false), 1.0, "27-inch 1440p");
        assert_eq!(pick_scale((3840, 2160), (597, 336), false), 1.5, "27-inch 4K");
        assert_eq!(pick_scale((3840, 2160), (527, 296), false), 1.75, "24-inch 4K");
        // Laptop panels: about 128.
        assert_eq!(pick_scale((1920, 1200), (300, 190), true), 1.25, "14-inch 1920 × 1200 (the owner's): 1536 × 960 logical");
        assert_eq!(pick_scale((1920, 1080), (344, 194), true), 1.0, "15.6-inch 1080p laptop");
        assert_eq!(pick_scale((1920, 1080), (294, 165), true), 1.25, "13.3-inch 1080p");
        assert_eq!(pick_scale((2560, 1600), (286, 179), true), 1.75, "13.3-inch 2560 × 1600: macOS's looks-like 1440 × 900");
        assert_eq!(pick_scale((2880, 1800), (301, 188), true), 2.0, "14-inch 2880 × 1800");
        assert_eq!(pick_scale((3840, 2160), (344, 194), true), 2.0, "15.6-inch 4K, capped at 200%");
        assert_eq!(pick_scale((1366, 768), (160, 90), true), 1.0, "never under 1280 × 720 logical");
        assert_eq!(pick_scale((1920, 1080), (16, 9), false), 1.0, "an aspect ratio, not a size");
        assert_eq!(pick_scale((1920, 1080), (0, 0), false), 1.0);
    }

    #[test]
    fn defaults_without_entries() {
        let p = plan(laptop_and_monitor(), &Displays::default());
        assert!(p.iter().all(|d| d.enabled));
        assert_eq!((p[0].mode, p[0].scale, p[0].x, p[0].width, p[0].height), (mode(1920, 1200, 60, true), 1.25, 0, 1536, 960));
        assert_eq!((p[1].mode.refresh, p[1].scale, p[1].x, p[1].y, p[1].width), (144_000, 1.0, 1536, 0, 2560), "highest refresh, placed right");
        assert!(p[0].primary && !p[1].primary, "the first is primary");
        let global = Displays { scale: 2.0, ..Default::default() };
        assert!(plan(laptop_and_monitor(), &global).iter().all(|d| d.scale == 2.0));
    }

    #[test]
    fn entries_match_by_name() {
        let config = Displays {
            scale: 1.5,
            outputs: vec![
                OutputSettings {
                    mode: "1920x1080".into(),
                    refresh: 50.0,
                    scale: 1.0,
                    rotation: 90,
                    position: Some([0, 0]),
                    primary: true,
                    ..output("HDMI-A-1")
                },
                OutputSettings { position: Some([1080, 400]), ..output("eDP-1") },
                OutputSettings { enabled: false, ..output("DP-9") },
            ],
            ..Default::default()
        };
        let p = plan(laptop_and_monitor(), &config);
        let hdmi = &p[1];
        assert_eq!((hdmi.mode.width, hdmi.mode.refresh, hdmi.scale, hdmi.rotation), (1920, 50_000, 1.0, 90));
        assert_eq!((hdmi.width, hdmi.height), (1080, 1920), "rotated: portrait");
        assert!(hdmi.primary && !p[0].primary);
        assert_eq!((p[0].x, p[0].y, p[0].scale), (1080, 400, 1.5), "the global scale under its own position");
    }

    #[test]
    fn bad_entries_fall_back() {
        let config = Displays {
            outputs: vec![
                OutputSettings { mode: "800x600".into(), rotation: 45, scale: 9.0, ..output("eDP-1") },
                OutputSettings { mode: "big".into(), ..output("HDMI-A-1") },
            ],
            ..Default::default()
        };
        let p = plan(laptop_and_monitor(), &config);
        assert_eq!((p[0].mode.width, p[0].rotation, p[0].scale), (1920, 0, 3.0), "unknown mode: preferred; scale clamped");
        assert_eq!(p[1].mode.refresh, 144_000);
    }

    #[test]
    fn off_displays_and_the_last_one_on() {
        let one_off = Displays { outputs: vec![OutputSettings { enabled: false, ..output("eDP-1") }], ..Default::default() };
        let p = plan(laptop_and_monitor(), &one_off);
        assert!(!p[0].enabled && p[1].enabled && p[1].primary);
        assert_eq!(p[1].x, 0, "nothing left of it any more");
        let all_off = Displays {
            outputs: vec![OutputSettings { enabled: false, ..output("eDP-1") }, OutputSettings { enabled: false, ..output("HDMI-A-1") }],
            ..Default::default()
        };
        let p = plan(laptop_and_monitor(), &all_off);
        assert!(p[0].enabled && p[0].primary && !p[1].enabled, "the first stays on");
        let primary_off = Displays { outputs: vec![OutputSettings { enabled: false, primary: true, ..output("eDP-1") }], ..Default::default() };
        assert!(plan(laptop_and_monitor(), &primary_off)[1].primary, "an off display is never primary");
    }

    #[test]
    fn unplaced_go_right_of_placed() {
        let config = Displays { outputs: vec![OutputSettings { position: Some([-1536, 100]), ..output("eDP-1") }], ..Default::default() };
        let p = plan(laptop_and_monitor(), &config);
        assert_eq!((p[0].x, p[0].y, p[1].x), (-1536, 100, 0));
    }

    #[test]
    fn edid_make_and_model() {
        let mut edid = vec![0u8; 128];
        edid[..8].copy_from_slice(&[0, 255, 255, 255, 255, 255, 255, 0]);
        // "DEL": D=4, E=5, L=12 → 0b0_00100_00101_01100.
        edid[8..10].copy_from_slice(&((4u16 << 10) | (5 << 5) | 12).to_be_bytes());
        edid[72..77].copy_from_slice(&[0, 0, 0, 0xFC, 0]);
        edid[77..90].copy_from_slice(b"DELL U2720Q\n ");
        assert_eq!(edid_names(&edid), ("DEL".into(), "DELL U2720Q".into()));
        assert_eq!(edid_names(&edid[..100]), (String::new(), String::new()));
    }
}
