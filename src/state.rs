//! Central state and the Wayland protocol handlers.

use std::{ffi::OsString, os::fd::OwnedFd, sync::Arc, time::Instant};

use smithay::{
    backend::{allocator::dmabuf::Dmabuf, renderer::ImportDma},
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_output, delegate_primary_selection, delegate_seat, delegate_shm, delegate_viewporter,
    delegate_xdg_activation, delegate_xdg_dialog, delegate_xdg_foreign, delegate_xdg_shell,
    desktop::{
        PopupKeyboardGrab, PopupKind, PopupManager, PopupPointerGrab, PopupUngrabStrategy, Space, Window, find_popup_root_surface, get_popup_toplevel_coords,
    },
    input::{Seat, SeatHandler, SeatState, pointer::Focus},
    reexports::{
        calloop::{EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction, channel, generic::Generic},
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            Client, Display, DisplayHandle, Resource,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer::WlBuffer, wl_seat::WlSeat, wl_surface::WlSurface},
        },
    },
    utils::{SERIAL_COUNTER, Serial},
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState, get_parent, is_sync_subsurface, with_states},
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        output::{OutputHandler, OutputManagerState},
        selection::{
            SelectionHandler, SelectionSource, SelectionTarget,
            data_device::{ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler, set_data_device_focus},
            primary_selection::{PrimarySelectionHandler, PrimarySelectionState, set_primary_focus},
        },
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState, XdgToplevelSurfaceData,
            dialog::{XdgDialogHandler, XdgDialogState},
        },
        shm::{ShmHandler, ShmState},
        socket::ListeningSocketSource,
        viewporter::ViewporterState,
        xdg_activation::{XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData},
        xdg_foreign::{XdgForeignHandler, XdgForeignState},
    },
};

use crate::clipboard::{self, Clipboard};

pub struct State {
    pub start_time: Instant,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,
    pub loop_signal: LoopSignal,
    pub loop_handle: LoopHandle<'static, Self>,

    pub space: Space<Window>,
    pub popups: PopupManager,
    pub seat: Seat<Self>,
    pub clipboard: Clipboard,
    /// Set by the backend once the renderer exists.
    pub backend: Option<crate::backend::winit::Winit>,
    redraw_pending: bool,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub dmabuf_state: DmabufState,
    pub _output_manager_state: OutputManagerState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub activation_state: XdgActivationState,
    pub foreign_state: XdgForeignState,
    pub _viewporter_state: ViewporterState,
    pub _dialog_state: XdgDialogState,
}

impl State {
    pub fn new(event_loop: &mut EventLoop<'static, Self>, display: Display<Self>) -> Result<Self, Box<dyn std::error::Error>> {
        let dh = display.handle();

        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        seat.add_keyboard(Default::default(), 500, 30)?;
        seat.add_pointer();

        let socket = ListeningSocketSource::new_auto()?;
        let socket_name = socket.socket_name().to_os_string();
        let handle = event_loop.handle();
        handle.insert_source(socket, |stream, _, state| {
            if let Err(err) = state.display_handle.insert_client(stream, Arc::new(ClientState::default())) {
                tracing::warn!("rejected client: {err}");
            }
        })?;
        handle.insert_source(Generic::new(display, Interest::READ, Mode::Level), |_, display, state| {
            // SAFETY: the display is never dropped while the event loop runs.
            if let Err(err) = unsafe { display.get_mut().dispatch_clients(state) } {
                tracing::warn!("dispatching clients: {err}");
            }
            Ok(PostAction::Continue)
        })?;
        let (clipboard_tx, clipboard_rx) = channel::channel();
        handle.insert_source(clipboard_rx, |event, _, state| {
            if let channel::Event::Msg(fetched) = event {
                state.clipboard.store(fetched);
            }
        })?;

        Ok(Self {
            start_time: Instant::now(),
            socket_name,
            loop_signal: event_loop.get_signal(),
            loop_handle: handle,
            space: Space::default(),
            popups: PopupManager::default(),
            seat,
            clipboard: Clipboard::new(clipboard_tx),
            backend: None,
            redraw_pending: false,
            compositor_state: CompositorState::new::<Self>(&dh),
            xdg_shell_state: XdgShellState::new::<Self>(&dh),
            shm_state: ShmState::new::<Self>(&dh, vec![]),
            dmabuf_state: DmabufState::new(),
            _output_manager_state: OutputManagerState::new_with_xdg_output::<Self>(&dh),
            seat_state,
            data_device_state: DataDeviceState::new::<Self>(&dh),
            primary_selection_state: PrimarySelectionState::new::<Self>(&dh),
            activation_state: XdgActivationState::new::<Self>(&dh),
            foreign_state: XdgForeignState::new::<Self>(&dh),
            _viewporter_state: ViewporterState::new::<Self>(&dh),
            _dialog_state: XdgDialogState::new::<Self>(&dh),
            display_handle: dh,
        })
    }

    /// Draws a frame at the next vblank. Called only when something changed, so an idle desktop costs no CPU.
    pub fn schedule_redraw(&mut self) {
        if !self.redraw_pending {
            self.redraw_pending = true;
            self.loop_handle.insert_idle(|state| {
                state.redraw_pending = false;
                state.render();
            });
        }
    }

    fn window_for(&self, surface: &WlSurface) -> Option<&Window> {
        self.space.elements().find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
    }

    /// Raises and activates `window`, giving it the keyboard; `None` clears focus.
    pub fn focus_window(&mut self, window: Option<Window>, serial: Serial) {
        let Some(keyboard) = self.seat.get_keyboard() else { return };
        match window {
            Some(window) => {
                self.space.raise_element(&window, true);
                keyboard.set_focus(self, window.toplevel().map(|t| t.wl_surface().clone()), serial);
            }
            None => {
                self.space.elements().for_each(|w| {
                    w.set_activated(false);
                });
                keyboard.set_focus(self, None, serial);
            }
        }
        self.space.elements().for_each(|w| {
            if let Some(toplevel) = w.toplevel() {
                toplevel.send_pending_configure();
            }
        });
    }

    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let kind = PopupKind::Xdg(popup.clone());
        let Ok(root) = find_popup_root_surface(&kind) else { return };
        let Some(window) = self.window_for(&root) else { return };
        let Some(output_geo) = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)) else { return };
        let Some(window_geo) = self.space.element_geometry(window) else { return };

        let mut target = output_geo;
        target.loc -= get_popup_toplevel_coords(&kind);
        target.loc -= window_geo.loc;
        popup.with_pending_state(|state| state.geometry = state.positioner.get_unconstrained_geometry(target));
    }
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        // Every client is inserted with a ClientState (see State::new).
        &client.get_data::<ClientState>().expect("client without ClientState").compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        smithay::backend::renderer::utils::on_commit_buffer_handler::<Self>(surface);

        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self.window_for(&root) {
                window.on_commit();
            }
        }

        // The first commit of a toplevel asks for its initial configure.
        if let Some(toplevel) = self.window_for(surface).and_then(|w| w.toplevel()) {
            let sent = with_states(surface, |states| {
                states.data_map.get::<XdgToplevelSurfaceData>().is_some_and(|data| data.lock().is_ok_and(|d| d.initial_configure_sent))
            });
            if !sent {
                toplevel.send_configure();
            }
        }

        self.popups.commit(surface);
        self.schedule_redraw();
        if let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface)
            && !popup.is_initial_configure_sent()
        {
            let _ = popup.send_configure();
        }
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|state| state.states.set(xdg_toplevel::State::Activated));
        let window = Window::new_wayland_window(surface);
        // ponytail: simple cascade; real placement (E1.4) moves to wm.rs.
        let step = 32 * (self.space.elements().count() as i32 % 10);
        self.space.map_element(window.clone(), (40 + step, 40 + step), true);
        self.focus_window(Some(window), SERIAL_COUNTER.next_serial());
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        self.schedule_redraw();
        let next = self.space.elements().rev().find(|w| w.toplevel() != Some(&surface)).cloned();
        self.focus_window(next, SERIAL_COUNTER.next_serial());
    }

    fn popup_destroyed(&mut self, _surface: PopupSurface) {
        self.schedule_redraw();
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        if let Err(err) = self.popups.track_popup(PopupKind::Xdg(surface)) {
            tracing::warn!("popup: {err}");
        }
    }

    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn grab(&mut self, surface: PopupSurface, seat: WlSeat, serial: Serial) {
        let Some(seat) = Seat::<Self>::from_resource(&seat) else { return };
        let kind = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&kind) else { return };
        let Ok(mut grab) = self.popups.grab_popup(root, kind, &seat, serial) else { return };

        if let Some(keyboard) = seat.get_keyboard() {
            if keyboard.is_grabbed() && !(keyboard.has_grab(serial) || keyboard.has_grab(grab.previous_serial().unwrap_or(serial))) {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            keyboard.set_focus(self, grab.current_grab(), serial);
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        if let Some(pointer) = seat.get_pointer() {
            if pointer.is_grabbed() && !(pointer.has_grab(serial) || pointer.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial()))) {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    /// The clipboard follows the keyboard: only the focused app may read it.
    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = &self.display_handle;
        self.clipboard.adopt_if_orphaned(dh, seat);
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client.clone());
        set_primary_focus(dh, seat, client);
    }
}

impl SelectionHandler for State {
    type SelectionUserData = ();

    fn new_selection(&mut self, ty: SelectionTarget, source: Option<SelectionSource>, seat: Seat<Self>) {
        if ty != SelectionTarget::Clipboard {
            return;
        }
        let owner = source.as_ref().and_then(|_| clipboard::focused_client(&self.display_handle, &seat));
        let generation = self.clipboard.replaced(owner);
        if let Some(source) = source {
            let mimes = source.mime_types();
            // Smithay installs the new source after this call returns; read it on the next idle.
            self.loop_handle.insert_idle(move |state| state.clipboard.fetch(&seat, generation, mimes));
        }
    }

    fn send_selection(&mut self, ty: SelectionTarget, mime_type: String, fd: OwnedFd, _seat: Seat<Self>, _user_data: &()) {
        if ty == SelectionTarget::Clipboard {
            self.clipboard.send(&mime_type, fd);
        }
    }
}

impl DataDeviceHandler for State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for State {}
impl ServerDndGrabHandler for State {}

impl PrimarySelectionHandler for State {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    /// Test-import every buffer so a format the GPU can't read fails here, not as a black window.
    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        if self.backend.as_mut().is_some_and(|w| w.backend.renderer().import_dmabuf(&dmabuf, None).is_ok()) {
            let _ = notifier.successful::<Self>();
        } else {
            notifier.failed();
        }
    }
}

impl OutputHandler for State {}

impl XdgActivationHandler for State {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.activation_state
    }

    /// An app asks to bring a window forward (e.g. a link clicked in the terminal raises Brave).
    /// ponytail: any token under 10 s old is honoured; focus-stealing rules come with the window manager.
    fn request_activation(&mut self, _token: XdgActivationToken, token_data: XdgActivationTokenData, surface: WlSurface) {
        if token_data.timestamp.elapsed().as_secs() >= 10 {
            return;
        }
        if let Some(window) = self.window_for(&surface).cloned() {
            self.focus_window(Some(window), SERIAL_COUNTER.next_serial());
        }
    }
}

impl XdgForeignHandler for State {
    fn xdg_foreign_state(&mut self) -> &mut XdgForeignState {
        &mut self.foreign_state
    }
}

impl XdgDialogHandler for State {}

delegate_compositor!(State);
delegate_xdg_shell!(State);
delegate_shm!(State);
delegate_dmabuf!(State);
delegate_output!(State);
delegate_seat!(State);
delegate_data_device!(State);
delegate_primary_selection!(State);
delegate_xdg_activation!(State);
delegate_xdg_foreign!(State);
delegate_xdg_dialog!(State);
delegate_viewporter!(State);
