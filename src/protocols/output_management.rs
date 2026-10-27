//! `zwlr_output_manager_v1` (E6.2, E6.11): every connected display as a head (modes, on/off, position, scale,
//! rotation, adaptive sync) for wlr-randr, kanshi and the like, and their changes applied the way System Settings applies its own:
//! written to `[[display.outputs]]` in config.toml, then `apply_displays`. Unsandboxed clients only (perms.rs).
// ponytail: every change sends all heads anew (old ones finished); custom modes, flipped transforms and adaptive sync
// on a display without `vrr_capable` are refused. Positions go through `displays::plan`, so overlapping ones are snapped edge to edge.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use smithay::reexports::{
    wayland_protocols_wlr::output_management::v1::server::{
        zwlr_output_configuration_head_v1::{self, ZwlrOutputConfigurationHeadV1},
        zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
        zwlr_output_head_v1::{AdaptiveSyncState, ZwlrOutputHeadV1},
        zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
        zwlr_output_mode_v1::ZwlrOutputModeV1,
    },
    wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        backend::{ClientId, GlobalId},
        protocol::wl_output::Transform,
    },
};

use crate::{
    config::OutputSettings,
    display_info::{Display, Mode},
    state::State,
};

/// A head sent to a manager, with its modes.
type Head = (ZwlrOutputHeadV1, Vec<ZwlrOutputModeV1>);
/// A head named in a configuration and its change (`None`: turned off).
type Named = (String, Option<Arc<Mutex<Change>>>);

pub struct OutputManagement {
    _global: GlobalId,
    /// Each bound manager and the heads (with their modes) it was last sent.
    managers: Vec<(ZwlrOutputManagerV1, Vec<Head>)>,
    displays: Vec<Display>,
    serial: u32,
}

impl OutputManagement {
    pub fn new(dh: &DisplayHandle) -> Self {
        Self { _global: dh.create_global::<State, ZwlrOutputManagerV1, _>(4, ()), managers: Vec::new(), displays: Vec::new(), serial: 0 }
    }
}

/// What a configuration asks of one head: `None` fields stay as they are.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Change {
    mode: Option<Mode>,
    position: Option<[i32; 2]>,
    rotation: Option<u16>,
    scale: Option<f64>,
    vrr: Option<bool>,
    /// A custom mode, a flipped transform or a mode of another head: the configuration fails.
    refused: bool,
}

/// A configuration being built: per head named, its change (`None`: turned off).
pub struct Configuration {
    serial: u32,
    heads: Mutex<Vec<Named>>,
    used: AtomicBool,
}

/// A rotation in degrees clockwise as a `wl_output` transform.
fn transform(degrees: u16) -> Transform {
    match degrees {
        90 => Transform::_90,
        180 => Transform::_180,
        270 => Transform::_270,
        _ => Transform::Normal,
    }
}

/// The rotation of a transform; `None` for the flipped ones (not offered by Settings either).
fn rotation(transform: Transform) -> Option<u16> {
    match transform {
        Transform::Normal => Some(0),
        Transform::_90 => Some(90),
        Transform::_180 => Some(180),
        Transform::_270 => Some(270),
        _ => None,
    }
}

/// `outputs` (the `[[display.outputs]]` entries) with `changes` made; `None` if one is refused or names no
/// connected display. Entries for other displays stay.
pub fn settings(displays: &[Display], outputs: &[OutputSettings], changes: &[(String, Option<Change>)]) -> Option<Vec<OutputSettings>> {
    let mut outputs = outputs.to_vec();
    for (name, change) in changes {
        let display = displays.iter().find(|d| d.name == *name)?;
        let i = outputs.iter().position(|o| o.name == *name).unwrap_or_else(|| {
            outputs.push(OutputSettings { name: name.clone(), ..Default::default() });
            outputs.len() - 1
        });
        let entry = &mut outputs[i];
        entry.enabled = change.is_some();
        let Some(change) = change else { continue };
        if change.refused {
            return None;
        }
        if let Some(m) = change.mode {
            if !display.modes.contains(&m) {
                return None;
            }
            entry.mode = format!("{}x{}", m.width, m.height);
            entry.refresh = f64::from(m.refresh) / 1000.0;
        }
        if let Some(scale) = change.scale {
            if !(0.5..=4.0).contains(&scale) {
                return None;
            }
            entry.scale = scale;
        }
        if let Some(vrr) = change.vrr {
            if vrr && !display.vrr_capable {
                return None;
            }
            entry.vrr = vrr;
        }
        entry.rotation = change.rotation.unwrap_or(entry.rotation);
        entry.position = change.position.or(entry.position);
    }
    Some(outputs)
}

impl State {
    /// The displays `apply_displays` left last time.
    pub fn last_displays(&self) -> &[Display] {
        &self.protocols.output_management.displays
    }

    /// The displays as `apply_displays` left them: every manager is sent them when they changed.
    pub fn output_heads(&mut self, displays: Vec<Display>) {
        let om = &mut self.protocols.output_management;
        if om.displays == displays {
            return;
        }
        om.displays = displays;
        om.serial = om.serial.wrapping_add(1);
        om.managers.retain(|(m, _)| m.is_alive());
        let dh = self.display_handle.clone();
        for (manager, heads) in &mut om.managers {
            for (head, modes) in heads.drain(..) {
                modes.iter().for_each(ZwlrOutputModeV1::finished);
                head.finished();
            }
            *heads = send_heads(&dh, manager, &om.displays, om.serial);
        }
    }

    /// Makes a configuration's changes; `false` if it can't be.
    fn configure(&mut self, changes: &[(String, Option<Change>)], apply: bool) -> bool {
        let om = &self.protocols.output_management;
        let Some(outputs) = settings(&om.displays, &self.config.display.outputs, changes) else { return false };
        if apply && outputs != self.config.display.outputs {
            crate::config::save_edit(|text| crate::displays::set_outputs(text, &outputs));
            self.config.display.outputs = outputs;
            self.apply_displays();
        }
        true
    }
}

/// Sends `displays` to `manager` as fresh heads, then `done`.
fn send_heads(dh: &DisplayHandle, manager: &ZwlrOutputManagerV1, displays: &[Display], serial: u32) -> Vec<Head> {
    let Some(client) = manager.client() else { return Vec::new() };
    let mut sent = Vec::new();
    for d in displays {
        let Ok(head) = client.create_resource::<ZwlrOutputHeadV1, _, State>(dh, manager.version(), d.name.clone()) else { continue };
        manager.head(&head);
        head.name(d.name.clone());
        let what = crate::display_info::maker(&d.make, &d.model);
        head.description(if what.is_empty() { d.name.clone() } else { format!("{what} ({})", d.name) });
        if d.width_mm > 0 && d.height_mm > 0 {
            head.physical_size(d.width_mm as i32, d.height_mm as i32);
        }
        let mut modes = Vec::new();
        for m in &d.modes {
            let Ok(mode) = client.create_resource::<ZwlrOutputModeV1, _, State>(dh, manager.version(), (d.name.clone(), *m)) else { continue };
            head.mode(&mode);
            mode.size(m.width, m.height);
            mode.refresh(m.refresh);
            if m.preferred {
                mode.preferred();
            }
            if d.enabled && *m == d.mode {
                head.current_mode(&mode);
            }
            modes.push(mode);
        }
        head.enabled(i32::from(d.enabled));
        if d.enabled {
            head.position(d.x, d.y);
            head.transform(transform(d.rotation));
            head.scale(d.scale);
        }
        if head.version() >= 2 {
            [(&d.make, ZwlrOutputHeadV1::make as fn(&_, String)), (&d.model, ZwlrOutputHeadV1::model), (&d.serial, ZwlrOutputHeadV1::serial_number)]
                .into_iter()
                .filter(|(v, _)| !v.is_empty())
                .for_each(|(v, send)| send(&head, v.clone()));
        }
        if head.version() >= 4 {
            head.adaptive_sync(if d.vrr { AdaptiveSyncState::Enabled } else { AdaptiveSyncState::Disabled });
        }
        sent.push((head, modes));
    }
    manager.done(serial);
    sent
}

impl GlobalDispatch<ZwlrOutputManagerV1, ()> for State {
    fn bind(state: &mut Self, dh: &DisplayHandle, _: &Client, resource: New<ZwlrOutputManagerV1>, _: &(), init: &mut DataInit<'_, Self>) {
        let manager = init.init(resource, ());
        let om = &mut state.protocols.output_management;
        let heads = send_heads(dh, &manager, &om.displays, om.serial);
        om.managers.push((manager, heads));
    }

    fn can_view(client: Client, _: &()) -> bool {
        super::unsandboxed(&client)
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        manager: &ZwlrOutputManagerV1,
        request: zwlr_output_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwlr_output_manager_v1::Request::CreateConfiguration { id, serial } => {
                init.init(id, Configuration { serial, heads: Mutex::default(), used: AtomicBool::new(false) });
            }
            zwlr_output_manager_v1::Request::Stop => {
                state.protocols.output_management.managers.retain(|(m, _)| m != manager);
                manager.finished();
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, manager: &ZwlrOutputManagerV1, _: &()) {
        state.protocols.output_management.managers.retain(|(m, _)| m != manager);
    }
}

impl Dispatch<ZwlrOutputHeadV1, String> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ZwlrOutputHeadV1,
        _: <ZwlrOutputHeadV1 as Resource>::Request,
        _: &String,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

impl Dispatch<ZwlrOutputModeV1, (String, Mode)> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ZwlrOutputModeV1,
        _: <ZwlrOutputModeV1 as Resource>::Request,
        _: &(String, Mode),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, Configuration> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        resource: &ZwlrOutputConfigurationV1,
        request: zwlr_output_configuration_v1::Request,
        data: &Configuration,
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_output_configuration_v1::{Error, Request};
        let named = |head: &ZwlrOutputHeadV1| head.data::<String>().cloned().unwrap_or_default();
        match request {
            Request::EnableHead { id, head } => {
                let change = Arc::new(Mutex::new(Change::default()));
                init.init(id, change.clone());
                data.heads.lock().unwrap().push((named(&head), Some(change)));
            }
            Request::DisableHead { head } => data.heads.lock().unwrap().push((named(&head), None)),
            Request::Apply | Request::Test => {
                if data.used.swap(true, Ordering::Relaxed) {
                    resource.post_error(Error::AlreadyUsed, "configuration already applied or tested");
                    return;
                }
                if data.serial != state.protocols.output_management.serial {
                    resource.cancelled();
                    return;
                }
                let changes: Vec<(String, Option<Change>)> =
                    data.heads.lock().unwrap().iter().map(|(n, c)| (n.clone(), c.as_ref().map(|c| c.lock().unwrap().clone()))).collect();
                if state.configure(&changes, matches!(request, Request::Apply)) { resource.succeeded() } else { resource.failed() }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrOutputConfigurationHeadV1, Arc<Mutex<Change>>> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ZwlrOutputConfigurationHeadV1,
        request: zwlr_output_configuration_head_v1::Request,
        data: &Arc<Mutex<Change>>,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        use zwlr_output_configuration_head_v1::Request;
        let mut change = data.lock().unwrap();
        match request {
            Request::SetMode { mode } => match mode.data::<(String, Mode)>() {
                Some((_, m)) => change.mode = Some(*m),
                None => change.refused = true,
            },
            Request::SetPosition { x, y } => change.position = Some([x, y]),
            Request::SetScale { scale } => change.scale = Some(scale),
            Request::SetTransform { transform } => match transform.into_result().ok().and_then(rotation) {
                Some(r) => change.rotation = Some(r),
                None => change.refused = true,
            },
            Request::SetAdaptiveSync { state } => match state.into_result() {
                Ok(s) => change.vrr = Some(s == AdaptiveSyncState::Enabled),
                Err(_) => change.refused = true,
            },
            // Custom modes and anything newer.
            _ => change.refused = true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotations_map_both_ways_and_flips_are_refused() {
        for degrees in [0, 90, 180, 270] {
            assert_eq!(rotation(transform(degrees)), Some(degrees));
        }
        assert_eq!(rotation(Transform::Flipped90), None);
    }

    #[test]
    fn changes_become_output_entries() {
        let mode = |width, height, refresh| Mode { width, height, refresh, preferred: false };
        let d = |name: &str| Display { name: name.into(), modes: vec![mode(1920, 1080, 60000), mode(1280, 720, 59940)], ..Default::default() };
        let displays = [d("eDP-1"), d("HDMI-A-1")];
        let kept = OutputSettings { name: "DP-9".into(), scale: 2.0, ..Default::default() };
        let change = Change { mode: Some(mode(1280, 720, 59940)), position: Some([1920, 0]), rotation: Some(90), scale: Some(1.5), vrr: None, refused: false };
        let out = settings(&displays, std::slice::from_ref(&kept), &[("HDMI-A-1".into(), Some(change)), ("eDP-1".into(), None)]).unwrap();
        assert_eq!(out[0], kept, "other displays' entries stay");
        assert_eq!(
            out[1],
            OutputSettings {
                name: "HDMI-A-1".into(),
                mode: "1280x720".into(),
                refresh: 59.94,
                scale: 1.5,
                rotation: 90,
                position: Some([1920, 0]),
                ..Default::default()
            }
        );
        assert!(!out[2].enabled);
        let other = Change { mode: Some(mode(3840, 2160, 60000)), ..Default::default() };
        assert_eq!(settings(&displays, &[], &[("eDP-1".into(), Some(other))]), None, "a mode the display hasn't");
        assert_eq!(settings(&displays, &[], &[("eDP-1".into(), Some(Change { refused: true, ..Default::default() }))]), None);
        assert_eq!(settings(&displays, &[], &[("gone".into(), None)]), None);
        // Adaptive sync: on and off where capable, only off where not.
        let vrr = |on| Some(Change { vrr: Some(on), ..Default::default() });
        let capable = [Display { vrr_capable: true, ..d("DP-1") }];
        let on = settings(&capable, &[], &[("DP-1".into(), vrr(true))]).unwrap();
        assert!(on[0].vrr);
        assert!(!settings(&capable, &on, &[("DP-1".into(), vrr(false))]).unwrap()[0].vrr);
        assert_eq!(settings(&displays, &[], &[("eDP-1".into(), vrr(true))]), None, "a display without vrr_capable");
        assert!(!settings(&displays, &[], &[("eDP-1".into(), vrr(false))]).unwrap()[0].vrr);
    }
}
