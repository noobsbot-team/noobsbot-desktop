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
            EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction, RegistrationToken, channel,
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

/// Keep on Top (window menu, `keep-on-top`): the window stays above normal windows.
#[derive(Default)]
struct OnTop(std::sync::atomic::AtomicBool);

/// Is `window` kept on top?
pub fn on_top(window: &Window) -> bool {
    window.user_data().get::<OnTop>().is_some_and(|t| t.0.load(std::sync::atomic::Ordering::Relaxed))
}

/// The window rules that matched this window.
struct Ruled(std::sync::Mutex<Actions>);

/// A tiled window tells the app every edge is tiled, so it drops its own shadow and corners.
const TILED: [xdg_toplevel::State; 4] =
    [xdg_toplevel::State::TiledLeft, xdg_toplevel::State::TiledRight, xdg_toplevel::State::TiledTop, xdg_toplevel::State::TiledBottom];

pub(crate) fn ruled(window: &Window) -> Option<Actions> {
    window.user_data().get::<Ruled>().and_then(|r| r.0.lock().ok().map(|a| a.clone()))
}

pub struct State {
    pub start_time: Instant,
    /// When the frame being drawn will be seen (the clock's target vblank), for animations to sample (C32).
    pub frame_time: Instant,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,
    pub loop_signal: LoopSignal,
    pub loop_handle: LoopHandle<'static, Self>,

    pub space: Space<Window>,
    pub popups: PopupManager,
    /// The keyboard layouts and the active one (E6.12).
    pub layouts: crate::ime::Layouts,
    /// Every client's text-input-v3 objects, as far as the emoji picker needs them (E7.2).
    pub text_inputs: crate::ime::TextInputs,
    pub seat: Seat<Self>,
    pub clipboard: Clipboard,
    /// Set by the backend once the renderer exists.
    pub backend: Option<crate::backend::Views>,
    /// The renderer and outputs: a winit window or the real screen.
    pub device: Option<crate::backend::Device>,
    redraw_pending: bool,
    /// Frames keep coming until then (anim.rs).
    pub anim_until: Option<Instant>,
    pub resize: Option<Resize>,
    /// Where the dragged window will tile (E8.5), shown as a glass outline.
    pub tile_preview: Option<Rect>,
    /// Nested in another desktop: Alt stands in for Super, which the host keeps.
    pub nested: bool,
    pub config: Config,
    /// Built from `config.rules`.
    pub rules: Vec<CompiledRule>,
    /// Built from `config.bindings`.
    pub bindings: Vec<(Binding, Action)>,
    /// Settings is recording a shortcut: none matches.
    pub recording: shortcuts::Recording<Window>,
    /// Keys whose press was a shortcut; their release is kept from the app too, so nothing sticks.
    pub suppressed: Vec<Keycode>,
    /// The key a shell surface is seeing held down (D14), and the timer repeating it.
    pub key_repeat: Option<(Keycode, RegistrationToken)>,
    /// Sticky, slow, bounce and mouse keys (E8.6).
    pub access: crate::input::AccessKeys,
    /// Minimised and hidden windows with where they were.
    pub hidden: Vec<(Window, Point<i32, Logical>)>,
    /// The pid of the `noobsbot --quake` the quake action started; its window gets `rules::QUAKE_ID`.
    pub quake: Option<u32>,
    /// The pid of the Orca `[accessibility] screen_reader` started.
    pub screen_reader: Option<u32>,
    /// Our widgets on the accessibility bus while the screen reader is on.
    pub reader_tree: Option<crate::access::Access>,
    /// xwayland-satellite's `DISPLAY` (E6.9), given to every program we start.
    pub x_display: Option<String>,
    /// A newer release, found by Software Update (E6.13).
    pub update: Option<crate::services::update::Release>,
    /// The Weather widget's forecast (E7.2); `None` while it is off.
    pub weather: Option<crate::services::weather::Weather>,
    /// Widgets other processes set over D-Bus (E8.8).
    pub widgets: Vec<crate::services::widgets::Widget>,
    /// Every changed `[weather]` goes to the weather thread.
    pub weather_settings: std::sync::mpsc::Sender<crate::config::Weather>,
    pub zones: crate::input::Zones,
    /// Shake to find, dwell click, hold for secondary click (E8.6).
    pub pointer_assist: crate::pointer_assist::Assist,

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
    /// Shown tray items (E2.2).
    pub tray: Vec<crate::services::tray::TrayItem>,
    /// The open tray menu: service, dbusmenu path and rows.
    pub tray_menu: Option<(String, String, Vec<Option<crate::services::tray::MenuRow>>)>,
    pub disks: crate::shell::disks::Disks,
    pub symbols: crate::symbols::Symbols,
    /// The menu-bar popup that is open (E2.2).
    pub popup: Option<crate::shell::Popup>,
    /// The open menu's row whose submenu is open, while that menu is the popup.
    pub submenu: Option<(crate::shell::Popup, usize)>,
    pub osd: Option<crate::shell::Osd>,
    /// The power alert that is up.
    pub alert: Option<crate::shell::alert::Alert>,
    /// Its once-a-second countdown.
    pub alert_timer: Option<smithay::reexports::calloop::RegistrationToken>,
    /// The Control Center module showing its list.
    pub expanded: Option<crate::shell::control::Module>,
    /// How far that list is scrolled, in logical px.
    pub control_scroll: f64,
    /// The Control Center slider being dragged.
    pub shell_drag: Option<crate::shell::control::Control>,
    /// Hides the OSD; replaced on every key press.
    pub osd_timer: Option<smithay::reexports::calloop::RegistrationToken>,
    /// When the bell last rang (E8.6).
    pub bell: Option<std::time::Instant>,
    /// Caps Lock is on (its OSD and the lock screen's ⇪).
    pub caps_lock: bool,
    /// Puts the display settings back unless they are kept in time (`arm_display_revert`).
    pub display_revert: Option<smithay::reexports::calloop::RegistrationToken>,
    /// Installed apps (E2.1); empty until the first scan lands.
    pub apps: crate::apps::Apps,
    /// Bumped with every `apps` load, so what windows cached against the last one (`dock_key`) is looked up again.
    pub apps_epoch: u64,
    pub icons: crate::apps::Icons,
    /// The pointer touched the bottom edge: the dock shows over the focused window until the pointer leaves it.
    pub dock_reveal: bool,
    /// The pointer is at the top edge (or on the bar it revealed): an auto-hiding menu bar shows.
    pub bar_pointer: bool,
    /// An auto-hiding menu bar sliding out of sight (on) and back.
    pub bar_hide: crate::shell::dock_fx::Ease,
    /// `--show dock-zoom`: a fake pointer position the dock zooms around.
    pub dock_pointer: Option<Point<f64, Logical>>,
    /// The dock's bounces, glow, slide, zoom easing and badges.
    pub dock_fx: crate::shell::dock_fx::Fx,
    /// The dock's right-click menu, icon drag and file drop (E3.1).
    pub dock_ui: crate::shell::dock::Ui,
    /// Desktop icons (E7.2).
    pub desktop: crate::shell::desktop::Desktop,
    /// Notifications, their banners and Notification Center's state (E4.1, E4.2).
    pub notifications: crate::shell::notify::Store,
    /// Calendar events from .ics files (E7.2).
    pub calendar: crate::events::Calendar,
    /// The app switcher (E4.5) and its most-recently-used order.
    pub switcher: crate::shell::switcher::Switcher,
    /// The Force Quit window (E4.7), when open.
    pub force_quit: Option<crate::shell::force_quit::ForceQuit>,
    /// Spotlight (E4.3), when open.
    pub spotlight: Option<crate::shell::spotlight::Spotlight>,
    /// The emoji picker (E7.2), when open.
    pub emoji: Option<crate::shell::emoji::Picker>,
    /// The on-screen keyboard (E8.6).
    pub osk: crate::shell::osk::Osk,
    /// Launchpad (E4.4), while open or fading out.
    pub launchpad: Option<crate::shell::launchpad::Launchpad>,
    /// The app drawer, when open.
    pub drawer: Option<crate::shell::drawer::Drawer>,
    /// Watches for a lone tap of Super, which opens the drawer.
    pub super_tap: crate::shell::drawer::SuperTap,
    /// Spaces (E5.1).
    pub spaces: crate::spaces::Spaces,
    /// Mission Control (E5.2), while up or closing.
    pub mission: Option<crate::mission::Mission>,
    /// Show Desktop is on: the windows pushed aside and the frame spots they go back to (E5.3).
    pub desktop_shown: Option<Vec<(Window, Point<i32, Logical>)>>,
    /// The system touchpad gesture in progress (E5.3).
    pub gesture: Option<crate::gestures::Tracker>,
    /// Hot corners (E5.4).
    pub corners: crate::gestures::Corners,
    /// The lock screen (E6.4), while locked.
    pub lock: Option<crate::shell::lock::Lock>,
    /// Idle timers, idle-notify, idle-inhibit and session-lock globals (E6.5).
    pub idle: crate::idle::Idle,
    /// Night Light's fade and the Control Center's override (E8.4).
    pub night_light: crate::render::NightLight,
    /// Zoom and colour filters (E8.6).
    pub zoom: crate::render::Zoom,
    /// The screenshot overlay, toolbar and thumbnail (E6.8).
    pub screenshots: crate::shell::screenshot::Screenshots,
    /// wlr-screencopy for other programs (E6.8, E6.11).
    pub capture: crate::capture::Capture,
}

/// A window's program name, kept on it (`State::app_id`'s fallback, asked for on every pointer motion over the dock).
struct Process(Option<String>);

/// The file name of what `pid` runs: its `exe` link, else the first word of its command line (`comm` would cut
/// `a-system-monitor` at 15 chars).
pub fn program_name(pid: i32) -> Option<String> {
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok().and_then(|p| Some(p.file_name()?.to_str()?.trim_end_matches(" (deleted)").to_owned()));
    let argv0 = || {
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        Some(std::str::from_utf8(cmdline.split(|b| *b == 0).next()?).ok()?.rsplit('/').next()?.to_owned())
    };
    exe.or_else(argv0).filter(|n| !n.is_empty())
}

impl State {
    pub fn new(event_loop: &mut EventLoop<'static, Self>, display: Display<Self>, nested: bool, config: Config) -> Result<Self, Box<dyn std::error::Error>> {
        let dh = display.handle();

        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        let keyboard = config.keyboard.resolved();
        let (rate, delay) = config.keyboard.repeat();
        if let Err(err) = seat.add_keyboard(crate::input::xkb(&keyboard), delay, rate) {
            // A typo in the layout must not stop the desktop: fall back to xkb's default (us).
            tracing::warn!(?keyboard, "keyboard layout: {err}");
            seat.add_keyboard(Default::default(), delay, rate)?;
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
        // `noobsbot-desktop --trusted <command>` connects here (E6.11, perms.rs).
        let trusted = ListeningSocketSource::with_name(&crate::perms::trusted_socket(&socket_name).to_string_lossy())?;
        handle.insert_source(trusted, |stream, _, state| {
            if let Err(err) = state.display_handle.insert_client(stream, Arc::new(ClientState { trusted: true, ..Default::default() })) {
                tracing::warn!("rejected trusted client: {err}");
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
        // App and tray icons are rasterised on their own thread too (C6): each one landing asks for a frame.
        let (icons_tx, icons_rx) = channel::channel();
        handle.insert_source(icons_rx, |event, _, state| {
            if let channel::Event::Msg(done) = event {
                state.icons.receive(done);
                state.schedule_redraw();
            }
        })?;
        let (services_tx, services_rx) = channel::channel();
        let services = crate::services::spawn(services_tx, !nested);
        // Nested is a dev session: no network.
        if !nested && config.check_updates {
            let (found_tx, found_rx) = channel::channel();
            crate::services::update::spawn(found_tx, config.update.channel);
            handle.insert_source(found_rx, |event, _, state| {
                if let channel::Event::Msg(release) = event {
                    state.notify(crate::services::update::notice(&release));
                    state.update = Some(release);
                    state.schedule_redraw();
                }
            })?;
        }
        // Off by default (only `[weather] enabled` fetches); also nested, so it can be tried in a dev session.
        let (weather_tx, weather_rx) = channel::channel();
        let weather_settings = crate::services::weather::spawn(config.weather.clone(), weather_tx);
        handle.insert_source(weather_rx, |event, _, state| {
            if let channel::Event::Msg(weather) = event {
                state.weather = weather;
                state.schedule_redraw();
            }
        })?;
        if !config.clock.timezone.is_empty() {
            services.send(crate::services::Command::Timezone(config.clock.timezone.clone()));
        }
        handle.insert_source(services_rx, |event, _, state| {
            if let channel::Event::Msg(update) = event {
                // Only the changed menu-bar item gets a new buffer, so the redraw damages just that item.
                match update {
                    crate::services::Update::Battery(battery) => state.battery_changed(battery),
                    crate::services::Update::Network(network) => state.network = network,
                    crate::services::Update::Bluetooth(bluetooth) => state.bluetooth = bluetooth,
                    crate::services::Update::Profiles(profiles) => state.profiles = profiles,
                    crate::services::Update::Brightness(brightness) => state.brightness = brightness,
                    crate::services::Update::Sound(sound) => state.sound = sound,
                    crate::services::Update::Media(media) => state.media = media,
                    crate::services::Update::Pairing(pairing) => state.show_pairing(pairing),
                    crate::services::Update::WifiSecret(ssid) => state.show_wifi_secret(ssid),
                    crate::services::Update::PinEntry { device, passkey } => state.show_pin_entry(device, passkey),
                    crate::services::Update::Polkit(event) => state.polkit_event(event),
                    crate::services::Update::Tray(tray) => state.tray = tray,
                    crate::services::Update::TrayMenu { service, menu, parent, rows } => state.show_tray_menu(service, menu, parent, rows),
                    crate::services::Update::Notify(n) => state.notify(*n),
                    crate::services::Update::CloseNotification(id) => state.close_notification(id, crate::shell::notify::Reason::Closed),
                    crate::services::Update::History(list) => state.restore_notifications(list),
                    crate::services::Update::Disks(volumes) => state.disks_changed(volumes),
                    crate::services::Update::KbdBacklight(percent) => {
                        state.show_osd(crate::shell::Osd { symbol: crate::symbols::Symbol::Keyboard, level: Some(percent), text: None });
                    }
                    crate::services::Update::DiskFailed { name, error } => state.disk_failed(name, error),
                    crate::services::Update::Launcher { app, entry } => state.dock_fx.entry(app, entry),
                    crate::services::Update::PrepareForSleep => state.prepare_for_sleep(),
                    crate::services::Update::Resumed => state.idle_activity(),
                    crate::services::Update::Fcitx(methods) => state.layouts.methods = methods,
                    crate::services::Update::Widgets(widgets) => state.widgets = widgets,
                    crate::services::Update::Record(on) => {
                        let focus = state.focused_window();
                        state.recording.set(on, focus, std::time::Instant::now());
                    }
                }
                state.schedule_redraw();
            }
        })?;
        // The app index (dock, Spotlight, Launchpad) loads off the frame thread and follows installs.
        let (apps_tx, apps_rx) = channel::channel();
        crate::apps::watch(move |apps| apps_tx.send(apps).is_ok());
        handle.insert_source(apps_rx, |event, _, state| {
            if let channel::Event::Msg(apps) = event {
                state.apps = apps;
                state.apps_epoch += 1;
                // An install brings icons along; a cached miss (or an old look) would stick until a restart.
                state.icons.clear();
                state.schedule_redraw();
            }
        })?;
        // The menu-bar clock (E2.4): one redraw per minute (second, with seconds on), on the boundary.
        let tick = |state: &Self| crate::clock::until_next_tick(&chrono::Local::now(), state.config.clock.format.seconds);
        // It also moves `auto` dark mode between day and night, apps included.
        let mut was_dark = crate::render::is_dark(&config.appearance);
        handle.insert_source(Timer::from_duration(crate::clock::until_next_tick(&chrono::Local::now(), config.clock.format.seconds)), move |_, _, state| {
            let appearance = &state.config.appearance;
            let dark = crate::render::is_dark(appearance);
            if dark != was_dark && appearance.mode == crate::config::Mode::Auto && appearance.theme_apps {
                crate::theme::write_in_background(appearance.clone(), !state.nested);
            }
            was_dark = dark;
            state.schedule_redraw();
            TimeoutAction::ToDuration(tick(state))
        })?;

        Ok(Self {
            start_time: Instant::now(),
            frame_time: Instant::now(),
            socket_name,
            loop_signal: event_loop.get_signal(),
            loop_handle: handle.clone(),
            space: Space::default(),
            popups: PopupManager::default(),
            layouts: Default::default(),
            text_inputs: Default::default(),
            seat,
            clipboard: Clipboard::new(clipboard_tx),
            backend: None,
            device: None,
            redraw_pending: false,
            anim_until: None,
            resize: None,
            tile_preview: None,
            nested,
            x_display: None,
            update: None,
            weather: None,
            widgets: Vec::new(),
            weather_settings,
            rules: rules::compile(&config.rules),
            bindings: shortcuts::from_config(&config),
            recording: Default::default(),
            config,
            suppressed: Vec::new(),
            key_repeat: None,
            access: Default::default(),
            hidden: Vec::new(),
            quake: None,
            screen_reader: None,
            reader_tree: None,
            zones: Default::default(),
            pointer_assist: Default::default(),
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
            tray: Vec::new(),
            tray_menu: None,
            submenu: None,
            disks: Default::default(),
            symbols: Default::default(),
            popup: None,
            osd: None,
            alert: None,
            alert_timer: None,
            expanded: None,
            control_scroll: 0.0,
            shell_drag: None,
            osd_timer: None,
            bell: None,
            caps_lock: false,
            display_revert: None,
            apps: Default::default(),
            apps_epoch: 0,
            icons: crate::apps::Icons::new(icons_tx),
            dock_reveal: false,
            bar_pointer: false,
            bar_hide: crate::shell::dock_fx::Ease::new(crate::shell::dock_fx::HIDE_TIME, true),
            dock_pointer: None,
            dock_fx: Default::default(),
            dock_ui: Default::default(),
            desktop: Default::default(),
            notifications: Default::default(),
            calendar: Default::default(),
            switcher: Default::default(),
            force_quit: None,
            spotlight: None,
            emoji: None,
            osk: Default::default(),
            launchpad: None,
            drawer: None,
            super_tap: Default::default(),
            spaces: Default::default(),
            mission: None,
            desktop_shown: None,
            gesture: None,
            corners: Default::default(),
            lock: None,
            idle: crate::idle::Idle::new(&dh, handle.clone()),
            night_light: Default::default(),
            zoom: Default::default(),
            screenshots: Default::default(),
            capture: crate::capture::Capture::new(&dh),
            display_handle: dh,
        })
    }

    /// Draws a frame at the next vblank. Called only when something changed, so an idle desktop costs no CPU.
    /// The frame starts as late as the frame clock allows (E1.10), so input arriving meanwhile is still in it.
    pub fn schedule_redraw(&mut self) {
        // The real screen keeps a frame clock per output.
        if let Some(crate::backend::Device::Udev(udev)) = self.device.as_mut() {
            return udev.redraw_all(&self.loop_handle);
        }
        if self.redraw_pending {
            return;
        }
        self.redraw_pending = true;
        let delay = match self.device.as_mut() {
            Some(crate::backend::Device::Winit(winit)) => winit.clock.delay(Instant::now()),
            _ => std::time::Duration::ZERO,
        };
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

    /// What the menu bar shows; `None` while a fullscreen window has focus (it covers the bar).
    pub fn menu_bar(&self) -> Option<crate::render::menubar::Content> {
        use crate::render::menubar::{self, Item};
        let focused = self.focused_window();
        let toplevel = focused.as_ref().and_then(|w| w.toplevel());
        if toplevel.is_some_and(|t| t.current_state().states.contains(xdg_toplevel::State::Fullscreen)) {
            return None;
        }
        let app = focused.as_ref().and_then(|w| self.app_key(w));
        let simple = |symbol| Item { symbol, text: None };
        let status = [
            self.network.as_ref().map(|n| simple(menubar::wifi(n))),
            self.bluetooth.as_ref().map(|b| simple(menubar::bluetooth(b))),
            self.battery.as_ref().map(menubar::battery),
            self.sound.as_ref().map(|s| simple(menubar::volume(s))),
        ];
        let tray = self.tray.iter().map(|t| t.icon.clone()).collect();
        Some(menubar::Content {
            app,
            tray,
            status: status.into_iter().flatten().collect(),
            input: self.input_item(),
            clock: crate::clock::text(&chrono::Local::now(), self.config.clock.format),
        })
    }

    /// The bar on `output`: the focused app's name only on its window's display, tray icons only on the first one.
    // ponytail: the other displays' bars are drawn but take no clicks, as popups and hit tests live on the first
    // output; per-display popups need the shell's caches per output.
    pub fn bar_content(&self, output: &smithay::output::Output) -> Option<crate::render::menubar::Content> {
        let mut content = self.menu_bar()?;
        let first = self.space.outputs().next() == Some(output);
        let on = self.focused_window().and_then(|w| self.window_output(&w));
        if on.as_ref().map_or(!first, |o| o != output) {
            content.app = None;
        }
        if !first {
            content.tray.clear();
        }
        Some(content)
    }

    /// The window's app id; noobsbot and others set none, so then its program's name, read once per window.
    pub fn app_id(&self, window: &Window) -> Option<String> {
        let t = window.toplevel()?;
        let id = with_states(t.wl_surface(), |s| s.data_map.get::<XdgToplevelSurfaceData>().and_then(|d| d.lock().ok()?.app_id.clone()));
        id.filter(|id| !id.is_empty()).or_else(|| window.user_data().get_or_insert(|| Process(self.pid(window).and_then(program_name))).0.clone())
    }

    /// The process behind a window, from its Wayland connection.
    pub fn pid(&self, window: &Window) -> Option<i32> {
        Some(window.toplevel()?.wl_surface().client()?.get_credentials(&self.display_handle).ok()?.pid)
    }

    /// Where windows may go: the first output minus the menu bar and what layer surfaces (bars, docks) reserve.
    /// ponytail: a zoomed window keeps its size when a bar appears later; refitting comes with output changes (M6).
    pub fn work_area(&self) -> Option<Rect> {
        self.output_area(self.space.outputs().next()?)
    }

    /// The work area of `output`: below its menu bar and its layer surfaces, above the dock on the first output.
    pub fn output_area(&self, output: &smithay::output::Output) -> Option<Rect> {
        let mut zone = layer_map_for_output(output).non_exclusive_zone();
        if !self.config.menu_bar.auto_hide {
            zone = crate::render::menubar::below(zone);
        }
        // Above the dock (E1.2: zoom fills the screen between the menu bar and the dock), when it never hides: Plank
        // sets struts in that mode alone, and a hiding dock gets overlapped. The dock is on the first output only.
        let first = self.space.outputs().next() == Some(output);
        let never = first && self.config.dock.hide == crate::config::DockHide::Never;
        let bottom = self.space.output_geometry(output)?.size.h - if never { crate::shell::dock::reserve(&self.config.dock) } else { 0 };
        zone.size.h = zone.size.h.min(bottom - zone.loc.y).max(0);
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
        let restored = !self.space.elements().any(|w| w == window);
        let old = self.space.element_location(window);
        let new = loc + Point::from((0, crate::decorations::bar_height(window)));
        self.space.map_element(window.clone(), new, false);
        if let Some(old) = old.filter(|old| *old != new) {
            self.carry_sheets(window, new - old);
        }
        for w in &above {
            self.space.raise_element(w, false);
        }
        self.restack();
        if restored {
            self.window_restored(window);
            crate::shell::dock::set_minimised(window, false);
        }
        self.schedule_redraw();
    }

    /// Starts moving `window` with the pointer; the move begins after the drag threshold.
    pub fn start_move(&mut self, window: Window, start_data: smithay::input::pointer::GrabStartData<Self>, serial: Serial) {
        let Some(pointer) = self.seat.get_pointer() else { return };
        // A sheet is dragged by its parent.
        let window = self.sheet_parent(&window).unwrap_or(window);
        let Some(start) = self.frame(&window) else { return };
        pointer.set_grab(self, MoveGrab { start_data, window, start, started: false, tile: None }, serial, Focus::Clear);
    }

    /// Tiles `window` (E8.5), remembering where it was; dragging it away puts it back (`untile`).
    pub fn tile(&mut self, window: &Window, tile: crate::wm::Tile) {
        let Some(surface) = window.toplevel().cloned() else { return };
        let rect = match tile {
            crate::wm::Tile::Zoom => return self.fill(&surface, xdg_toplevel::State::Maximized),
            crate::wm::Tile::Part(rect) => rect,
        };
        self.window_reframing(window);
        let restore = window.user_data().get_or_insert(|| Restore(std::sync::Mutex::new(None)));
        if let (Ok(mut saved), Some(frame)) = (restore.0.lock(), self.frame(window)) {
            saved.get_or_insert(frame);
        }
        surface.with_pending_state(|s| TILED.iter().for_each(|t| _ = s.states.set(*t)));
        let size = self.content_size(window, rect.size);
        surface.with_pending_state(|s| s.size = Some(size));
        surface.send_pending_configure();
        self.set_visible_loc(window, rect.loc);
    }

    /// A tiled window being dragged away gets its old size back, its title bar under the pointer at `at`; returns its
    /// new rectangle.
    pub fn untile(&mut self, window: &Window, at: Point<f64, Logical>) -> Option<Rect> {
        let surface = window.toplevel()?;
        if !surface.with_pending_state(|s| s.states.contains(xdg_toplevel::State::TiledLeft)) {
            return None;
        }
        self.window_reframing(window);
        let saved = window.user_data().get::<Restore>().and_then(|r| r.0.lock().ok()?.take());
        surface.with_pending_state(|s| TILED.iter().for_each(|t| _ = s.states.unset(*t)));
        surface.with_pending_state(|s| s.size = saved.map(|r| self.content_size(window, r.size)));
        surface.send_pending_configure();
        let saved = saved?;
        let at = at.to_i32_round::<i32>();
        let rect = Rect::new((at.x - saved.size.w / 2, at.y - crate::decorations::BAR / 2).into(), saved.size);
        self.set_visible_loc(window, rect.loc);
        Some(rect)
    }

    /// Super+Ctrl+arrows, Window ▸ Move & Resize: tiles `window` against `edges` of the work area (none: Fill), the
    /// same tile a drag gives, so dragging it away restores it. A zoomed window keeps its pre-zoom size to go back to.
    /// ponytail: does nothing to a full-screen window.
    pub fn tile_to(&mut self, window: &Window, edges: wm::Edges) {
        let (Some(surface), Some(area)) = (window.toplevel().cloned(), self.work_area()) else { return };
        if surface.with_pending_state(|s| s.states.contains(xdg_toplevel::State::Fullscreen)) {
            return;
        }
        surface.with_pending_state(|s| s.states.unset(xdg_toplevel::State::Maximized));
        self.tile(window, wm::Tile::Part(wm::tile_part(area, edges)));
    }

    /// Window ▸ Center: the middle of the work area, at the same size.
    pub fn center(&mut self, window: &Window) {
        let (Some(frame), Some(area)) = (self.frame(window), self.work_area()) else { return };
        self.set_visible_loc(window, wm::center(area, frame.size));
    }

    /// Window ▸ Keep on Top, toggled.
    pub fn keep_on_top(&mut self, window: &Window) {
        let flag = &window.user_data().get_or_insert_threadsafe(OnTop::default).0;
        flag.fetch_xor(true, std::sync::atomic::Ordering::Relaxed);
        self.restack();
        self.schedule_redraw();
    }

    /// Move to ▸ Display: the next (`delta` 1) or previous (-1) display, at the same place on it.
    /// ponytail: the size is kept, so a zoomed or tiled window doesn't refit a display of another size.
    pub fn move_to_display(&mut self, window: &Window, delta: i32) {
        let outputs: Vec<_> = self.space.outputs().cloned().collect();
        let Some(from) = self.window_output(window).and_then(|o| outputs.iter().position(|x| *x == o)) else { return };
        let to = (from as i32 + delta).rem_euclid(outputs.len() as i32) as usize;
        let area = |i: usize| self.output_area(&outputs[i]);
        let (Some(a), Some(b), Some(frame)) = (area(from), area(to), self.frame(window)) else { return };
        if to == from {
            return;
        }
        let rect = Rect::new(b.loc + (frame.loc - a.loc), frame.size);
        self.set_visible_loc(window, wm::move_to(rect, b, &[], false));
    }

    /// Zoom (fill the work area) or fullscreen (fill the output), remembering where the window was.
    /// ponytail: one output and no menu bar or dock yet, so both fill the same area; refitting on output resize comes with udev (M6).
    pub fn fill(&mut self, surface: &ToplevelSurface, state: xdg_toplevel::State) {
        let Some(window) = self.window_for(surface.wl_surface()).cloned() else { return };
        // The display the window is on, so zoom fills that one.
        let Some(area) = self.window_output(&window).map_or_else(|| self.work_area(), |o| self.output_area(&o)) else { return };
        self.window_reframing(&window);
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
        if state == xdg_toplevel::State::Fullscreen {
            self.fullscreen_space(&window);
        }
    }

    /// Leaves zoom or fullscreen; returns where the window went back to.
    pub fn unfill(&mut self, surface: &ToplevelSurface, state: xdg_toplevel::State) -> Option<Rect> {
        let window = self.window_for(surface.wl_surface()).cloned()?;
        if !surface.with_pending_state(|s| s.states.contains(state)) {
            return None;
        }
        self.window_reframing(&window);
        let saved = window.user_data().get::<Restore>().and_then(|r| r.0.lock().ok()?.take());
        surface.with_pending_state(|s| s.states.unset(state));
        let size = saved.map(|r| self.content_size(&window, r.size));
        surface.with_pending_state(|s| s.size = size);
        surface.send_pending_configure();
        if state == xdg_toplevel::State::Fullscreen {
            self.leave_fullscreen_space(&window);
        }
        if let Some(rect) = saved {
            self.set_visible_loc(&window, rect.loc);
        }
        saved
    }

    /// Where a tiled, zoomed or full-screen window goes back to.
    pub fn restore_frame(&self, window: &Window) -> Option<Rect> {
        *window.user_data().get::<Restore>()?.0.lock().ok()?
    }

    pub fn window_for(&self, surface: &WlSurface) -> Option<&Window> {
        self.space.elements().find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
    }

    /// Raises and activates `window`, giving it the keyboard; `None` clears focus.
    pub fn focus_window(&mut self, window: Option<Window>, serial: Serial) {
        let Some(keyboard) = self.seat.get_keyboard() else { return };
        // While locked, no app gets the keyboard (a client lock screen keeps it).
        if self.locked() {
            return;
        }
        // A layer surface with exclusive keyboard (a launcher, a lock screen) keeps it while it is up.
        let exclusive = self.exclusive_layer().map(|l| l.wl_surface().clone());
        match window {
            Some(window) => {
                // A parent's focus goes to its sheet, the two raised together.
                let window = match self.sheet_of(&window) {
                    Some(sheet) => {
                        self.space.raise_element(&window, false);
                        sheet
                    }
                    None => window,
                };
                // Any window taking focus ends Show Desktop.
                self.restore_desktop();
                self.reveal(&window);
                self.switcher.touch(&window);
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

    /// Gives `window` the keyboard and the active look, leaving the stacking alone (hover focus).
    pub fn focus_unraised(&mut self, window: &Window) {
        let Some(keyboard) = self.seat.get_keyboard() else { return };
        if self.locked() || self.exclusive_layer().is_some() {
            return;
        }
        self.switcher.touch(window);
        self.space.elements().for_each(|w| _ = w.set_activated(w == window));
        keyboard.set_focus(self, window.toplevel().map(|t| t.wl_surface().clone()), SERIAL_COUNTER.next_serial());
        self.space.elements().filter_map(|w| w.toplevel()).filter(|t| t.is_initial_configure_sent()).for_each(|t| _ = t.send_pending_configure());
    }

    /// Window rules (E1.9), resolved before the first configure so the app opens at the ruled size, and again if the
    /// app sets its app_id only after that (then the window is resized and moved where the rule says).
    /// `layer` is kept by `restack`, `decorations` by decorations.rs.
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
    pub(crate) fn restack(&mut self) {
        let is = |w: &Window, f: fn(&Actions) -> bool| ruled(w).is_some_and(|a| f(&a));
        let float = |a: &Actions| a.float_above == Some(true);
        let overlay = |a: &Actions| a.layer == Some(rules::Layer::Overlay);
        let above: Vec<Window> =
            self.space.elements().filter(|w| is(w, float) || on_top(w)).chain(self.space.elements().filter(|w| is(w, overlay))).cloned().collect();
        for window in above {
            self.space.raise_element(&window, false);
        }
        self.stack_sheets();
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
    /// Connected through the trusted socket (perms.rs): may record the screen without asking.
    pub trusted: bool,
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
                if let Some(window) = self.window_for(surface).cloned() {
                    self.recall_frame(&window);
                }
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
                        let loc = self.attached_loc(&window, size).unwrap_or_else(|| {
                            position.map_or_else(|| wm::place(area, size), |[x, y]| area.loc + Point::from((x.to_px(area.size.w), y.to_px(area.size.h))))
                        });
                        tracing::debug!(?size, ?loc, ?position, "placed");
                        self.set_visible_loc(&window, loc);
                        self.apply_space_rule(&window);
                        self.window_opened(&window);
                    }
                }
            }
            self.resize_commit(&root);
        }

        self.layer_commit(surface);
        self.popups.commit(surface);
        self.ime_commit(surface);
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

    fn client_pong(&mut self, client: smithay::wayland::shell::xdg::ShellClient) {
        self.pong(&client);
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
        let only = !self.space.elements().any(|w| self.on_current(w)) && self.hidden.is_empty();
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
            // It wants attention: its dock icon bounces (or glows while the dock is hidden) until it is focused.
            self.dock_fx.urgent(window);
            self.space.raise_element(&focused, false);
            self.restack();
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        self.schedule_redraw();
        if let Some(window) = self.window_for(surface.wl_surface()).cloned() {
            self.remember_frame(&window);
        }
        // Its buffers are still there unless the client is gone (then it just vanishes).
        if let Some(window) =
            self.window_for(surface.wl_surface()).filter(|_| smithay::reexports::wayland_server::Resource::is_alive(surface.wl_surface())).cloned()
        {
            self.window_closing(&window);
        }
        self.hidden.retain(|(w, _)| w.toplevel() != Some(&surface));
        if let Some(window) = self.window_for(surface.wl_surface()).cloned() {
            self.leave_fullscreen_space(&window);
        }
        let next = self.space.elements().rev().find(|w| w.toplevel() != Some(&surface) && self.on_current(w)).cloned();
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
        // Dragging a zoomed window un-zooms it first, centred under the pointer.
        if let Some(restore) = self.unfill(&surface, xdg_toplevel::State::Maximized) {
            let at = start_data.location.to_i32_round::<i32>();
            self.set_visible_loc(&window, (at.x - restore.size.w / 2, at.y - crate::decorations::BAR / 2).into());
        }
        self.start_move(window, start_data, serial);
    }

    /// The app's own minimise button (GTK4, libadwaita).
    fn minimize_request(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.window_for(surface.wl_surface()).cloned() {
            self.minimise(vec![window]);
        }
    }

    /// The app's own title bar was right-clicked: our window menu, at `location` on its surface.
    fn show_window_menu(&mut self, surface: ToplevelSurface, _seat: WlSeat, _serial: Serial, location: Point<i32, Logical>) {
        let Some(window) = self.window_for(surface.wl_surface()).cloned() else { return };
        let Some(origin) = self.space.element_location(&window).map(|l| l - window.geometry().loc) else { return };
        self.open_window_menu(window, self.to_output((origin + location).to_f64()));
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
        // Locked (E6.4): no popup takes the keyboard from the lock screen.
        if self.locked() {
            grab.ungrab(PopupUngrabStrategy::All);
            return;
        }

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

    /// Caps Lock turned on or off: the OSD says so (E8.6).
    fn led_state_changed(&mut self, _seat: &Seat<Self>, led_state: smithay::input::keyboard::LedState) {
        let caps = led_state.caps.unwrap_or(false);
        if caps != std::mem::replace(&mut self.caps_lock, caps) {
            let text = if caps { "Caps Lock On" } else { "Caps Lock Off" };
            self.show_osd(crate::shell::Osd { symbol: crate::symbols::Symbol::Keyboard, level: None, text: Some(text.into()) });
        }
    }

    /// The clipboard follows the keyboard: only the focused app may read it.
    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = &self.display_handle;
        self.clipboard.adopt_if_orphaned(dh, seat);
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client.clone());
        set_primary_focus(dh, seat, client);
        self.text_input_focus(focused);
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

/// The dock is a drop target: files open with the app they land on, `.desktop` files pin (E3.1).
impl ClientDndGrabHandler for State {
    fn started(
        &mut self,
        source: Option<smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource>,
        _icon: Option<WlSurface>,
        _seat: Seat<Self>,
    ) {
        self.dock_drag_started(source);
    }

    fn dropped(&mut self, _target: Option<WlSurface>, validated: bool, _seat: Seat<Self>) {
        if !validated {
            self.dock_drop();
        }
    }
}
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
        if self.device.as_mut().and_then(crate::backend::Device::renderer).is_some_and(|r| r.import_dmabuf(&dmabuf, None).is_ok()) {
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
            // Too late to take focus: the app asks for attention in the dock instead.
            if let Some(window) = self.window_for(&surface).cloned() {
                self.dock_fx.urgent(window);
                self.schedule_redraw();
            }
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
