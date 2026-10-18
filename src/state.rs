//! Central state and the Wayland protocol handlers.

use std::{ffi::OsString, os::fd::OwnedFd, sync::Arc, time::Instant};

use smithay::{
    backend::{allocator::dmabuf::Dmabuf, renderer::ImportDma},
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_output, delegate_primary_selection, delegate_seat, delegate_shm, delegate_viewporter,
    delegate_xdg_activation, delegate_xdg_dialog, delegate_xdg_foreign, delegate_xdg_shell,
    desktop::{
        PopupKeyboardGrab, PopupKind, PopupManager, PopupPointerGrab, PopupUngrabStrategy, Space, Window, find_popup_root_surface, get_popup_toplevel_coords,
        layer_map_for_output,
    },
    input::{Seat, SeatHandler, SeatState, keyboard::Keycode, pointer::Focus},
    reexports::{
        calloop::{
            EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction, channel,
            generic::Generic,
            timer::{TimeoutAction, Timer},
        },
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            Client, Display, DisplayHandle, Resource,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer::WlBuffer, wl_seat::WlSeat, wl_surface::WlSurface},
        },
    },
    utils::{Logical, Point, SERIAL_COUNTER, Serial, Size},
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState, get_parent, is_sync_subsurface, with_states},
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        output::{OutputHandler, OutputManagerState},
        security_context::SecurityContext,
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

use crate::{
    clipboard::{self, Clipboard},
    config::Config,
    grabs::{MoveGrab, Resize, edges_from},
    protocols::Protocols,
    rules::{self, Actions, CompiledRule},
    shortcuts::{self, Action, Binding},
    wm::{self, Rect},
};

/// Marks a window that got its first position (it is placed once its size is known).
struct Placed;

/// Where a zoomed or fullscreen window goes back to.
struct Restore(std::sync::Mutex<Option<Rect>>);

/// The window rules that matched this window.
struct Ruled(std::sync::Mutex<Actions>);

fn ruled(window: &Window) -> Option<Actions> {
    window.user_data().get::<Ruled>().and_then(|r| r.0.lock().ok().map(|a| a.clone()))
}

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
    pub resize: Option<Resize>,
    /// Nested in another desktop: Alt stands in for Super, which the host keeps.
    pub nested: bool,
    pub config: Config,
    /// Built from `config.rules`.
    pub rules: Vec<CompiledRule>,
    /// Built from `config.bindings`.
    pub bindings: Vec<(Binding, Action)>,
    /// Keys whose press was a shortcut; their release is kept from the app too, so nothing sticks.
    pub suppressed: Vec<Keycode>,
    /// Minimised and hidden windows with where they were.
    pub hidden: Vec<(Window, Point<i32, Logical>)>,
    /// The pid of the `noobsbot --quake` the quake action started; its window gets `rules::QUAKE_ID`.
    pub quake: Option<u32>,
    pub zones: crate::input::Zones,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub dmabuf_state: DmabufState,
    /// linux-drm-syncobj-v1, when the GPU supports it (see sync.rs).
    pub syncobj: Option<smithay::wayland::drm_syncobj::DrmSyncobjState>,
    pub _output_manager_state: OutputManagerState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub activation_state: XdgActivationState,
    pub foreign_state: XdgForeignState,
    pub _viewporter_state: ViewporterState,
    pub _dialog_state: XdgDialogState,
    pub protocols: Protocols,
    pub decorations: crate::decorations::Decorations,
    /// Title bar labels; the font loads on first use.
    pub text: crate::text::Text,
    pub services: crate::services::Services,
    /// `None`: no battery, or UPower isn't running.
    pub battery: Option<crate::services::power::Battery>,
    /// `None`: NetworkManager isn't running.
    pub network: Option<crate::services::network::Network>,
    /// `None`: no adapter, or BlueZ isn't running.
    pub bluetooth: Option<crate::services::bluetooth::Bluetooth>,
    /// `None`: power-profiles-daemon isn't running.
    pub profiles: Option<crate::services::profiles::Profiles>,
    /// Percent; `None` without a backlight.
    pub brightness: Option<u8>,
    /// `None`: no output device, or PipeWire isn't running.
    pub sound: Option<crate::services::sound::Sound>,
    /// `None`: nothing playing or paused.
    pub media: Option<crate::services::media::Media>,
    pub symbols: crate::symbols::Symbols,
    /// The menu-bar popup that is open (E2.2).
    pub popup: Option<crate::shell::Popup>,
    pub osd: Option<crate::shell::Osd>,
    /// The power alert that is up.
    pub alert: Option<crate::shell::alert::Alert>,
    /// Its once-a-second countdown.
    pub alert_timer: Option<smithay::reexports::calloop::RegistrationToken>,
    /// Do Not Disturb, from Control Center; the notification server (M4) honours it.
    pub dnd: bool,
    /// The Control Center module showing its list.
    pub expanded: Option<crate::shell::control::Module>,
    /// The Control Center slider being dragged.
    pub shell_drag: Option<crate::shell::control::Control>,
    /// Hides the OSD; replaced on every key press.
    pub osd_timer: Option<smithay::reexports::calloop::RegistrationToken>,
}

impl State {
    pub fn new(event_loop: &mut EventLoop<'static, Self>, display: Display<Self>, nested: bool, config: Config) -> Result<Self, Box<dyn std::error::Error>> {
        let dh = display.handle();

        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        let keyboard = config.keyboard.resolved();
        if let Err(err) = seat.add_keyboard(crate::input::xkb(&keyboard), 500, 30) {
            // A typo in the layout must not stop the desktop: fall back to xkb's default (us).
            tracing::warn!(?keyboard, "keyboard layout: {err}");
            seat.add_keyboard(Default::default(), 500, 30)?;
        }
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
        // The system font scan takes ~20 ms: do it off the frame thread (E1.10). A label drawn before it lands loads
        // its own fonts; the cache is simply replaced.
        let (text_tx, text_rx) = channel::channel();
        std::thread::Builder::new().name("font-load".into()).spawn(move || {
            let mut text = crate::text::Text::default();
            text.preload();
            let _ = text_tx.send(text);
        })?;
        handle.insert_source(text_rx, |event, _, state| {
            if let channel::Event::Msg(text) = event {
                state.text = text;
                state.schedule_redraw();
            }
        })?;
        let (services_tx, services_rx) = channel::channel();
        let services = crate::services::spawn(services_tx);
        if !config.clock.timezone.is_empty() {
            services.send(crate::services::Command::Timezone(config.clock.timezone.clone()));
        }
        handle.insert_source(services_rx, |event, _, state| {
            if let channel::Event::Msg(update) = event {
                // Only the changed menu-bar item gets a new buffer, so the redraw damages just that item.
                match update {
                    crate::services::Update::Battery(battery) => state.battery = battery,
                    crate::services::Update::Network(network) => state.network = network,
                    crate::services::Update::Bluetooth(bluetooth) => state.bluetooth = bluetooth,
                    crate::services::Update::Profiles(profiles) => state.profiles = profiles,
                    crate::services::Update::Brightness(brightness) => state.brightness = brightness,
                    crate::services::Update::Sound(sound) => state.sound = sound,
                    crate::services::Update::Media(media) => state.media = media,
                }
                state.schedule_redraw();
            }
        })?;
        // The menu-bar clock (E2.4): one redraw per minute (second, with seconds on), on the boundary.
        let tick = |state: &Self| crate::clock::until_next_tick(&chrono::Local::now(), state.config.clock.format.seconds);
        handle.insert_source(Timer::from_duration(crate::clock::until_next_tick(&chrono::Local::now(), config.clock.format.seconds)), move |_, _, state| {
            state.schedule_redraw();
            TimeoutAction::ToDuration(tick(state))
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
            resize: None,
            nested,
            rules: rules::compile(&config.rules),
            bindings: shortcuts::from_config(&config),
            config,
            suppressed: Vec::new(),
            hidden: Vec::new(),
            quake: None,
            zones: Default::default(),
            compositor_state: CompositorState::new::<Self>(&dh),
            xdg_shell_state: XdgShellState::new::<Self>(&dh),
            shm_state: ShmState::new::<Self>(&dh, vec![]),
            dmabuf_state: DmabufState::new(),
            syncobj: None,
            _output_manager_state: OutputManagerState::new_with_xdg_output::<Self>(&dh),
            seat_state,
            data_device_state: DataDeviceState::new::<Self>(&dh),
            primary_selection_state: PrimarySelectionState::new::<Self>(&dh),
            activation_state: XdgActivationState::new::<Self>(&dh),
            foreign_state: XdgForeignState::new::<Self>(&dh),
            _viewporter_state: ViewporterState::new::<Self>(&dh),
            _dialog_state: XdgDialogState::new::<Self>(&dh),
            protocols: Protocols::new(&dh),
            decorations: crate::decorations::Decorations::new(&dh),
            text: Default::default(),
            services,
            battery: None,
            network: None,
            bluetooth: None,
            profiles: None,
            brightness: None,
            sound: None,
            media: None,
            symbols: Default::default(),
            popup: None,
            osd: None,
            alert: None,
            alert_timer: None,
            dnd: false,
            expanded: None,
            shell_drag: None,
            osd_timer: None,
            display_handle: dh,
        })
    }

    /// Draws a frame at the next vblank. Called only when something changed, so an idle desktop costs no CPU.
    /// The frame starts as late as the frame clock allows (E1.10), so input arriving meanwhile is still in it.
    pub fn schedule_redraw(&mut self) {
        if self.redraw_pending {
            return;
        }
        self.redraw_pending = true;
        let delay = self.backend.as_mut().map_or(std::time::Duration::ZERO, |b| b.clock.delay(Instant::now()));
        let draw = |state: &mut Self| {
            state.redraw_pending = false;
            state.render();
        };
        if delay.is_zero() {
            self.loop_handle.insert_idle(draw);
        } else if let Err(err) = self.loop_handle.insert_source(Timer::from_duration(delay), move |_, _, state| {
            draw(state);
            TimeoutAction::Drop
        }) {
            tracing::warn!("frame timer: {err}");
            self.loop_handle.insert_idle(draw);
        }
    }

    /// What the menu bar shows; `None` while a fullscreen window has focus (it covers the bar, as on macOS).
    pub fn menu_bar(&self) -> Option<crate::render::menubar::Content> {
        use crate::render::menubar::{self, Item};
        let focused = self.focused_window();
        let toplevel = focused.as_ref().and_then(|w| w.toplevel());
        if toplevel.is_some_and(|t| t.current_state().states.contains(xdg_toplevel::State::Fullscreen)) {
            return None;
        }
        let app = toplevel.and_then(|t| {
            let id = with_states(t.wl_surface(), |s| s.data_map.get::<XdgToplevelSurfaceData>().and_then(|d| d.lock().ok()?.app_id.clone()));
            // noobsbot and others set no app id: fall back to the process name.
            id.filter(|id| !id.is_empty()).or_else(|| {
                let pid = t.wl_surface().client()?.get_credentials(&self.display_handle).ok()?.pid;
                Some(std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?.trim().to_owned())
            })
        });
        let simple = |symbol| Item { symbol, text: None };
        let status = [
            self.network.as_ref().map(|n| simple(menubar::wifi(n))),
            self.bluetooth.as_ref().map(|b| simple(menubar::bluetooth(b))),
            self.battery.as_ref().map(menubar::battery),
            self.sound.as_ref().map(|s| simple(menubar::volume(s))),
        ];
        Some(menubar::Content { app, status: status.into_iter().flatten().collect(), clock: crate::clock::text(&chrono::Local::now(), self.config.clock.format) })
    }

    /// Where windows may go: the first output minus the menu bar and what layer surfaces (bars, docks) reserve.
    /// ponytail: a zoomed window keeps its size when a bar appears later; refitting comes with output changes (M6).
    pub fn work_area(&self) -> Option<Rect> {
        let output = self.space.outputs().next()?;
        let mut zone = crate::render::menubar::below(layer_map_for_output(output).non_exclusive_zone());
        zone.loc += self.space.output_geometry(output)?.loc;
        Some(zone)
    }

    /// Super, or Alt when nested (the host desktop keeps Super).
    pub fn desktop_key_held(&self) -> bool {
        let mods = self.modifiers();
        if self.nested { mods.alt } else { mods.logo }
    }

    /// The `windows.move_modifier` key; "super" means the desktop key.
    pub fn move_key_held(&self) -> bool {
        match self.config.windows.move_modifier {
            crate::config::Modifier::Super => self.desktop_key_held(),
            crate::config::Modifier::Alt => self.modifiers().alt,
        }
    }

    pub fn modifiers(&self) -> smithay::input::keyboard::ModifiersState {
        self.seat.get_keyboard().map(|k| k.modifier_state()).unwrap_or_default()
    }

    /// Moves a window so its frame (our title bar, then its visible part without client-side shadows) starts at
    /// `loc`; a Space location is the visible corner of the content.
    pub fn set_visible_loc(&mut self, window: &Window, loc: smithay::utils::Point<i32, smithay::utils::Logical>) {
        // map_element also raises the window: put back what was above it, so moving never restacks.
        let above: Vec<Window> = self.space.elements().skip_while(|w| *w != window).skip(1).cloned().collect();
        self.space.map_element(window.clone(), loc + Point::from((0, crate::decorations::bar_height(window))), false);
        for w in &above {
            self.space.raise_element(w, false);
        }
        self.restack();
        self.schedule_redraw();
    }

    /// Starts moving `window` with the pointer; the move begins after the drag threshold.
    pub fn start_move(&mut self, window: Window, start_data: smithay::input::pointer::GrabStartData<Self>, serial: Serial) {
        let Some(pointer) = self.seat.get_pointer() else { return };
        let Some(start) = self.frame(&window) else { return };
        pointer.set_grab(self, MoveGrab { start_data, window, start, started: false }, serial, Focus::Clear);
    }

    /// Zoom (fill the work area) or fullscreen (fill the output), remembering where the window was.
    /// ponytail: one output and no menu bar or dock yet, so both fill the same area; refitting on output resize comes with udev (M6).
    pub fn fill(&mut self, surface: &ToplevelSurface, state: xdg_toplevel::State) {
        let (Some(window), Some(area)) = (self.window_for(surface.wl_surface()).cloned(), self.work_area()) else { return };
        let restore = window.user_data().get_or_insert(|| Restore(std::sync::Mutex::new(None)));
        if let (Ok(mut saved), Some(frame)) = (restore.0.lock(), self.frame(&window)) {
            saved.get_or_insert(frame);
        }
        window.user_data().insert_if_missing(|| Placed);
        // State first: a fullscreen window has no title bar, so its content is the whole area.
        surface.with_pending_state(|s| s.states.set(state));
        let size = self.content_size(&window, area.size);
        surface.with_pending_state(|s| s.size = Some(size));
        if surface.is_initial_configure_sent() {
            surface.send_pending_configure();
        }
        self.set_visible_loc(&window, area.loc);
    }

    /// Leaves zoom or fullscreen; returns where the window went back to.
    pub fn unfill(&mut self, surface: &ToplevelSurface, state: xdg_toplevel::State) -> Option<Rect> {
        let window = self.window_for(surface.wl_surface()).cloned()?;
        if !surface.with_pending_state(|s| s.states.contains(state)) {
            return None;
        }
        let saved = window.user_data().get::<Restore>().and_then(|r| r.0.lock().ok()?.take());
        surface.with_pending_state(|s| s.states.unset(state));
        let size = saved.map(|r| self.content_size(&window, r.size));
        surface.with_pending_state(|s| s.size = size);
        surface.send_pending_configure();
        if let Some(rect) = saved {
            self.set_visible_loc(&window, rect.loc);
        }
        saved
    }

    pub fn window_for(&self, surface: &WlSurface) -> Option<&Window> {
        self.space.elements().find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
    }

    /// Raises and activates `window`, giving it the keyboard; `None` clears focus.
    pub fn focus_window(&mut self, window: Option<Window>, serial: Serial) {
        let Some(keyboard) = self.seat.get_keyboard() else { return };
        // A layer surface with exclusive keyboard (a launcher, a lock screen) keeps it while it is up.
        let exclusive = self.exclusive_layer().map(|l| l.wl_surface().clone());
        match window {
            Some(window) => {
                self.space.raise_element(&window, true);
                self.restack();
                keyboard.set_focus(self, exclusive.or_else(|| window.toplevel().map(|t| t.wl_surface().clone())), serial);
            }
            None => {
                self.space.elements().for_each(|w| {
                    w.set_activated(false);
                });
                keyboard.set_focus(self, exclusive, serial);
            }
        }
        // A new window gets its first configure on its first commit (with window rules applied), not here.
        self.space.elements().for_each(|w| {
            if let Some(toplevel) = w.toplevel().filter(|t| t.is_initial_configure_sent()) {
                toplevel.send_pending_configure();
            }
        });
    }

    /// Window rules (E1.9), resolved before the first configure so the app opens at the ruled size, and again if the
    /// app sets its app_id only after that (then the window is resized and moved where the rule says).
    /// `layer` is kept by `restack`, `decorations` by decorations.rs.
    /// ponytail: space and opacity wait for Spaces (M4) and alpha.
    fn apply_rules(&mut self, surface: &ToplevelSurface) {
        let actions = self.resolve_rules(surface);
        let (window, area) = (self.window_for(surface.wl_surface()).cloned(), self.work_area());
        let (Some(window), Some(area)) = (window, area) else { return };
        if let Some([w, h]) = actions.size {
            // A ruled size is the frame's, title bar included.
            let size = self.content_size(&window, (w.to_px(area.size.w), h.to_px(area.size.h)).into());
            surface.with_pending_state(|s| s.size = Some(size));
        }
        let (fullscreen, position) = (actions.fullscreen == Some(true), actions.position);
        if let Ok(mut ruled) = window.user_data().get_or_insert(|| Ruled(std::sync::Mutex::new(Actions::default()))).0.lock() {
            *ruled = actions;
        }
        if fullscreen {
            self.fill(surface, xdg_toplevel::State::Fullscreen);
        } else if surface.is_initial_configure_sent() {
            surface.send_pending_configure();
            if let Some([x, y]) = position.filter(|_| window.user_data().get::<Placed>().is_some()) {
                self.set_visible_loc(&window, area.loc + Point::from((x.to_px(area.size.w), y.to_px(area.size.h))));
            }
        }
        self.restack();
    }

    pub fn resolve_rules(&self, surface: &ToplevelSurface) -> Actions {
        let (app_id, title) = with_states(surface.wl_surface(), |states| {
            let data = states.data_map.get::<XdgToplevelSurfaceData>().and_then(|d| d.lock().ok().map(|d| (d.app_id.clone(), d.title.clone())));
            data.map(|(a, t)| (a.unwrap_or_default(), t.unwrap_or_default())).unwrap_or_default()
        });
        let quake = app_id.is_empty() && self.window_for(surface.wl_surface()).is_some_and(|w| self.is_quake(w));
        let app_id = if quake { rules::QUAKE_ID.to_owned() } else { app_id };
        let actions = rules::resolve(&self.rules, &app_id, &title);
        tracing::debug!(app_id, title, ?actions, "window rules");
        actions
    }

    /// Keeps `float_above` windows over every normal one, and `layer = "overlay"` ones (the quake terminal) over those
    /// and over fullscreen apps.
    fn restack(&mut self) {
        let is = |w: &Window, f: fn(&Actions) -> bool| ruled(w).is_some_and(|a| f(&a));
        let float = |a: &Actions| a.float_above == Some(true);
        let overlay = |a: &Actions| a.layer == Some(rules::Layer::Overlay);
        let above: Vec<Window> = self.space.elements().filter(|w| is(w, float)).chain(self.space.elements().filter(|w| is(w, overlay))).cloned().collect();
        for window in above {
            self.space.raise_element(&window, false);
        }
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
    /// Set for clients of a sandbox (security-context-v1); they get no privileged globals.
    pub security_context: Option<SecurityContext>,
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
        // Unreachable by any client: both insert_client calls (the socket here, security contexts in protocols.rs)
        // pass a ClientState, so this can only fire on a bug of ours.
        &client.get_data::<ClientState>().expect("client without ClientState").compositor_state
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        crate::sync::add_hook(surface);
    }

    fn commit(&mut self, surface: &WlSurface) {
        smithay::backend::renderer::utils::on_commit_buffer_handler::<Self>(surface);

        // The first commit of a toplevel asks for its initial configure. Rules go first: an app may already give its
        // window geometry in this commit, and placement below must see the ruled position.
        if let Some(toplevel) = self.window_for(surface).and_then(|w| w.toplevel()).cloned() {
            let sent = with_states(surface, |states| {
                states.data_map.get::<XdgToplevelSurfaceData>().is_some_and(|data| data.lock().is_ok_and(|d| d.initial_configure_sent))
            });
            if !sent {
                // Apps that size themselves should fit the work area (xdg_toplevel bounds).
                if let (Some(window), Some(area)) = (self.window_for(surface).cloned(), self.work_area()) {
                    let bounds = self.content_size(&window, area.size);
                    toplevel.with_pending_state(|s| s.bounds = Some(bounds));
                }
                self.apply_rules(&toplevel);
                toplevel.send_configure();
            }
        }

        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self.window_for(&root).cloned() {
                window.on_commit();
                let size = window.geometry().size + Size::from((0, crate::decorations::bar_height(&window)));
                if size.w > 0 && window.user_data().get::<Placed>().is_none() {
                    window.user_data().insert_if_missing(|| Placed);
                    if let Some(area) = self.work_area() {
                        // A window bigger than the work area (the app ignored the bounds) is shrunk to fit.
                        let fitted = Size::from((size.w.min(area.size.w), size.h.min(area.size.h)));
                        if fitted != size
                            && let Some(toplevel) = window.toplevel()
                        {
                            let content = self.content_size(&window, fitted);
                            toplevel.with_pending_state(|s| s.size = Some(content));
                            toplevel.send_pending_configure();
                        }
                        let size = fitted;
                        let position = ruled(&window).and_then(|a| a.position);
                        let loc = position.map_or_else(|| wm::place(area, size), |[x, y]| area.loc + Point::from((x.to_px(area.size.w), y.to_px(area.size.h))));
                        tracing::debug!(?size, ?loc, ?position, "placed");
                        self.set_visible_loc(&window, loc);
                    }
                }
            }
            self.resize_commit(&root);
        }

        self.layer_commit(surface);
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

    /// A rule can match on the title, so `float_above` follows it (Helium names its PiP window after it opens).
    fn title_changed(&mut self, surface: ToplevelSurface) {
        let float_above = self.resolve_rules(&surface).float_above;
        let Some(window) = self.window_for(surface.wl_surface()).cloned() else { return };
        if let Some(Ok(mut ruled)) = window.user_data().get::<Ruled>().map(|r| r.0.lock())
            && ruled.float_above != float_above
        {
            ruled.float_above = float_above;
            drop(ruled);
            self.restack();
            self.schedule_redraw();
        }
    }

    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        if surface.is_initial_configure_sent() {
            self.apply_rules(&surface);
        }
    }

    /// Focus stealing (E1.2): a new window takes focus only if it is the only one or the focused app opened it; any
    /// other opens behind the focused window until it activates with a token (see `request_activation`).
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let client = |s: &WlSurface| s.client().map(|c| c.id());
        let focused = self.seat.get_keyboard().and_then(|k| k.current_focus());
        let only = self.space.elements().next().is_none() && self.hidden.is_empty();
        let window = Window::new_wayland_window(surface.clone());
        // The quake terminal opens on its key press, to be typed in.
        let takes_focus = only || focused.as_ref().is_some_and(|f| client(f).is_some() && client(f) == client(surface.wl_surface())) || self.is_quake(&window);
        if takes_focus {
            surface.with_pending_state(|state| state.states.set(xdg_toplevel::State::Activated));
        }
        // Positioned on its first buffer, once its size is known (see commit).
        self.space.map_element(window.clone(), (0, 0), takes_focus);
        if takes_focus {
            self.focus_window(Some(window), SERIAL_COUNTER.next_serial());
        } else if let Some(focused) = self.focused_window() {
            // ponytail: no dock yet, so nothing bounces (M3).
            self.space.raise_element(&focused, false);
            self.restack();
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        self.schedule_redraw();
        self.hidden.retain(|(w, _)| w.toplevel() != Some(&surface));
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

    /// The app's own title bar was dragged.
    fn move_request(&mut self, surface: ToplevelSurface, seat: WlSeat, serial: Serial) {
        let Some(pointer) = Seat::<Self>::from_resource(&seat).and_then(|s| s.get_pointer()) else { return };
        // Only while the button that started it is still held.
        if !pointer.has_grab(serial) {
            return;
        }
        let (Some(start_data), Some(window)) = (pointer.grab_start_data(), self.window_for(surface.wl_surface()).cloned()) else { return };
        // Dragging a zoomed window un-zooms it first, centred under the pointer (macOS).
        if let Some(restore) = self.unfill(&surface, xdg_toplevel::State::Maximized) {
            let at = start_data.location.to_i32_round::<i32>();
            self.set_visible_loc(&window, (at.x - restore.size.w / 2, at.y - crate::decorations::BAR / 2).into());
        }
        self.start_move(window, start_data, serial);
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        self.fill(&surface, xdg_toplevel::State::Maximized);
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        self.unfill(&surface, xdg_toplevel::State::Maximized);
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>) {
        self.fill(&surface, xdg_toplevel::State::Fullscreen);
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        self.unfill(&surface, xdg_toplevel::State::Fullscreen);
    }

    /// The app's own window edge was dragged.
    fn resize_request(&mut self, surface: ToplevelSurface, seat: WlSeat, serial: Serial, edges: xdg_toplevel::ResizeEdge) {
        let Some(pointer) = Seat::<Self>::from_resource(&seat).and_then(|s| s.get_pointer()) else { return };
        if !pointer.has_grab(serial) {
            return;
        }
        let (Some(start_data), Some(window)) = (pointer.grab_start_data(), self.window_for(surface.wl_surface()).cloned()) else { return };
        self.start_resize(window, edges_from(edges), start_data, serial);
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

    /// The app under the pointer picks its cursor; not while the pointer is in one of our zones.
    fn cursor_image(&mut self, _seat: &Seat<Self>, image: smithay::input::pointer::CursorImageStatus) {
        if self.zones.hover.is_none() {
            self.set_cursor(image);
        }
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

    /// Only the focused app may hand out tokens (a click or key in it opened something); we make our own in `spawn`.
    fn token_created(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        let focused = self.seat.get_keyboard().and_then(|k| k.current_focus());
        focused.and_then(|f| f.client()).is_some_and(|c| data.client_id == Some(c.id()))
    }

    /// An app asks to bring a window forward (e.g. a link clicked in the terminal raises Helium).
    /// ponytail: any valid token under ACTIVATION_TIMEOUT is honoured, without checking its input serial.
    fn request_activation(&mut self, _token: XdgActivationToken, token_data: XdgActivationTokenData, surface: WlSurface) {
        if token_data.timestamp.elapsed() >= wm::ACTIVATION_TIMEOUT {
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
