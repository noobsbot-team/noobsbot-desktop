//! The rest of the E1.1/E1.7 protocols: layer shell, presentation time, pointer constraints, shortcut inhibiting,
//! cursor shapes, fractional scale, tearing hints, security contexts, pointer gestures and text input (the input
//! method behind it is E6.12, see ime.rs).

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use smithay::{
    delegate_cursor_shape, delegate_fractional_scale, delegate_keyboard_shortcuts_inhibit, delegate_layer_shell, delegate_pointer_constraints,
    delegate_pointer_gestures, delegate_presentation, delegate_relative_pointer, delegate_security_context, delegate_virtual_keyboard_manager,
    desktop::{LayerSurface, WindowSurfaceType, layer_map_for_output},
    input::pointer::{CursorIcon, CursorImageStatus, MotionEvent, PointerHandle, RelativeMotionEvent},
    output::Output,
    reexports::{
        wayland_protocols::wp::tearing_control::v1::server::{
            wp_tearing_control_manager_v1::{self, WpTearingControlManagerV1},
            wp_tearing_control_v1::{self, WpTearingControlV1},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
            backend::{ClientId, GlobalId},
            protocol::{wl_output::WlOutput, wl_surface::WlSurface},
        },
    },
    utils::{Clock, IsAlive, Logical, Monotonic, Point, SERIAL_COUNTER},
    wayland::{
        compositor::{RegionAttributes, with_states},
        cursor_shape::CursorShapeManagerState,
        fractional_scale::{FractionalScaleHandler, FractionalScaleManagerState, with_fractional_scale},
        input_method::InputMethodManagerState,
        keyboard_shortcuts_inhibit::{
            KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState, KeyboardShortcutsInhibitor, KeyboardShortcutsInhibitorSeat,
        },
        pointer_constraints::{PointerConstraint, PointerConstraintsHandler, PointerConstraintsState, with_pointer_constraint},
        pointer_gestures::PointerGesturesState,
        presentation::PresentationState,
        relative_pointer::RelativePointerManagerState,
        security_context::{SecurityContext, SecurityContextHandler, SecurityContextListenerSource, SecurityContextState},
        shell::wlr_layer::{KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler, WlrLayerShellState},
        tablet_manager::TabletSeatHandler,
        text_input::TextInputManagerState,
        virtual_keyboard::VirtualKeyboardManagerState,
    },
};

use crate::state::{ClientState, State};

mod toplevel_list;

pub struct Protocols {
    pub layer_shell: WlrLayerShellState,
    pub shortcuts_inhibit: KeyboardShortcutsInhibitState,
    /// The clock presentation feedback is stamped with.
    pub clock: Clock<Monotonic>,
    /// What apps asked the cursor to look like (wl_pointer.set_cursor or cursor-shape).
    pub cursor: CursorImageStatus,
    /// Where the host cursor was last; relative motion is the step from there, even while the pointer is locked.
    pub last_host_pointer: Option<Point<f64, Logical>>,
    _presentation: PresentationState,
    _fractional_scale: FractionalScaleManagerState,
    _relative_pointer: RelativePointerManagerState,
    _pointer_constraints: PointerConstraintsState,
    _pointer_gestures: PointerGesturesState,
    _cursor_shape: CursorShapeManagerState,
    _security_context: SecurityContextState,
    _tearing_control: GlobalId,
    _text_input: TextInputManagerState,
    _input_method: InputMethodManagerState,
    _system_bell: smithay::wayland::xdg_system_bell::XdgSystemBellState,
    _virtual_keyboard: VirtualKeyboardManagerState,
    toplevel_list: toplevel_list::ToplevelList,
}

impl Protocols {
    pub fn new(dh: &DisplayHandle) -> Self {
        let clock = Clock::<Monotonic>::new();
        Self {
            // A sandboxed app may not draw over the desktop.
            layer_shell: WlrLayerShellState::new_with_filter::<State, _>(dh, unsandboxed),
            shortcuts_inhibit: KeyboardShortcutsInhibitState::new::<State>(dh),
            _presentation: PresentationState::new::<State>(dh, clock.id() as u32),
            clock,
            cursor: CursorImageStatus::default_named(),
            last_host_pointer: None,
            _fractional_scale: FractionalScaleManagerState::new::<State>(dh),
            _relative_pointer: RelativePointerManagerState::new::<State>(dh),
            _pointer_constraints: PointerConstraintsState::new::<State>(dh),
            _pointer_gestures: PointerGesturesState::new::<State>(dh),
            _cursor_shape: CursorShapeManagerState::new::<State>(dh),
            // The protocol requires that sandboxed clients cannot nest contexts.
            _security_context: SecurityContextState::new::<State, _>(dh, unsandboxed),
            _tearing_control: dh.create_global::<State, WpTearingControlManagerV1, _>(1, ()),
            _text_input: TextInputManagerState::new::<State>(dh),
            // An input method sees every key typed: not for sandboxed apps.
            _input_method: InputMethodManagerState::new::<State, _>(dh, unsandboxed),
            _system_bell: smithay::wayland::xdg_system_bell::XdgSystemBellState::new::<State>(dh),
            // A virtual keyboard types into any app (and the lock screen): trusted clients only.
            _virtual_keyboard: VirtualKeyboardManagerState::new::<State, _>(dh, crate::perms::trusted),
            toplevel_list: toplevel_list::ToplevelList::new(dh),
        }
    }
}

/// True unless the client connected through a security context (Flatpak and other sandboxes).
/// Who sees which privileged global: perms.rs.
pub fn unsandboxed(client: &Client) -> bool {
    client.get_data::<ClientState>().is_none_or(|data| data.security_context.is_none())
}

impl State {
    /// Moves the pointer to where the host cursor is, honouring pointer locks and confinement, and sends the step as
    /// relative motion (games, maps). ponytail: nested, the host cursor still roams freely; udev feeds raw deltas (M6).
    pub fn pointer_motion(&mut self, host: Point<f64, Logical>, utime: u64) {
        let Some(pointer) = self.seat.get_pointer() else { return };
        let delta = self.protocols.last_host_pointer.replace(host).map_or_else(Point::default, |last| host - last);
        let current = pointer.current_location();
        self.assist_motion(host, delta);
        if self.locked() {
            self.lock_motion(self.to_output(host));
        }
        // The screenshot overlay keeps the pointer from every app (and its cursor).
        if self.shot_motion(self.to_output(host)) {
            pointer.motion(self, None, &MotionEvent { location: host, serial: SERIAL_COUNTER.next_serial(), time: (utime / 1000) as u32 });
            pointer.frame(self);
            return;
        }
        self.switcher_motion(host);
        self.hover_focus(host);
        self.shell_motion(self.to_output(current), self.to_output(host));
        self.dock_follow(self.to_output(host));
        self.dock_motion(self.to_dock(self.to_output(current)), self.to_dock(self.to_output(host)));
        self.desktop_motion(self.to_output(host));
        self.mission_motion(host);
        self.hot_corner_motion(host);
        self.bar_motion(host);
        let under = self.surface_under(current);

        let (mut locked, mut confined, mut region) = (false, false, None::<RegionAttributes>);
        if let Some((surface, origin)) = &under {
            with_pointer_constraint(surface, &pointer, |constraint| {
                let Some(constraint) = constraint.filter(|c| c.is_active()) else { return };
                // A constraint only holds while the pointer is inside its region.
                if constraint.region().is_some_and(|r| !r.contains((current - *origin).to_i32_round())) {
                    return;
                }
                match &*constraint {
                    PointerConstraint::Locked(_) => locked = true,
                    PointerConstraint::Confined(confine) => (confined, region) = (true, confine.region().cloned()),
                }
            });
        }

        pointer.relative_motion(self, under.clone(), &RelativeMotionEvent { delta, delta_unaccel: delta, utime });
        if locked {
            pointer.frame(self);
            return;
        }
        // A resize zone takes the pointer from whatever app is underneath it.
        let zone = if pointer.is_grabbed() { None } else { self.zone_under(host) };
        let new_under = if zone.is_some() { None } else { self.surface_under(host) };
        if let (true, Some((surface, origin))) = (confined, &under) {
            let left = new_under.as_ref().is_none_or(|(s, _)| s != surface) || region.is_some_and(|r| !r.contains((host - *origin).to_i32_round()));
            if left {
                pointer.frame(self);
                return;
            }
        }

        // Our cursor in a zone; entering an app or the bare desktop resets to the arrow, then an app sets its own.
        let hover = zone.as_ref().map(|(_, edges)| *edges);
        if !pointer.is_grabbed() && (hover != self.zones.hover || pointer.current_focus() != new_under.as_ref().map(|(s, _)| s.clone())) {
            self.zones.hover = hover;
            self.set_cursor(hover.map_or_else(CursorImageStatus::default_named, |e| CursorImageStatus::Named(crate::input::resize_icon(e))));
        }
        if !pointer.is_grabbed() {
            let lights = zone.is_none().then(|| self.bar_under(host)).flatten().filter(|(_, p)| *p != crate::decorations::Part::Title).map(|(w, _)| w);
            self.set_lights_hover(lights);
        }
        pointer.motion(self, new_under.clone(), &MotionEvent { location: host, serial: SERIAL_COUNTER.next_serial(), time: (utime / 1000) as u32 });
        pointer.frame(self);

        // A waiting constraint takes hold once the pointer is inside its region.
        if let Some((surface, origin)) = new_under {
            with_pointer_constraint(&surface, &pointer, |constraint| {
                if let Some(constraint) = constraint.filter(|c| !c.is_active())
                    && constraint.region().is_none_or(|r| r.contains((host - origin).to_i32_round()))
                {
                    constraint.activate();
                }
            });
        }
    }

    /// Stores the cursor an app asked for and, nested, shows it on the host window.
    pub fn set_cursor(&mut self, image: CursorImageStatus) {
        if let Some(crate::backend::Device::Winit(winit)) = &self.device {
            let window = winit.backend.window();
            window.set_cursor_visible(image != CursorImageStatus::Hidden);
            match &image {
                CursorImageStatus::Named(icon) => window.set_cursor(*icon),
                // ponytail: an app-drawn cursor surface shows as the arrow nested; we draw it ourselves with udev (M6).
                CursorImageStatus::Surface(_) => window.set_cursor(CursorIcon::Default),
                CursorImageStatus::Hidden => {}
            }
        }
        self.protocols.cursor = image;
    }

    /// The topmost layer surface taking input at `location` among top/overlay (`above`) or bottom/background layers,
    /// with the (sub)surface hit and its origin. Click-through parts (an empty input region) are skipped.
    pub fn layer_under(&self, location: Point<f64, Logical>, above: bool) -> Option<(LayerSurface, WlSurface, Point<f64, Logical>)> {
        let layers: [Layer; 2] = if above { [Layer::Overlay, Layer::Top] } else { [Layer::Bottom, Layer::Background] };
        self.space.outputs().find_map(|output| {
            let origin = self.space.output_geometry(output)?.loc;
            let map = layer_map_for_output(output);
            layers.into_iter().find_map(|layer| {
                map.layers_on(layer).rev().find_map(|l| {
                    let loc = (origin + map.layer_geometry(l)?.loc).to_f64();
                    let (surface, offset) = l.surface_under(location - loc, WindowSurfaceType::ALL)?;
                    Some((l.clone(), surface, offset.to_f64() + loc))
                })
            })
        })
    }

    /// A top or overlay layer surface that asked for the keyboard exclusively (a launcher, a lock screen).
    pub fn exclusive_layer(&self) -> Option<LayerSurface> {
        self.space.outputs().find_map(|output| {
            let map = layer_map_for_output(output);
            [Layer::Overlay, Layer::Top].into_iter().find_map(|layer| {
                map.layers_on(layer).rev().find(|l| l.alive() && l.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive).cloned()
            })
        })
    }

    /// Lays out a committed layer surface, sends its first configure, and hands it the keyboard if it wants it exclusively.
    pub fn layer_commit(&mut self, surface: &WlSurface) {
        let output = self.space.outputs().find(|o| layer_map_for_output(o).layer_for_surface(surface, WindowSurfaceType::TOPLEVEL).is_some()).cloned();
        let Some(output) = output else { return };
        let layer = {
            let mut map = layer_map_for_output(&output);
            map.arrange();
            map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL).cloned()
        };
        let Some(layer) = layer else { return };
        let sent = with_states(surface, |states| states.data_map.get::<LayerSurfaceData>().is_some_and(|d| d.lock().is_ok_and(|d| d.initial_configure_sent)));
        if !sent {
            layer.layer_surface().send_configure();
        }
        let Some(keyboard) = self.seat.get_keyboard() else { return };
        if !self.locked() && self.exclusive_layer().as_ref() == Some(&layer) && keyboard.current_focus().as_ref() != Some(surface) {
            keyboard.set_focus(self, Some(surface.clone()), SERIAL_COUNTER.next_serial());
        }
    }

    /// Whether the focused app holds an active shortcut inhibitor (VMs, remote desktop): then our shortcuts go to it.
    pub fn shortcuts_inhibited(&self) -> bool {
        let focus = self.seat.get_keyboard().and_then(|k| k.current_focus());
        focus.is_some_and(|surface| self.seat.keyboard_shortcuts_inhibitor_for_surface(&surface).is_some_and(|i| i.is_active()))
    }

    fn output_for(&self, output: Option<&WlOutput>) -> Option<Output> {
        output.and_then(Output::from_resource).or_else(|| self.space.outputs().next().cloned())
    }
}

impl WlrLayerShellHandler for State {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.protocols.layer_shell
    }

    fn new_layer_surface(&mut self, surface: WlrLayerSurface, output: Option<WlOutput>, _layer: Layer, namespace: String) {
        let Some(output) = self.output_for(output.as_ref()) else {
            surface.send_close();
            return;
        };
        // Configured on its first commit (see layer_commit), once it said where it wants to be.
        if let Err(err) = layer_map_for_output(&output).map_layer(&LayerSurface::new(surface, namespace)) {
            tracing::warn!("layer surface: {err}");
        }
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        for output in self.space.outputs() {
            let mut map = layer_map_for_output(output);
            let layer = map.layers().find(|l| l.layer_surface() == &surface).cloned();
            if let Some(layer) = layer {
                map.unmap_layer(&layer);
            }
        }
        let focused = self.seat.get_keyboard().and_then(|k| k.current_focus());
        if focused.as_ref() == Some(surface.wl_surface()) {
            let top = self.top_window();
            self.focus_window(top, SERIAL_COUNTER.next_serial());
        }
        self.schedule_redraw();
    }
}

impl FractionalScaleHandler for State {
    /// The first output's scale to begin with; `send_scales` corrects it to its own output's after the next frame.
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let scale = self.space.outputs().next().map_or(1.0, |o| o.current_scale().fractional_scale());
        with_states(&surface, |states| with_fractional_scale(states, |fractional| fractional.set_preferred_scale(scale)));
    }
}

impl PointerConstraintsHandler for State {
    /// A lock or confinement takes hold at once if the pointer is already over the surface.
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        if pointer.current_focus().as_ref() == Some(surface) {
            with_pointer_constraint(surface, pointer, |constraint| {
                if let Some(constraint) = constraint {
                    constraint.activate();
                }
            });
        }
    }

    /// Where the app wants the pointer once its lock ends. ponytail: only for a window's main surface; nested, the host cursor stays put.
    fn cursor_position_hint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>, location: Point<f64, Logical>) {
        if !with_pointer_constraint(surface, pointer, |c| c.is_some_and(|c| c.is_active())) {
            return;
        }
        let window = self.space.elements().find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface));
        if let Some(origin) = window.and_then(|w| Some(self.space.element_location(w)? - w.geometry().loc)) {
            pointer.set_location(origin.to_f64() + location);
        }
    }
}

impl KeyboardShortcutsInhibitHandler for State {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.protocols.shortcuts_inhibit
    }

    /// Granted at once (VMs, remote desktop), except to sandboxed apps.
    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        if self.display_handle.get_client(inhibitor.wl_surface().id()).is_ok_and(|c| unsandboxed(&c)) {
            inhibitor.activate();
        }
    }
}

impl SecurityContextHandler for State {
    /// Clients connecting through the sandbox's socket are tagged with its context, which keeps privileged globals from them.
    fn context_created(&mut self, source: SecurityContextListenerSource, context: SecurityContext) {
        let inserted = self.loop_handle.insert_source(source, move |stream, _, state| {
            let data = ClientState { security_context: Some(context.clone()), ..Default::default() };
            if let Err(err) = state.display_handle.insert_client(stream, Arc::new(data)) {
                tracing::warn!("rejected sandboxed client: {err}");
            }
        });
        if let Err(err) = inserted {
            tracing::warn!("security context: {err}");
        }
    }
}

impl TabletSeatHandler for State {}

/// A surface's wp_tearing_control hint. ponytail: stored only; nested there is no async flip, the udev backend uses it (M6).
#[derive(Default)]
pub struct Tearing {
    controlled: AtomicBool,
    pub allowed: AtomicBool,
}

fn tearing(surface: &WlSurface, f: impl FnOnce(&Tearing)) {
    if surface.is_alive() {
        with_states(surface, |states| f(states.data_map.get_or_insert_threadsafe(Tearing::default)));
    }
}

impl GlobalDispatch<WpTearingControlManagerV1, ()> for State {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<WpTearingControlManagerV1>, _: &(), init: &mut DataInit<'_, Self>) {
        init.init(resource, ());
    }
}

impl Dispatch<WpTearingControlManagerV1, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        manager: &WpTearingControlManagerV1,
        request: wp_tearing_control_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let wp_tearing_control_manager_v1::Request::GetTearingControl { id, surface } = request {
            let mut taken = true;
            tearing(&surface, |t| taken = t.controlled.swap(true, Ordering::Relaxed));
            if taken {
                manager.post_error(wp_tearing_control_manager_v1::Error::TearingControlExists, "surface already has a tearing control");
                return;
            }
            init.init(id, surface);
        }
    }
}

impl Dispatch<WpTearingControlV1, WlSurface> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &WpTearingControlV1,
        request: wp_tearing_control_v1::Request,
        surface: &WlSurface,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let wp_tearing_control_v1::Request::SetPresentationHint { hint } = request {
            let allowed = matches!(hint, WEnum::Value(wp_tearing_control_v1::PresentationHint::Async));
            tearing(surface, |t| t.allowed.store(allowed, Ordering::Relaxed));
        }
    }

    fn destroyed(_: &mut Self, _: ClientId, _: &WpTearingControlV1, surface: &WlSurface) {
        tearing(surface, |t| {
            t.controlled.store(false, Ordering::Relaxed);
            t.allowed.store(false, Ordering::Relaxed);
        });
    }
}

delegate_layer_shell!(State);
delegate_presentation!(State);
delegate_fractional_scale!(State);
delegate_relative_pointer!(State);
delegate_pointer_constraints!(State);
delegate_pointer_gestures!(State);
// Text input: ime.rs delegates it, watching each request first (the emoji picker, E7.2).
delegate_cursor_shape!(State);
delegate_keyboard_shortcuts_inhibit!(State);
delegate_security_context!(State);
delegate_virtual_keyboard_manager!(State);
