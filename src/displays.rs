//! Display settings (E6.2): what each connected display is lit with and where it goes, from `[display]` and its
//! `[[display.outputs]]` entry (`plan`, pure), applied live by `State::apply_displays` on both backends, and written
//! to `displays.json` for System Settings.

use smithay::{
    desktop::{Window, layer_map_for_output},
    output::{Output, Scale},
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::{Logical, Physical, Point, Rectangle, Size, Transform},
    wayland::{
        compositor::{SurfaceData, send_surface_state},
        fractional_scale::with_fractional_scale,
        shell::xdg::ToplevelSurface,
    },
};

use crate::{
    arrange::{Area, snap},
    backend::Device,
    config::{Displays, OutputSettings},
    display_info::{self, Display, Mode},
    state::State,
};

/// `displays` (their identity and modes filled in) with what each should be lit with: on or off, mode, scale,
/// rotation, position and primary. Entries are matched by connector name; a display without one gets its preferred
/// mode at the highest refresh, the `[display] scale` (or auto) and a place right of the others. At least one
/// display stays on, and exactly one on display is primary. With the lid closed, the built-in panel is off while
/// another display is on (clamshell). A mirror (`mirror = "<connector>"`) shows an on, non-mirror other display's
/// picture and is left out of the arrangement; otherwise it falls back to extended.
pub fn plan(mut displays: Vec<Display>, config: &Displays, lid_closed: bool) -> Vec<Display> {
    let entry = |name: &str| config.outputs.iter().find(|o| o.name == name).cloned().unwrap_or_default();
    for d in &mut displays {
        let e = entry(&d.name);
        d.enabled = e.enabled;
        d.mode = configured_mode(&d.modes, &e).or_else(|| pick_mode(&d.modes).map(|i| d.modes[i])).unwrap_or(d.mode);
        d.rotation = if [0, 90, 180, 270].contains(&e.rotation) { e.rotation } else { 0 };
        d.auto_scale = pick_scale((d.mode.width, d.mode.height), (d.width_mm, d.height_mm), d.built_in);
        // In 1/120 steps, the granularity of wp_fractional_scale, so what apps are told is what we draw at.
        d.scale = [e.scale, config.scale].into_iter().find(|s| *s > 0.0).map_or(d.auto_scale, |s| (s.clamp(1.0, 3.0) * 120.0).round() / 120.0);
        let (w, h) = if d.rotation % 180 == 90 { (d.mode.height, d.mode.width) } else { (d.mode.width, d.mode.height) };
        // As smithay's `Space::output_geometry` rounds, so neighbours meet exactly.
        (d.width, d.height) = ((f64::from(w) / d.scale).ceil() as i32, (f64::from(h) / d.scale).ceil() as i32);
        d.primary = false;
        d.mirror = String::new();
        d.vrr = e.vrr && d.vrr_capable;
    }
    clamshell(&mut displays, lid_closed);
    if !displays.iter().any(|d| d.enabled)
        && let Some(first) = displays.first_mut()
    {
        first.enabled = true;
    }
    mirrors(&mut displays, |name| entry(name).mirror);
    // Placed ones first, then the rest in a row right of them, tops aligned.
    let mut right = i32::MIN;
    let extended = |d: &Display| d.enabled && d.mirror.is_empty();
    for d in displays.iter_mut().filter(|d| extended(d)) {
        if let Some([x, y]) = entry(&d.name).position {
            (d.x, d.y) = (x, y);
            right = right.max(x + d.width);
        }
    }
    let mut right = right.max(0);
    for d in displays.iter_mut().filter(|d| extended(d) && entry(&d.name).position.is_none()) {
        (d.x, d.y) = (right, 0);
        right += d.width;
    }
    // Saved positions were for the sizes of their day: a scale, mode or rotation change leaves overlaps or gaps,
    // where the pointer gets trapped. Each display is re-snapped edge to edge onto the ones before it.
    let mut placed: Vec<Area> = Vec::new();
    for d in displays.iter_mut().filter(|d| extended(d)) {
        if !placed.is_empty() {
            (d.x, d.y) = snap(Area::of(d), &placed, 0);
        }
        placed.push(Area::of(d));
    }
    let primary = displays.iter().position(|d| extended(d) && entry(&d.name).primary).or_else(|| displays.iter().position(extended));
    if let Some(i) = primary {
        displays[i].primary = true;
    }
    displays
}

/// Clamshell (E6.2): the lid closed and another display on → the built-in panel goes off; its windows and Spaces
/// then go the way of an unplugged display's. Lid open (or nothing else on): left as configured.
fn clamshell(displays: &mut [Display], lid_closed: bool) {
    if lid_closed && displays.iter().any(|d| d.enabled && !d.built_in) {
        for d in displays.iter_mut().filter(|d| d.built_in) {
            d.enabled = false;
        }
    }
}

/// Each on display's `mirror` (by connector name, from `wanted`) where it names another on display; then any whose
/// source is itself a mirror goes back to extended (no chains; two mirroring each other both do).
fn mirrors(displays: &mut [Display], wanted: impl Fn(&str) -> String) {
    let on: Vec<String> = displays.iter().filter(|d| d.enabled).map(|d| d.name.clone()).collect();
    for d in displays.iter_mut().filter(|d| d.enabled) {
        let source = wanted(&d.name);
        if source != d.name && on.contains(&source) {
            d.mirror = source;
        }
    }
    let mirroring: Vec<String> = displays.iter().filter(|d| !d.mirror.is_empty()).map(|d| d.name.clone()).collect();
    for d in displays.iter_mut().filter(|d| mirroring.contains(&d.mirror)) {
        d.mirror.clear();
    }
}

/// Where a mirror shows a picture of `src` px inside its own `dst` px: as large as fits, aspect kept, centred
/// (bars on the long sides).
pub fn letterbox(src: Size<i32, Physical>, dst: Size<i32, Physical>) -> Rectangle<i32, Physical> {
    if src.w <= 0 || src.h <= 0 {
        return Rectangle::from_size(dst);
    }
    let k = (f64::from(dst.w) / f64::from(src.w)).min(f64::from(dst.h) / f64::from(src.h));
    let size = Size::from(((f64::from(src.w) * k).round() as i32, (f64::from(src.h) * k).round() as i32));
    Rectangle::new(((dst.w - size.w) / 2, (dst.h - size.h) / 2).into(), size)
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
/// inch on a monitor and 128 on a laptop's own panel, which sits closer to the eyes (the densities of a 27-inch
/// 1440p or 5K monitor and a laptop's default "looks like"). Never less than 1280 × 720 logical px; without a believable
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

/// The serial from an EDID: its serial-string descriptor, else the 32-bit serial number; empty when it has neither.
pub fn edid_serial(edid: &[u8]) -> String {
    if edid.len() < 128 || edid[..8] != [0, 255, 255, 255, 255, 255, 255, 0] {
        return String::new();
    }
    let text = (54..126).step_by(18).find(|&d| edid[d..d + 3] == [0, 0, 0] && edid[d + 3] == 0xFF).map(|d| {
        let s = &edid[d + 5..d + 18];
        String::from_utf8_lossy(&s[..s.iter().position(|&b| b == b'\n').unwrap_or(s.len())]).trim().to_owned()
    });
    let number = u32::from_le_bytes([edid[12], edid[13], edid[14], edid[15]]);
    text.filter(|t| !t.is_empty()).unwrap_or_else(|| if number == 0 { String::new() } else { number.to_string() })
}

/// Who a display is, wherever it is plugged in: make, model and serial; its connector when the EDID says nothing.
fn identity(d: &Display) -> String {
    let id = [d.make.as_str(), d.model.as_str(), d.serial.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" ");
    if id.is_empty() { d.name.clone() } else { id }
}

/// The key a set of displays' layout is remembered under: their identities, sorted.
pub fn layout_key(displays: &[Display]) -> String {
    let mut ids: Vec<String> = displays.iter().map(identity).collect();
    ids.sort();
    ids.join(" + ")
}

/// A set's layout, kept by identity: the `[[display.outputs]]` entries of its displays as they were last used.
type Saved = Vec<(String, OutputSettings)>;

/// `outputs`' entries for `displays`, by identity, to remember.
fn remember(outputs: &[OutputSettings], displays: &[Display]) -> Saved {
    displays.iter().filter_map(|d| Some((identity(d), outputs.iter().find(|o| o.name == d.name)?.clone()))).collect()
}

/// `outputs` with `displays`' entries replaced by `saved`'s, renamed to the connectors they are on now. Entries for
/// other connectors stay. Two displays with one identity take the saved entries in order.
fn restore(saved: &Saved, displays: &[Display], outputs: &[OutputSettings]) -> Vec<OutputSettings> {
    let mut left: Vec<&(String, OutputSettings)> = saved.iter().collect();
    let mut kept: Vec<OutputSettings> = outputs.iter().filter(|o| !displays.iter().any(|d| d.name == o.name)).cloned().collect();
    for d in displays {
        if let Some(i) = left.iter().position(|(id, _)| *id == identity(d)) {
            kept.push(OutputSettings { name: d.name.clone(), ..left.remove(i).1.clone() });
        }
    }
    kept
}

/// `text` (the settings file) with its `[[display.outputs]]` tables replaced by `outputs`, where the first one was
/// (else at the end); the rest of the file and `[display]`'s own keys and comments stay.
fn set_outputs(text: &str, outputs: &[OutputSettings]) -> String {
    let mut tables = String::new();
    for o in outputs {
        tables += &format!("[[display.outputs]]\nname = {:?}\n", o.name);
        if !o.enabled {
            tables += "enabled = false\n";
        }
        if !o.mode.is_empty() {
            tables += &format!("mode = {:?}\n", o.mode);
        }
        if o.refresh > 0.0 {
            tables += &format!("refresh = {:?}\n", o.refresh);
        }
        if o.scale > 0.0 {
            tables += &format!("scale = {:?}\n", o.scale);
        }
        if o.rotation != 0 {
            tables += &format!("rotation = {}\n", o.rotation);
        }
        if let Some([x, y]) = o.position {
            tables += &format!("position = [{x}, {y}]\n");
        }
        if o.primary {
            tables += "primary = true\n";
        }
        if !o.mirror.is_empty() {
            tables += &format!("mirror = {:?}\n", o.mirror);
        }
        tables += "\n";
    }
    let (mut out, mut skip, mut placed) = (String::new(), false, false);
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with('[') {
            skip = line.trim().strip_prefix("[[").and_then(|l| l.strip_suffix("]]")).is_some_and(|t| t.trim() == "display.outputs");
            if skip && !placed {
                out += &tables;
                placed = true;
            }
        }
        if !skip {
            out += line;
        }
    }
    if !placed {
        if !out.is_empty() && !out.ends_with("\n\n") {
            out += if out.ends_with('\n') { "\n" } else { "\n\n" };
        }
        out += tables.trim_end();
        out += "\n";
    }
    out
}

/// `$XDG_STATE_HOME/noobsbot-desktop/display-layouts.json`: every set of displays' remembered layout, by `layout_key`.
fn layouts_path() -> std::path::PathBuf {
    crate::config::xdg_dir("XDG_STATE_HOME", ".local/state").join("noobsbot-desktop/display-layouts.json")
}

impl State {
    /// Layouts per set of displays (E6.2): `connected` is a set not seen last time → its remembered layout goes into
    /// `[[display.outputs]]` (in memory and in the file); then the current entries are remembered for it.
    // ponytail: no 500 ms settle; a dock bringing up two monitors one after the other passes through the set with one.
    pub(crate) fn follow_layout(&mut self, connected: &[Display]) {
        let key = layout_key(connected);
        let path = layouts_path();
        let mut layouts: std::collections::HashMap<String, Saved> =
            std::fs::read_to_string(&path).ok().and_then(|json| serde_json::from_str(&json).ok()).unwrap_or_default();
        if key != self.layout_key {
            self.layout_key = key.clone();
            if let Some(saved) = layouts.get(&key) {
                let outputs = restore(saved, connected, &self.config.display.outputs);
                if outputs != self.config.display.outputs {
                    tracing::info!("displays: the layout last used with {key}");
                    crate::config::save_edit(|text| set_outputs(text, &outputs));
                    self.config.display.outputs = outputs;
                }
            }
        }
        let now = remember(&self.config.display.outputs, connected);
        if layouts.get(&key) != Some(&now) {
            layouts.insert(key, now);
            if let Ok(json) = serde_json::to_string_pretty(&layouts)
                && let Err(err) = crate::config::save_atomic(&path, &json)
            {
                tracing::warn!("{}: {err}", path.display());
            }
        }
    }

    /// Applies the display settings to every display, live: lights, darkens and modesets them (real screen), sets
    /// scale, rotation and position, puts the primary first (the shell draws on the first output), tells every
    /// window and panel its new scale, and rewrites `displays.json`. At start, on hotplug and on config reload.
    // ponytail: windows keep their logical size.
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
                plan(vec![nested], &self.config.display, false).into_iter().map(|d| (d, Some(output.clone()))).collect()
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
            // A mirror is drawn in its own pixels (scale 1) and gets no place, so no Spaces, bar, dock or windows.
            if !d.mirror.is_empty() {
                output.change_current_state(None, Some(base + rotation(d.rotation)), Some(Scale::Integer(1)), None);
                continue;
            }
            output.change_current_state(None, Some(base + rotation(d.rotation)), Some(Scale::Fractional(d.scale)), Some((d.x, d.y).into()));
            self.space.map_output(output, (d.x, d.y));
            layer_map_for_output(output).arrange();
        }
        // The pointer and windows of a display that went (unplugged, turned off) come onto the ones left.
        let screens: Vec<Rectangle<i32, Logical>> = self.space.outputs().filter_map(|o| self.space.output_geometry(o)).collect();
        if let Some(pointer) = self.seat.get_pointer() {
            let at = pointer.current_location();
            let kept = crate::backend::udev::clamp_pointer(&screens, at, at);
            if kept != at {
                pointer.set_location(kept);
            }
        }
        let lost: Vec<Window> = self
            .space
            .elements()
            .filter(|w| {
                let Some(mut frame) = self.space.element_geometry(w) else { return false };
                frame.loc.y -= self.window_row(w) * crate::spaces::STRIDE;
                stranded(frame, &screens)
            })
            .cloned()
            .collect();
        for window in lost {
            let (Some(frame), Some(area)) = (self.frame(&window), self.work_area()) else { continue };
            // Per display, a gone display's Spaces mean nothing on the first: its windows join the shown Space.
            let row = if self.config.spaces.per_display { 0 } else { self.window_row(&window) * crate::spaces::STRIDE };
            self.set_visible_loc(&window, crate::wm::place(area, frame.size) + smithay::utils::Point::from((0, row)));
        }
        self.refit_filled();
        self.send_scales();
        write_info(&shown.iter().map(|(d, _)| d.clone()).collect::<Vec<_>>());
        self.schedule_redraw();
    }

    /// Zoomed and fullscreen windows fill their display's work area again, at its new scale or size, or after the
    /// menu bar starts or stops hiding.
    pub fn refit_filled(&mut self) {
        let filled: Vec<(Window, ToplevelSurface, Output)> = self
            .space
            .elements()
            .filter_map(|w| {
                let toplevel = w.toplevel()?;
                let states = toplevel.current_state().states;
                let filled = states.contains(xdg_toplevel::State::Maximized) || states.contains(xdg_toplevel::State::Fullscreen);
                filled.then(|| Some((w.clone(), toplevel.clone(), self.window_output(w)?))).flatten()
            })
            .collect();
        for (window, toplevel, output) in filled {
            let Some(area) = self.output_area(&output) else { continue };
            let size = self.content_size(&window, area.size);
            toplevel.with_pending_state(|s| s.size = Some(size));
            toplevel.send_pending_configure();
            let row = self.window_row(&window) * crate::spaces::STRIDE;
            self.set_visible_loc(&window, area.loc + smithay::utils::Point::from((0, row)));
        }
    }

    /// The output `window` is mostly on (its hidden-Space row folded away); `None` when it is on no output.
    pub fn window_output(&self, window: &Window) -> Option<Output> {
        let mut frame = self.space.element_geometry(window)?;
        frame.loc.y -= self.window_row(window) * crate::spaces::STRIDE;
        mostly_on(frame, self.space.outputs().filter_map(|o| Some((self.space.output_geometry(o)?, o.clone()))))
    }

    /// Tells every window the scale of the output it is mostly on (the first output's when it is on none) and every
    /// panel its output's: after each frame, as windows move, and when displays change. Nothing is sent twice.
    pub fn send_scales(&self) {
        let tell = |scale: f64| {
            move |surface: &WlSurface, states: &SurfaceData| {
                with_fractional_scale(states, |fractional| fractional.set_preferred_scale(scale));
                send_surface_state(surface, states, scale.ceil() as i32, Transform::Normal);
            }
        };
        let first = self.space.outputs().next().cloned();
        for window in self.space.elements() {
            let scale = self.window_output(window).or_else(|| first.clone()).map_or(1.0, |o| o.current_scale().fractional_scale());
            window.with_surfaces(tell(scale));
        }
        for output in self.space.outputs() {
            let scale = output.current_scale().fractional_scale();
            for layer in layer_map_for_output(output).layers() {
                layer.with_surfaces(tell(scale));
            }
        }
    }
}

/// Of `outputs`, the one `frame` overlaps most, if any.
pub fn mostly_on<T>(frame: Rectangle<i32, Logical>, outputs: impl Iterator<Item = (Rectangle<i32, Logical>, T)>) -> Option<T> {
    outputs
        .filter_map(|(geo, output)| frame.intersection(geo).map(|i| (i64::from(i.size.w) * i64::from(i.size.h), output)))
        .filter(|(area, _)| *area > 0)
        .max_by_key(|(area, _)| *area)
        .map(|(_, output)| output)
}

/// Less than half of `frame` is on the displays: what a window left on an unplugged one keeps (a sliver over the
/// edge of the one beside it is not enough to grab it back).
pub fn stranded(frame: Rectangle<i32, Logical>, screens: &[Rectangle<i32, Logical>]) -> bool {
    let area = |r: Rectangle<i32, Logical>| i64::from(r.size.w) * i64::from(r.size.h);
    let seen: i64 = screens.iter().filter_map(|s| frame.intersection(*s)).map(area).sum();
    seen * 2 < area(frame)
}

/// The display (index into `screens`, the main one first) holding `wanted` (a text field's caret, a focused window),
/// else the one under `pointer`, else the main one.
pub fn display_for(screens: &[Rectangle<i32, Logical>], wanted: Option<Point<i32, Logical>>, pointer: Point<f64, Logical>) -> usize {
    wanted.and_then(|p| screens.iter().position(|s| s.contains(p))).or_else(|| screens.iter().position(|s| s.to_f64().contains(pointer))).unwrap_or(0)
}

impl State {
    /// `display_for` among the displays: the one holding `wanted`, else the pointer's, else the main one.
    pub fn display_with(&self, wanted: Option<Point<i32, Logical>>) -> Option<Output> {
        let outputs: Vec<Output> = self.space.outputs().cloned().collect();
        let screens: Vec<Rectangle<i32, Logical>> = outputs.iter().map(|o| self.space.output_geometry(o).unwrap_or_default()).collect();
        let pointer = self.seat.get_pointer().map(|p| p.current_location()).unwrap_or_default();
        outputs.get(display_for(&screens, wanted, pointer)).cloned()
    }

    /// Displays ▸ Identify: each display's name in its middle for `SHOWN_FOR` (again from the start if asked again).
    pub fn identify_displays(&mut self) {
        if let Some(token) = self.identify.take() {
            self.loop_handle.remove(token);
        }
        let timer = self.loop_handle.insert_source(Timer::from_duration(crate::render::identify::SHOWN_FOR), |_, _, state| {
            state.identify = None;
            state.schedule_redraw();
            TimeoutAction::Drop
        });
        self.identify = timer.inspect_err(|err| tracing::warn!("identify timer: {err}")).ok();
        self.schedule_redraw();
    }

    /// The settings file was written: with a `display.previous` set aside by System Settings, the `[display]` from
    /// before goes back in `KEEP_FOR` unless Settings deletes it first ("Keep Changes"). Here rather than in
    /// Settings, so a mode the user cannot see is undone even when Settings is gone by then.
    pub fn arm_display_revert(&mut self) {
        if let Some(token) = self.display_revert.take() {
            self.loop_handle.remove(token);
        }
        if !display_info::previous_path().is_some_and(|p| p.exists()) {
            return;
        }
        let timer = self.loop_handle.insert_source(Timer::from_duration(display_info::KEEP_FOR), |_, _, state| {
            state.display_revert = None;
            revert_displays();
            TimeoutAction::Drop
        });
        self.display_revert = timer.inspect_err(|err| tracing::warn!("display revert timer: {err}")).ok();
    }
}

/// Puts the `[display]` section in `display.previous` (if any) back into the settings file, which the live reload
/// then applies; the rest of the file stays as it is.
pub fn revert_displays() {
    let Some(path) = display_info::previous_path() else { return };
    let Ok(previous) = std::fs::read_to_string(&path) else { return };
    tracing::info!("display settings not kept: putting the previous ones back");
    if crate::config::save_edit(|text| crate::config::set_section(text, "display", &previous)) {
        let _ = std::fs::remove_file(path);
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
    fn the_keyboard_and_alerts_pick_their_display() {
        let screens = [Rectangle::new((0, 0).into(), (1920, 1080).into()), Rectangle::new((1920, 0).into(), (2560, 1440).into())];
        // The focused text field's (or window's) display, else the pointer's.
        assert_eq!(display_for(&screens, Some((2000, 300).into()), (10.0, 10.0).into()), 1);
        assert_eq!(display_for(&screens, None, (3000.0, 1200.0).into()), 1);
        assert_eq!(display_for(&screens, Some((9000, 0).into()), (10.0, 10.0).into()), 0, "a caret off every display");
        assert_eq!(display_for(&[], None, (0.0, 0.0).into()), 0);
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
        assert_eq!(pick_scale((2560, 1600), (286, 179), true), 1.75, "13.3-inch 2560 × 1600: looks like 1440 × 900");
        assert_eq!(pick_scale((2880, 1800), (301, 188), true), 2.0, "14-inch 2880 × 1800");
        assert_eq!(pick_scale((3840, 2160), (344, 194), true), 2.0, "15.6-inch 4K, capped at 200%");
        assert_eq!(pick_scale((1366, 768), (160, 90), true), 1.0, "never under 1280 × 720 logical");
        assert_eq!(pick_scale((1920, 1080), (16, 9), false), 1.0, "an aspect ratio, not a size");
        assert_eq!(pick_scale((1920, 1080), (0, 0), false), 1.0);
    }

    #[test]
    fn defaults_without_entries() {
        let p = plan(laptop_and_monitor(), &Displays::default(), false);
        assert!(p.iter().all(|d| d.enabled));
        assert_eq!((p[0].mode, p[0].scale, p[0].x, p[0].width, p[0].height), (mode(1920, 1200, 60, true), 1.25, 0, 1536, 960));
        assert_eq!((p[1].mode.refresh, p[1].scale, p[1].x, p[1].y, p[1].width), (144_000, 1.0, 1536, 0, 2560), "highest refresh, placed right");
        assert!(p[0].primary && !p[1].primary, "the first is primary");
        let global = Displays { scale: 2.0, ..Default::default() };
        assert!(plan(laptop_and_monitor(), &global, false).iter().all(|d| d.scale == 2.0));
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
        let p = plan(laptop_and_monitor(), &config, false);
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
        let p = plan(laptop_and_monitor(), &config, false);
        assert_eq!((p[0].mode.width, p[0].rotation, p[0].scale), (1920, 0, 3.0), "unknown mode: preferred; scale clamped");
        assert_eq!(p[1].mode.refresh, 144_000);
        let odd = Displays { scale: 1.3333, ..Default::default() };
        let p = plan(laptop_and_monitor(), &odd, false);
        assert_eq!((p[0].scale, p[0].width), (160.0 / 120.0, 1440), "rounded to a 1/120 step");
    }

    #[test]
    fn off_displays_and_the_last_one_on() {
        let one_off = Displays { outputs: vec![OutputSettings { enabled: false, ..output("eDP-1") }], ..Default::default() };
        let p = plan(laptop_and_monitor(), &one_off, false);
        assert!(!p[0].enabled && p[1].enabled && p[1].primary);
        assert_eq!(p[1].x, 0, "nothing left of it any more");
        let all_off = Displays {
            outputs: vec![OutputSettings { enabled: false, ..output("eDP-1") }, OutputSettings { enabled: false, ..output("HDMI-A-1") }],
            ..Default::default()
        };
        let p = plan(laptop_and_monitor(), &all_off, false);
        assert!(p[0].enabled && p[0].primary && !p[1].enabled, "the first stays on");
        let primary_off = Displays { outputs: vec![OutputSettings { enabled: false, primary: true, ..output("eDP-1") }], ..Default::default() };
        assert!(plan(laptop_and_monitor(), &primary_off, false)[1].primary, "an off display is never primary");
    }

    #[test]
    fn vrr_only_where_capable() {
        let mut displays = laptop_and_monitor();
        displays[1].vrr_capable = true;
        let both = Displays {
            outputs: vec![OutputSettings { vrr: true, ..output("eDP-1") }, OutputSettings { vrr: true, ..output("HDMI-A-1") }],
            ..Default::default()
        };
        let p = plan(displays, &both, false);
        assert!(!p[0].vrr && p[1].vrr);
    }

    #[test]
    fn clamshell_turns_the_panel_off_only_with_another_display_on() {
        let p = plan(laptop_and_monitor(), &Displays::default(), true);
        assert!(!p[0].enabled && p[1].enabled && p[1].primary, "lid closed, monitor on: panel off");
        assert_eq!(p[1].x, 0, "the monitor takes the panel's place");
        let p = plan(laptop_and_monitor(), &Displays::default(), false);
        assert!(p[0].enabled && p[1].enabled, "lid open: both");
        let laptop = plan(laptop_and_monitor()[..1].to_vec(), &Displays::default(), true);
        assert!(laptop[0].enabled, "nothing else: the panel stays on");
        let monitor_off = Displays { outputs: vec![OutputSettings { enabled: false, ..output("HDMI-A-1") }], ..Default::default() };
        let p = plan(laptop_and_monitor(), &monitor_off, true);
        assert!(p[0].enabled && !p[1].enabled, "the monitor turned off in Settings: the panel stays on");
    }

    #[test]
    fn a_mirror_is_left_out_of_the_arrangement() {
        let mirror = |name: &str, of: &str| OutputSettings { mirror: of.into(), ..output(name) };
        let config = Displays { outputs: vec![mirror("eDP-1", "HDMI-A-1"), OutputSettings { primary: true, ..output("eDP-1") }], ..Default::default() };
        let p = plan(laptop_and_monitor(), &config, false);
        assert_eq!(p[0].mirror, "HDMI-A-1");
        assert!(p[0].enabled && !p[0].primary && p[1].primary, "a mirror is never primary");
        assert_eq!(p[1].x, 0, "the source alone in the arrangement");
        // Gone, off, itself or unknown: extended.
        for bad in ["DP-9", "eDP-1", ""] {
            let p = plan(laptop_and_monitor(), &Displays { outputs: vec![mirror("eDP-1", bad)], ..Default::default() }, false);
            assert_eq!((p[0].mirror.as_str(), p[1].x), ("", 1536), "{bad:?}");
        }
        let off = Displays { outputs: vec![mirror("eDP-1", "HDMI-A-1"), OutputSettings { enabled: false, ..output("HDMI-A-1") }], ..Default::default() };
        let p = plan(laptop_and_monitor(), &off, false);
        assert!(p[0].mirror.is_empty() && p[0].primary, "its source turned off");
        // Lid closed: the panel goes off, so the monitor mirroring it takes over as the desktop.
        let p = plan(laptop_and_monitor(), &Displays { outputs: vec![mirror("HDMI-A-1", "eDP-1")], ..Default::default() }, true);
        assert!(!p[0].enabled && p[1].mirror.is_empty() && p[1].primary);
        // Each mirroring the other: neither (no chains).
        let both = Displays { outputs: vec![mirror("eDP-1", "HDMI-A-1"), mirror("HDMI-A-1", "eDP-1")], ..Default::default() };
        assert!(plan(laptop_and_monitor(), &both, false).iter().all(|d| d.mirror.is_empty()));
        let mut three = laptop_and_monitor();
        three.push(display("DP-1", vec![mode(1920, 1080, 60, true)], (531, 299)));
        let chain = Displays { outputs: vec![mirror("DP-1", "HDMI-A-1"), mirror("HDMI-A-1", "eDP-1")], ..Default::default() };
        let p = plan(three, &chain, false);
        assert_eq!((p[1].mirror.as_str(), p[2].mirror.as_str()), ("eDP-1", ""), "a chain's far end goes back to extended");
    }

    #[test]
    fn a_mirror_letterboxes_its_source() {
        let fit = |src: (i32, i32), dst: (i32, i32)| {
            let r = letterbox(src.into(), dst.into());
            (r.loc.x, r.loc.y, r.size.w, r.size.h)
        };
        assert_eq!(fit((1920, 1080), (3840, 2160)), (0, 0, 3840, 2160), "same shape: filled");
        assert_eq!(fit((1920, 1200), (1920, 1080)), (96, 0, 1728, 1080), "16:10 on 16:9: bars left and right");
        assert_eq!(fit((2560, 1080), (1920, 1080)), (0, 135, 1920, 810), "ultrawide on 16:9: bars above and below");
        assert_eq!(fit((1920, 1080), (1080, 1920)), (0, 656, 1080, 608), "onto a portrait display");
        assert_eq!(fit((0, 0), (1920, 1080)), (0, 0, 1920, 1080));
    }

    #[test]
    fn unplaced_go_right_of_placed() {
        let config = Displays { outputs: vec![OutputSettings { position: Some([-1536, 100]), ..output("eDP-1") }], ..Default::default() };
        let p = plan(laptop_and_monitor(), &config, false);
        assert_eq!((p[0].x, p[0].y, p[1].x), (-1536, 100, 0));
    }

    #[test]
    fn saved_positions_are_resnapped_after_a_size_change() {
        // Saved with the laptop at 125% (1536 wide); now at 100% it is 1920 wide and the monitor would overlap it.
        let outputs = |laptop_scale| {
            vec![
                OutputSettings { scale: laptop_scale, position: Some([0, 0]), ..output("eDP-1") },
                OutputSettings { position: Some([1536, 0]), ..output("HDMI-A-1") },
            ]
        };
        let p = plan(laptop_and_monitor(), &Displays { outputs: outputs(1.0), ..Default::default() }, false);
        assert_eq!((p[0].width, p[1].x, p[1].y), (1920, 1920, 0), "pushed out to the edge");
        // At 150% it is 1280 wide: the gap closes.
        let p = plan(laptop_and_monitor(), &Displays { outputs: outputs(1.5), ..Default::default() }, false);
        assert_eq!((p[0].width, p[1].x), (1280, 1280));
        // Still touching: nothing moves.
        let p = plan(laptop_and_monitor(), &Displays { outputs: outputs(1.25), ..Default::default() }, false);
        assert_eq!((p[0].x, p[0].y, p[1].x, p[1].y), (0, 0, 1536, 0));
    }

    #[test]
    fn a_window_belongs_to_the_output_it_overlaps_most() {
        let rect = |x, y, w, h| Rectangle::<i32, Logical>::new((x, y).into(), (w, h).into());
        let outputs = || [(rect(0, 0, 1536, 960), 1.25), (rect(1536, 0, 2560, 1440), 1.0)].into_iter();
        assert_eq!(mostly_on(rect(100, 100, 800, 600), outputs()), Some(1.25));
        assert_eq!(mostly_on(rect(1400, 100, 800, 600), outputs()), Some(1.0), "more of it on the monitor");
        assert_eq!(mostly_on(rect(1536, 0, 800, 600), outputs()), Some(1.0), "touching the laptop's edge is not on it");
        assert_eq!(mostly_on(rect(5000, 5000, 800, 600), outputs()), None, "on no output");
        // window_area's choice: a display left of the main one has a negative origin.
        let left = || [(rect(0, 0, 1920, 1080), "main"), (rect(-1920, 0, 1920, 1080), "left")].into_iter();
        assert_eq!(mostly_on(rect(-1000, 200, 800, 600), left()), Some("left"));
        assert_eq!(mostly_on(rect(-300, 200, 800, 600), left()), Some("main"), "more of it on the main one");
    }

    #[test]
    fn a_window_mostly_off_every_display_is_stranded() {
        let rect = |x, y, w, h| Rectangle::<i32, Logical>::new((x, y).into(), (w, h).into());
        let laptop = [rect(0, 0, 1920, 1080)];
        // Left on the unplugged monitor to the right, 40 px over the laptop's edge (seen in the VM).
        assert!(stranded(rect(1880, 80, 1400, 900), &laptop));
        assert!(stranded(rect(5000, 0, 800, 600), &laptop), "on no display");
        assert!(!stranded(rect(1400, 80, 800, 600), &laptop), "more than half still on it");
        assert!(!stranded(rect(1400, 80, 800, 600), &[laptop[0], rect(1920, 0, 1280, 720)]), "the rest on the monitor");
    }

    #[test]
    fn layouts_are_kept_per_set_of_displays() {
        let named = |name: &str, model: &str, serial: &str| Display {
            name: name.into(),
            make: "DEL".into(),
            model: model.into(),
            serial: serial.into(),
            ..Default::default()
        };
        let laptop = Display { name: "eDP-1".into(), make: "BOE".into(), model: "".into(), ..Default::default() };
        let desk = [laptop.clone(), named("HDMI-A-1", "U2720Q", "ABC")];
        assert_eq!(layout_key(&desk), "BOE + DEL U2720Q ABC");
        assert_eq!(layout_key(&[desk[1].clone(), laptop.clone()]), layout_key(&desk), "order does not matter");
        assert_eq!(layout_key(&[Display { name: "DP-1".into(), ..Default::default() }]), "DP-1", "no EDID: the connector");
        // Remembered at the desk, the monitor then comes back on another port.
        let outputs =
            vec![OutputSettings { position: Some([-2560, 0]), primary: true, ..output("HDMI-A-1") }, OutputSettings { scale: 1.5, ..output("eDP-1") }];
        let saved = remember(&outputs, &desk);
        assert_eq!(saved.len(), 2);
        let moved = [laptop.clone(), named("DP-2", "U2720Q", "ABC")];
        assert_eq!(layout_key(&moved), layout_key(&desk));
        let stale = vec![OutputSettings { scale: 2.0, ..output("eDP-1") }, output("DP-2"), OutputSettings { enabled: false, ..output("DP-9") }];
        let back = restore(&saved, &moved, &stale);
        assert_eq!(back.len(), 3);
        assert_eq!(back[0].name, "DP-9", "other connectors' entries stay");
        let dp = back.iter().find(|o| o.name == "DP-2").unwrap();
        assert!(dp.primary && dp.position == Some([-2560, 0]));
        assert_eq!(back.iter().find(|o| o.name == "eDP-1").unwrap().scale, 1.5);
        // Planned with the restored entries: the monitor left of the laptop, main.
        let p = plan(laptop_and_monitor(), &Displays { outputs: restore(&saved, &desk, &[]), ..Default::default() }, false);
        assert!(p[1].primary && p[1].x < p[0].x);
    }

    #[test]
    fn outputs_are_rewritten_in_place() {
        let text = "[display]\n# mine\nscale = 0.0\n\n[[display.outputs]]\nname = \"eDP-1\"\nscale = 2.0\n\n[windows]\nspaces = 0\n";
        let outputs =
            [OutputSettings { position: Some([0, 0]), refresh: 59.95, primary: true, ..output("eDP-1") }, OutputSettings { enabled: false, ..output("DP-1") }];
        let new = set_outputs(text, &outputs);
        assert_eq!(
            new,
            "[display]\n# mine\nscale = 0.0\n\n[[display.outputs]]\nname = \"eDP-1\"\nrefresh = 59.95\nposition = [0, 0]\nprimary = true\n\n\
             [[display.outputs]]\nname = \"DP-1\"\nenabled = false\n\n[windows]\nspaces = 0\n"
        );
        let parsed: crate::config::Config = toml::from_str(&new).unwrap();
        assert_eq!(parsed.display.outputs, outputs);
        assert_eq!(set_outputs("[display]\nscale = 0.0\n", &outputs[1..]), "[display]\nscale = 0.0\n\n[[display.outputs]]\nname = \"DP-1\"\nenabled = false\n");
        assert_eq!(set_outputs(text, &[]), "[display]\n# mine\nscale = 0.0\n\n[windows]\nspaces = 0\n");
        let mirrored = [OutputSettings { mirror: "eDP-1".into(), ..output("HDMI-A-1") }];
        let new = set_outputs("", &mirrored);
        assert_eq!(new, "[[display.outputs]]\nname = \"HDMI-A-1\"\nmirror = \"eDP-1\"\n");
        assert_eq!(toml::from_str::<crate::config::Config>(&new).unwrap().display.outputs, mirrored);
    }

    #[test]
    fn remembered_layouts_keep_the_mirror() {
        let desk = laptop_and_monitor();
        let saved = remember(&[OutputSettings { mirror: "eDP-1".into(), ..output("HDMI-A-1") }], &desk);
        let json = serde_json::to_string(&saved).unwrap();
        let back: Saved = serde_json::from_str(&json).unwrap();
        assert_eq!(restore(&back, &desk, &[])[0].mirror, "eDP-1");
        // Saved before mirroring existed: extended.
        let old: Saved = serde_json::from_str(r#"[["HDMI-A-1", {"name": "HDMI-A-1", "primary": true}]]"#).unwrap();
        assert_eq!(old[0].1.mirror, "");
    }

    #[test]
    fn edid_serial_string_else_number() {
        let mut edid = vec![0u8; 128];
        edid[..8].copy_from_slice(&[0, 255, 255, 255, 255, 255, 255, 0]);
        assert_eq!(edid_serial(&edid), "");
        edid[12..16].copy_from_slice(&1234u32.to_le_bytes());
        assert_eq!(edid_serial(&edid), "1234");
        edid[90..95].copy_from_slice(&[0, 0, 0, 0xFF, 0]);
        edid[95..108].copy_from_slice(b"7MT0123AB\n   ");
        assert_eq!(edid_serial(&edid), "7MT0123AB");
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
