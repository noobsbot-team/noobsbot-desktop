//! The real screen (E6.1–E6.3): libseat (device access without root, VT switching), udev (GPU and monitor hotplug),
//! DRM/KMS outputs through `DrmCompositor` (direct scanout of fullscreen apps, hardware cursor plane), libinput, and
//! GBM/EGL drawing on the primary GPU through `MultiRenderer`, so a monitor wired to another GPU (a laptop's dGPU
//! HDMI port) gets the frame copied across.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        SwapBuffersError,
        allocator::{
            Fourcc,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmEventMetadata, DrmEventTime, DrmNode, NodeType,
            compositor::{DrmCompositor, FrameFlags, RenderFrameError},
            exporter::gbm::GbmFramebufferExporter,
        },
        egl::EGLError,
        input::{AbsolutePositionEvent, Event, InputEvent, PointerMotionEvent, Switch, SwitchState, SwitchToggleEvent},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            Bind, ImportDma, Offscreen, Renderer, RendererSuper, Texture,
            damage::{Error as OutputDamageTrackerError, OutputDamageTracker},
            element::{
                Element, Id, Kind, RenderElement, UnderlyingStorage,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                surface::render_elements_from_surface_tree,
                texture::TextureRenderElement,
                utils::{Relocate, RelocateRenderElement, RescaleRenderElement},
            },
            gles::{GlesRenderer, GlesTexture},
            multigpu::{ApiDevice, Error as MultiError, GpuManager, MultiRenderer, gbm::GbmGlesBackend},
            utils::{CommitCounter, DamageSet, OpaqueRegions},
        },
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::{UdevBackend, UdevEvent, all_gpus, primary_gpu},
    },
    input::{
        keyboard::Keysym,
        pointer::{CursorIcon, CursorImageStatus, CursorImageSurfaceData},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            EventLoop, LoopHandle, RegistrationToken,
            timer::{TimeoutAction, Timer},
        },
        drm::control::{self as drm_control, Device as _, ModeTypeFlags, ResourceHandles, connector, crtc},
        input::{AccelProfile, ClickMethod, Device as InputDevice, DeviceCapability, Libinput, SendEventsMode},
        rustix::fs::OFlags,
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::{DisplayHandle, backend::GlobalId},
    },
    utils::{Buffer, DeviceFd, Logical, Monotonic, Physical, Point, Rectangle, Scale, Size, Time, Transform},
    wayland::{compositor::with_states, presentation::Refresh},
};

use super::{CLEAR, Device, Views, cursor};
use crate::{
    config::{Mouse, ScrollMethod, SecondaryClick, Touchpad},
    display_info::{self, Display},
    displays,
    frame::FrameClock,
    render::{self, SceneElement},
    state::State,
};

type Api = GbmGlesBackend<GlesRenderer, DrmDeviceFd>;
type Multi<'a> = MultiRenderer<'a, 'a, Api, Api>;
type Compositor =
    DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, Option<smithay::desktop::utils::OutputPresentationFeedback>, DrmDeviceFd>;
type Error = Box<dyn std::error::Error>;
/// Named cursors by (icon, px size): picture, hotspot and size in physical px; `None`: the theme has none.
type Cursors = HashMap<(CursorIcon, u32), Option<(MemoryRenderBuffer, Point<i32, Physical>, Size<i32, Physical>)>>;

/// 8-bit only in v1 (no HDR or 10-bit colour, E6.2).
const FORMATS: [Fourcc; 2] = [Fourcc::Argb8888, Fourcc::Xrgb8888];

pub struct Udev {
    session: LibSeatSession,
    libinput: Libinput,
    /// The render node everything is drawn on.
    primary: DrmNode,
    gpus: GpuManager<Api>,
    cards: HashMap<DrmNode, Card>,
    cursors: Cursors,
    /// Plugged-in input devices, so a config reload reaches them at once.
    inputs: Vec<InputDevice>,
    /// When the GPU was last rebuilt (`gpu_verdict`).
    resets: Vec<Instant>,
    settle: Settle,
}

/// One GPU and the displays wired to it.
struct Card {
    drm: DrmDevice,
    gbm: GbmDevice<DrmDeviceFd>,
    render: DrmNode,
    token: RegistrationToken,
    screens: HashMap<crtc::Handle, Screen>,
    /// Every connected display, lit or off (`[[display.outputs]] enabled = false`), as of the last scan.
    connected: Vec<(connector::Info, Display)>,
}

/// One lit display.
struct Screen {
    output: Output,
    global: GlobalId,
    connector: connector::Handle,
    compositor: Compositor,
    clock: FrameClock,
    phase: Phase,
    /// Asked to redraw while a frame was on its way: draw again after the vblank.
    again: bool,
    render_time: Duration,
    /// The connector whose picture it shows (mirroring), and that picture as last drawn.
    mirror_of: Option<String>,
    mirrored: Option<Mirrored>,
    /// Percent the frame is drawn smaller, centred (`underscan`).
    underscan: u8,
    /// Frames in a row that failed to draw (`Fail::Render`), and whether the current run of failures was logged.
    failures: u32,
    failed: bool,
}

/// A mirror's copy of its source's frame: drawn offscreen at the source's size, redrawn where the source changed.
struct Mirrored {
    texture: GlesTexture,
    damage: OutputDamageTracker,
    /// New whenever the picture changed, so the mirror's own frame redraws it.
    id: Id,
}

/// Failed frames in a row, on every display, before they count as a GPU reset (EGL said nothing of a lost context).
const FAILURES: u32 = 3;
/// Rebuilds within `RESET_WINDOW` before giving up.
const MAX_RESETS: usize = 3;
const RESET_WINDOW: Duration = Duration::from_secs(60);

/// Why a frame failed.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Fail {
    /// EGL says the context is lost: a GPU reset.
    Lost,
    /// Drawing failed otherwise: a reset only if it keeps failing on every display.
    Render,
    /// Worth another go at once (no free buffer, an import across GPUs).
    Temporary,
    /// This display's scanout (DRM, allocator, a GPU gone): it stays dark until its next frame.
    Display,
}

/// What a failed frame calls for.
#[derive(Debug, PartialEq)]
enum Gpu {
    /// Leave this display be.
    Skip,
    /// Try the next frame as is.
    Retry,
    /// Remake the EGL contexts, the shell's textures and the scanout buffers.
    Rebuild,
    /// Exit non-zero: the session wrapper restarts or rolls back.
    GiveUp,
}

/// A frame failed: `failures` the fewest failed frames in a row of any lit display, `resets` when the GPU was
/// rebuilt before.
// ponytail: Smithay 0.7 makes its EGL contexts without EGL_EXT_create_context_robustness, so a reset is seen only
// as failing frames (or EGL_CONTEXT_LOST); a driver that keeps "succeeding" after a reset draws garbage until restart.
fn gpu_verdict(fail: Fail, failures: u32, resets: &[Instant], now: Instant) -> Gpu {
    match fail {
        Fail::Display => return Gpu::Skip,
        Fail::Temporary => return Gpu::Retry,
        Fail::Render if failures < FAILURES => return Gpu::Retry,
        Fail::Render | Fail::Lost => {}
    }
    match resets.iter().filter(|t| now.duration_since(**t) < RESET_WINDOW).count() >= MAX_RESETS {
        true => Gpu::GiveUp,
        false => Gpu::Rebuild,
    }
}

/// Sorts a renderer error. Smithay calls many errors `ContextLost`; only EGL_CONTEXT_LOST is a GPU reset.
fn multi_fail(err: MultiError<Api, Api>) -> Fail {
    match err {
        MultiError::NoDevice(_) | MultiError::DeviceMissing | MultiError::AllocatorError(_) => Fail::Display,
        err => match SwapBuffersError::from(err) {
            SwapBuffersError::ContextLost(err) => {
                match std::iter::successors(Some(&*err as &(dyn std::error::Error + 'static)), |e| e.source())
                    .any(|e| matches!(e.downcast_ref(), Some(EGLError::ContextLost)))
                {
                    true => Fail::Lost,
                    false => Fail::Render,
                }
            }
            _ => Fail::Temporary,
        },
    }
}

/// How long connectors must stay unchanged before hotplug re-plans the displays: a dock lights its monitors one
/// after the other, and each would otherwise move the windows (and pick a remembered layout) on its own.
const SETTLE: Duration = Duration::from_millis(500);

/// Connector hotplug waiting for `SETTLE`.
#[derive(Default)]
struct Settle {
    /// The cards (`dev_t`) whose connectors changed.
    cards: Vec<libc::dev_t>,
    /// The last change; `None`: no timer runs.
    last: Option<Instant>,
}

impl Settle {
    /// A card's connectors changed: true when no timer runs yet, so one must start.
    fn changed(&mut self, card: libc::dev_t, now: Instant) -> bool {
        if !self.cards.contains(&card) {
            self.cards.push(card);
        }
        self.last.replace(now).is_none()
    }

    /// The timer is due: `Err(at)` waits on until `at` (a change came meanwhile), `Ok` hands over the cards to scan.
    fn due(&mut self, now: Instant) -> Result<Vec<libc::dev_t>, Instant> {
        match self.last.map(|t| t + SETTLE) {
            Some(at) if at > now => Err(at),
            _ => {
                self.last = None;
                Ok(std::mem::take(&mut self.cards))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    /// A frame timer is set (frame.rs decides when).
    Scheduled,
    /// A frame waits for its vblank.
    Flipping,
}

pub fn init(event_loop: &mut EventLoop<State>, state: &mut State) -> Result<(), Error> {
    let (session, notifier) = LibSeatSession::new()?;
    let seat = session.seat();
    let configured = &state.config.display.gpu;
    let configured = std::fs::canonicalize(configured).map_or_else(|_| configured.clone(), |p| p.to_string_lossy().into_owned());
    let all = all_gpus(&seat)?;
    let path = pick_gpu(&configured, primary_gpu(&seat)?, &all).ok_or("no GPU found")?;
    if !configured.is_empty() && path != Path::new(&configured) {
        tracing::warn!(gpu = configured, "display.gpu: no such GPU, using {}", path.display());
    }
    let card = DrmNode::from_path(&path)?;
    let primary = card.node_with_type(NodeType::Render).and_then(Result::ok).unwrap_or(card);
    tracing::info!("drawing on {} ({primary})", path.display());

    let mut libinput = Libinput::new_with_udev(LibinputSessionInterface::from(session.clone()));
    libinput.udev_assign_seat(&seat).map_err(|()| "libinput: cannot use seat")?;
    event_loop.handle().insert_source(LibinputInputBackend::new(libinput.clone()), |event, _, state| state.libinput_event(event))?;
    event_loop.handle().insert_source(notifier, |event, _, state| state.session_event(event))?;

    let udev = UdevBackend::new(&seat)?;
    let gpus = GpuManager::new(Api::default())?;
    state.device = Some(Device::Udev(Box::new(Udev {
        session,
        libinput,
        primary,
        gpus,
        cards: HashMap::new(),
        cursors: HashMap::new(),
        inputs: Vec::new(),
        resets: Vec::new(),
        settle: Settle::default(),
    })));
    for (dev, path) in udev.device_list() {
        if let Err(err) = state.card_added(dev, path) {
            tracing::warn!("{}: {err}", path.display());
        }
    }
    // ponytail: no pixman fallback; on a GPU without 3D (QEMU without virgl) Mesa's EGL picks its software driver
    // (kms_swrast/llvmpipe) by itself, which runs the shell's shaders. A pixman renderer would need every shader redone.
    let wallpaper = super::winit::wallpaper(event_loop, state)?;
    let renderer = state.device.as_mut().and_then(Device::renderer).ok_or("the primary GPU did not start (EGL)")?;
    let formats = renderer.dmabuf_formats();
    state.backend = Some(Views::new(renderer, wallpaper)?);
    super::dmabuf_global(state, formats, Some(primary));

    event_loop.handle().insert_source(udev, |event, _, state| {
        match event {
            UdevEvent::Added { device_id, path } => match state.card_added(device_id, &path) {
                Ok(node) => {
                    state.scan_connectors(node);
                    state.apply_displays();
                }
                Err(err) => tracing::warn!("{}: {err}", path.display()),
            },
            UdevEvent::Changed { device_id } => state.connectors_changed(device_id),
            UdevEvent::Removed { device_id } => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    state.card_removed(node);
                }
            }
        }
        crate::services::brightness::redetect();
    })?;
    // Applied by `main` right after.
    for node in state.cards() {
        state.scan_connectors(node);
    }
    Ok(())
}

impl Udev {
    /// The primary GPU's renderer (the one the shell's caches belong to).
    pub fn renderer(&mut self) -> Option<&mut GlesRenderer> {
        let primary = self.primary;
        self.gpus.devices_mut().ok()?.find(|d| *d.node() == primary).map(ApiDevice::renderer_mut)
    }

    /// The displays mirroring `source` draw a frame (each on its own frame clock).
    fn redraw_mirrors(&mut self, handle: &LoopHandle<'static, State>, source: &str) {
        for (node, card) in &mut self.cards {
            for (crtc, screen) in card.screens.iter_mut().filter(|(_, s)| s.mirror_of.as_deref() == Some(source)) {
                screen.request(handle, *node, *crtc);
            }
        }
    }

    /// Every display draws a frame (each when its own frame clock says).
    pub fn redraw_all(&mut self, handle: &LoopHandle<'static, State>) {
        for (node, card) in &mut self.cards {
            for (crtc, screen) in &mut card.screens {
                screen.request(handle, *node, *crtc);
            }
        }
    }
}

impl Screen {
    fn request(&mut self, handle: &LoopHandle<'static, State>, node: DrmNode, crtc: crtc::Handle) {
        match self.phase {
            Phase::Idle => {
                self.phase = Phase::Scheduled;
                let timer = Timer::from_duration(self.clock.delay(Instant::now()));
                if let Err(err) = handle.insert_source(timer, move |_, _, state| {
                    state.render_screen(node, crtc);
                    TimeoutAction::Drop
                }) {
                    tracing::warn!("frame timer: {err}");
                    self.phase = Phase::Idle;
                }
            }
            Phase::Scheduled => {}
            Phase::Flipping => self.again = true,
        }
    }
}

impl State {
    fn udev(&mut self) -> Option<&mut Udev> {
        match self.device.as_mut() {
            Some(Device::Udev(udev)) => Some(udev),
            _ => None,
        }
    }

    fn cards(&mut self) -> Vec<DrmNode> {
        self.udev().map(|u| u.cards.keys().copied().collect()).unwrap_or_default()
    }

    /// Opens a GPU through the session and adds it to the renderers; its displays are read in `scan_connectors`.
    fn card_added(&mut self, dev: libc::dev_t, path: &Path) -> Result<DrmNode, Error> {
        let node = DrmNode::from_dev_id(dev)?;
        let handle = self.loop_handle.clone();
        let udev = self.udev().ok_or("no session")?;
        let fd = udev.session.open(path, OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK)?;
        let fd = DrmDeviceFd::new(DeviceFd::from(fd));
        let (drm, notifier) = DrmDevice::new(fd.clone(), true)?;
        let gbm = GbmDevice::new(fd)?;
        let render = node.node_with_type(NodeType::Render).and_then(Result::ok).unwrap_or(node);
        udev.gpus.as_mut().add_node(render, gbm.clone())?;
        let token = handle
            .insert_source(notifier, move |event, meta, state| match event {
                DrmEvent::VBlank(crtc) => state.vblank(node, crtc, meta.as_ref()),
                DrmEvent::Error(err) => tracing::warn!("{node}: {err}"),
            })
            .map_err(|e| e.error)?;
        udev.cards.insert(node, Card { drm, gbm, render, token, screens: HashMap::new(), connected: Vec::new() });
        // The primary GPU went away and came back: the shell's textures belong to its old context.
        if render == udev.primary && self.backend.is_some() {
            let mut device = self.device.take();
            if let Some(Device::Udev(udev)) = &mut device
                && let Err(err) = self.rebuild_gpu(udev)
            {
                tracing::error!("the GPU did not come back: {err}");
                std::process::exit(1);
            }
            self.device = device;
        }
        Ok(node)
    }

    /// A GPU went away (an eGPU unplugged): its displays go dark; `apply_displays` brings their windows over.
    fn card_removed(&mut self, node: DrmNode) {
        let Some(udev) = self.udev() else { return };
        let Some(card) = udev.cards.remove(&node) else { return };
        udev.gpus.as_mut().remove_node(&card.render);
        self.loop_handle.remove(card.token);
        for screen in card.screens.into_values() {
            self.space.unmap_output(&screen.output);
            self.display_handle.remove_global::<State>(screen.global);
        }
        self.apply_displays();
    }

    /// A card's connectors changed (udev): scanned and applied once none has changed for `SETTLE`. Until then an
    /// unplugged display stays lit: its frames go nowhere, or fail as `Fail::Display` and are skipped.
    fn connectors_changed(&mut self, card: libc::dev_t) {
        let Some(udev) = self.udev() else { return };
        if !udev.settle.changed(card, Instant::now()) {
            return;
        }
        let timer = Timer::from_duration(SETTLE);
        if let Err(err) = self.loop_handle.insert_source(timer, |_, _, state| {
            let Some(udev) = state.udev() else { return TimeoutAction::Drop };
            match udev.settle.due(Instant::now()) {
                Err(at) => TimeoutAction::ToInstant(at),
                Ok(cards) => {
                    state.settled(cards);
                    TimeoutAction::Drop
                }
            }
        }) {
            tracing::warn!("display settle timer: {err}");
            let cards = self.udev().map(|u| std::mem::take(&mut u.settle).cards).unwrap_or_default();
            self.settled(cards);
        }
    }

    /// Hotplug settled: the changed cards are scanned and every display planned once.
    fn settled(&mut self, cards: Vec<libc::dev_t>) {
        for node in cards.into_iter().filter_map(|c| DrmNode::from_dev_id(c).ok()) {
            self.scan_connectors(node);
        }
        self.apply_displays();
    }

    /// Reads which displays are connected and drops unplugged ones (E6.2 hotplug); `apply_displays` lights the new.
    fn scan_connectors(&mut self, node: DrmNode) {
        let Some(Device::Udev(udev)) = self.device.as_mut() else { return };
        let Some(card) = udev.cards.get_mut(&node) else { return };
        let Ok(res) = card.drm.resource_handles() else { return };
        let connected: Vec<connector::Info> =
            res.connectors().iter().filter_map(|c| card.drm.get_connector(*c, true).ok()).filter(|info| info.state() == connector::State::Connected).collect();
        let gone: Vec<crtc::Handle> =
            card.screens.iter().filter(|(_, s)| !connected.iter().any(|i| i.handle() == s.connector)).map(|(crtc, _)| *crtc).collect();
        for crtc in gone {
            let Some(screen) = card.screens.remove(&crtc) else { continue };
            tracing::info!("display {} unplugged", screen.output.name());
            self.space.unmap_output(&screen.output);
            self.display_handle.remove_global::<State>(screen.global);
        }
        card.connected = connected.into_iter().map(|info| (info.clone(), describe(&card.drm, &info))).collect();
    }

    /// The real-screen half of `apply_displays`: plans every connected display, darkens the ones turned off, lights
    /// the ones turned on and modesets the ones whose mode changed; returns them all, with the lit ones' outputs.
    pub(crate) fn udev_displays(&mut self) -> Vec<(Display, Option<Output>)> {
        let Some(udev) = self.udev() else { return Vec::new() };
        let mut nodes: Vec<DrmNode> = udev.cards.keys().copied().collect();
        nodes.sort_by_key(|n| n.dev_id());
        let all: Vec<(DrmNode, usize)> = nodes.iter().flat_map(|n| (0..udev.cards[n].connected.len()).map(|i| (*n, i))).collect();
        let connected: Vec<Display> = all.iter().map(|(n, i)| udev.cards[n].connected[*i].1.clone()).collect();
        self.follow_layout(&connected);
        let planned = displays::plan(connected, &self.config.display, self.lid_closed);
        let Some(Device::Udev(udev)) = self.device.as_mut() else { return Vec::new() };
        let mut shown = Vec::new();
        for ((node, i), d) in all.into_iter().zip(planned) {
            let Some(card) = udev.cards.get_mut(&node) else { continue };
            let info = card.connected[i].0.clone();
            let lit = card.screens.iter().find(|(_, s)| s.connector == info.handle()).map(|(crtc, _)| *crtc);
            let drm_mode = d.modes.iter().position(|m| *m == d.mode).and_then(|i| info.modes().get(i).copied());
            let output = match (d.enabled, lit, drm_mode) {
                (false, Some(crtc), _) => {
                    if let Some(screen) = card.screens.remove(&crtc) {
                        tracing::info!("display {} turned off", d.name);
                        self.space.unmap_output(&screen.output);
                        self.display_handle.remove_global::<State>(screen.global);
                    }
                    None
                }
                (true, None, Some(drm_mode)) => match card
                    .drm
                    .resource_handles()
                    .map_err(Error::from)
                    .and_then(|res| light(card, &mut udev.gpus, &info, &res, drm_mode, &self.display_handle))
                {
                    Ok((crtc, screen)) => {
                        tracing::info!(mode = ?(d.mode.width, d.mode.height, d.mode.refresh), "display {} lit", d.name);
                        let output = screen.output.clone();
                        card.screens.insert(crtc, screen);
                        Some(output)
                    }
                    Err(err) => {
                        tracing::warn!("display {}: {err}", d.name);
                        None
                    }
                },
                (true, Some(crtc), Some(drm_mode)) => card.screens.get_mut(&crtc).map(|screen| {
                    let mode = Mode::from(drm_mode);
                    if screen.output.current_mode() != Some(mode) {
                        match screen.compositor.use_mode(drm_mode) {
                            Ok(()) => {
                                tracing::info!(mode = ?(d.mode.width, d.mode.height, d.mode.refresh), "display {} mode changed", d.name);
                                screen.output.change_current_state(Some(mode), None, None, None);
                                screen.clock = FrameClock::new(refresh(&screen.output));
                            }
                            Err(err) => tracing::warn!("display {}: mode {}x{}: {err}", d.name, d.mode.width, d.mode.height),
                        }
                    }
                    screen.output.clone()
                }),
                _ => None,
            };
            if let Some(screen) = card.screens.values_mut().find(|s| s.connector == info.handle()) {
                let of = Some(d.mirror.clone()).filter(|m| !m.is_empty());
                if screen.mirror_of != of {
                    (screen.mirror_of, screen.mirrored) = (of, None);
                }
                screen.underscan = d.underscan;
                // Takes effect with the next frame; a display that needs a modeset for it may blank briefly.
                if screen.compositor.vrr_enabled() != d.vrr
                    && let Err(err) = screen.compositor.use_vrr(d.vrr)
                {
                    tracing::warn!("display {}: variable refresh rate: {err}", d.name);
                }
            }
            shown.push((d, output));
        }
        shown
    }

    /// Draws one display's frame and queues it for its vblank.
    fn render_screen(&mut self, node: DrmNode, crtc: crtc::Handle) {
        let mut device = self.device.take();
        if let Some(Device::Udev(udev)) = &mut device {
            self.draw_screen(udev, node, crtc);
        }
        self.device = device;
    }

    fn draw_screen(&mut self, udev: &mut Udev, node: DrmNode, crtc: crtc::Handle) {
        let primary = udev.primary;
        // A mirror draws its source's picture: that display, on whichever GPU it is lit.
        let of = udev.cards.get(&node).and_then(|c| c.screens.get(&crtc)).and_then(|s| s.mirror_of.clone());
        let source = of.and_then(|name| udev.cards.values().flat_map(|c| c.screens.values()).map(|s| &s.output).find(|o| o.name() == name).cloned());
        let Some(card) = udev.cards.get_mut(&node) else { return };
        let Some(screen) = card.screens.get_mut(&crtc) else { return };
        screen.phase = Phase::Idle;
        if !udev.session.is_active() {
            return;
        }
        // The screen is "off": DPMS off and every plane cleared instead of a black frame; the next frame (input woke
        // us) lights it up again. Black is as good as the lock's frame, so sleep need not wait for one.
        if self.idle.stage >= crate::idle::Stage::Off {
            if let Err(err) = screen.compositor.clear() {
                tracing::warn!("{}: screen off: {err}", screen.output.name());
            }
            self.lock_frame_shown();
            self.frame_done();
            return;
        }
        let started = Instant::now();
        self.frame_time = screen.clock.target().unwrap_or(started);
        let renderer = match card.render == primary {
            true => udev.gpus.single_renderer(&primary),
            false => udev.gpus.renderer(&primary, &card.render, screen.compositor.format()),
        };
        let mut renderer = match renderer {
            Ok(renderer) => renderer,
            Err(err) => {
                if !std::mem::replace(&mut screen.failed, true) {
                    tracing::warn!("{}: no renderer: {err}", screen.output.name());
                }
                let fail = multi_fail(err);
                screen.failures += u32::from(fail == Fail::Render);
                return self.frame_failed(udev, fail);
            }
        };
        let drawn = match &source {
            Some(source) => self.frame_elements(renderer.as_mut(), source).and_then(|(elements, animating)| {
                let mut scene = self.cursor_elements(renderer.as_mut(), &mut udev.cursors, source);
                scene.extend(elements);
                Some((vec![mirror(renderer.as_mut(), &mut screen.mirrored, source, &screen.output, &scene)?], animating))
            }),
            None => self.frame_elements(renderer.as_mut(), &screen.output),
        };
        let Some((elements, animating)) = drawn else { return };
        let mut scene = self.cursor_elements(renderer.as_mut(), &mut udev.cursors, &screen.output);
        let cursors = scene.len();
        scene.extend(elements);
        let all: Vec<OnGpu> = scene.iter().map(OnGpu).collect();
        let frame = if screen.underscan == 0 {
            screen.compositor.render_frame(&mut renderer, &all, CLEAR, FrameFlags::DEFAULT).map(|f| f.is_empty)
        } else {
            // Underscan: the frame (drawn at the output's larger scale, see `displays::drawn_scale`) shrunk into a
            // centred rect, black around it. Input needs nothing: the output's logical size is the shrunk picture's.
            // ponytail: a touchscreen or tablet mapped to this display still spans the whole panel onto the picture,
            // so touches land up to 5 % off at the edges; offset absolute input too if a touch TV ever turns up.
            let size = screen.output.current_transform().transform_size(screen.output.current_mode().map_or_else(Size::default, |m| m.size));
            let at = displays::underscan_rect(size, screen.underscan).loc;
            let k = 1.0 - f64::from(screen.underscan) / 100.0;
            let shrunk: Vec<_> = all
                .into_iter()
                .map(|e| RelocateRenderElement::from_element(RescaleRenderElement::from_element(e, Point::default(), k), at, Relocate::Relative))
                .collect();
            screen.compositor.render_frame(&mut renderer, &shrunk, [0.0, 0.0, 0.0, 1.0], FrameFlags::DEFAULT).map(|f| f.is_empty)
        };
        // A screenshot or an allowed copy waits: the frame drawn once more offscreen on the render GPU, without the
        // cursor (the scanout buffer may be on another GPU, and planes hold the cursor and direct-scanout apps).
        let pixels = match frame {
            Ok(empty) if self.frame_wanted(&screen.output, !empty) => {
                let scene: Vec<&SceneElement> = match &self.zoom.plain {
                    Some(plain) => vec![plain],
                    None => scene[cursors..].iter().filter(|e| !matches!(e, SceneElement::Tint(_))).collect(),
                };
                crate::capture::render(renderer.as_mut(), &screen.output, &scene, CLEAR)
            }
            _ => None,
        };
        drop(renderer);
        let (output, damaged) = (screen.output.clone(), frame.as_ref().ok().map(|empty| !empty));
        let mut failed = None;
        if damaged.is_some() {
            (screen.failures, screen.failed) = (0, false);
        }
        screen.render_time = started.elapsed();
        match frame {
            // A mirror's apps are its source's: they get their frame callbacks from there.
            Ok(false) => {
                let feedback = match source {
                    Some(_) => smithay::desktop::utils::OutputPresentationFeedback::new(&screen.output),
                    None => self.frame_callbacks(&screen.output),
                };
                match screen.compositor.queue_frame(Some(feedback)) {
                    Ok(()) => {
                        screen.phase = Phase::Flipping;
                        // ponytail: queued, not yet on screen; release the sleep inhibitor on the page flip if a
                        // locked frame ever misses the screen before suspend.
                        self.lock_frame_shown();
                    }
                    Err(err) => tracing::warn!("{}: queue frame: {err}", screen.output.name()),
                }
            }
            // Nothing changed on screen: apps still get their frame callbacks, one refresh later, so one drawing in a
            // loop (hidden behind another window) runs at the refresh rate, not as fast as it can.
            Ok(true) => {
                if source.is_none() {
                    let _ = self.frame_callbacks(&screen.output);
                }
                screen.phase = Phase::Flipping;
                let timer = Timer::from_duration(refresh(&screen.output));
                if self
                    .loop_handle
                    .insert_source(timer, move |_, _, state| {
                        state.flip_done(node, crtc);
                        TimeoutAction::Drop
                    })
                    .is_err()
                {
                    screen.phase = Phase::Idle;
                }
            }
            Err(err) => {
                if !std::mem::replace(&mut screen.failed, true) {
                    tracing::warn!("{}: render: {err}", screen.output.name());
                }
                let fail = match err {
                    RenderFrameError::PrepareFrame(err) => match SwapBuffersError::from(err) {
                        SwapBuffersError::TemporaryFailure(_) => Fail::Temporary,
                        _ => Fail::Display,
                    },
                    RenderFrameError::RenderFrame(OutputDamageTrackerError::Rendering(err)) => multi_fail(err),
                    RenderFrameError::RenderFrame(_) => Fail::Display,
                };
                screen.failures += u32::from(fail == Fail::Render);
                failed = Some(fail);
            }
        }
        if animating || self.pointer_assist.animating(self.frame_time, self.dwell_wait()) {
            screen.again = true;
        }
        if damaged == Some(true) {
            udev.redraw_mirrors(&self.loop_handle, &output.name());
        }
        if let Some(damaged) = damaged {
            self.after_frame(&output, pixels, damaged);
        }
        self.frame_done();
        if let Some(fail) = failed {
            self.frame_failed(udev, fail);
        }
    }

    /// A frame failed: retried, or taken as a GPU reset (`gpu_verdict`).
    fn frame_failed(&mut self, udev: &mut Udev, fail: Fail) {
        let now = Instant::now();
        let failures = udev.cards.values().flat_map(|c| c.screens.values()).map(|s| s.failures).min().unwrap_or(0);
        match gpu_verdict(fail, failures, &udev.resets, now) {
            Gpu::Skip => {}
            Gpu::Retry => udev.redraw_all(&self.loop_handle),
            Gpu::Rebuild => {
                tracing::warn!("GPU reset (or {failures} failed frames on every display): starting the renderer again");
                udev.resets.retain(|t| now.duration_since(*t) < RESET_WINDOW);
                udev.resets.push(now);
                if let Err(err) = self.rebuild_gpu(udev) {
                    tracing::error!("the GPU did not come back: {err}");
                    std::process::exit(1);
                }
            }
            Gpu::GiveUp => {
                tracing::error!("the GPU keeps resetting ({MAX_RESETS} times in {RESET_WINDOW:?}): exiting");
                std::process::exit(1);
            }
        }
    }

    /// Remakes every EGL context on the same GPUs, the shell's textures and shaders (`Views`), the cursors, the
    /// mirrors' pictures and the scanout buffers, then redraws every display. Client buffers import again on their
    /// next frame: Smithay keeps their textures per renderer context, so the new context finds none.
    // ponytail: the DRM surfaces (mode, planes) are kept; only their buffers are remade. The shell's open popovers
    // and animations restart from their defaults.
    fn rebuild_gpu(&mut self, udev: &mut Udev) -> Result<(), Error> {
        let wallpaper = self.backend.take().ok_or("no views")?.wallpaper.reupload();
        udev.gpus = GpuManager::new(Api::default())?;
        for card in udev.cards.values_mut() {
            udev.gpus.as_mut().add_node(card.render, card.gbm.clone())?;
            for screen in card.screens.values_mut() {
                screen.compositor.reset_buffers();
                screen.mirrored = None;
                screen.failures = 0;
            }
        }
        udev.cursors.clear();
        let renderer = udev.renderer().ok_or("the primary GPU did not start (EGL)")?;
        self.backend = Some(Views::new(renderer, wallpaper)?);
        udev.redraw_all(&self.loop_handle);
        Ok(())
    }

    /// The pointer, if it is on `output`: the app's cursor surface or the theme's picture (hardware cursor plane).
    fn cursor_elements(&self, renderer: &mut GlesRenderer, cache: &mut Cursors, output: &Output) -> Vec<SceneElement> {
        let (Some(pointer), Some(geo)) = (self.seat.get_pointer(), self.space.output_geometry(output)) else { return Vec::new() };
        let location = pointer.current_location();
        if !geo.to_f64().contains(location) {
            return Vec::new();
        }
        let scale = output.current_scale().fractional_scale();
        let at = (location - geo.loc.to_f64()).to_physical(scale);
        // The dwell click's countdown: an accent ring shrinking around the pointer.
        let ring = self.dwell_ring(geo.loc).and_then(|rect| {
            let color = render::accent(self.config.appearance.accent, render::is_dark(&self.config.appearance)).map(|c| f32::from(c) / 255.0);
            let shaders = &self.backend.as_ref()?.shaders;
            Some(SceneElement::Shader(shaders.ring(rect, f64::from(rect.size.w) / 2.0, 2, [color[0], color[1], color[2], 1.0], scale)))
        });
        // Shaken (shake to find), the theme's arrow grows in place of an app's own cursor too.
        let grow = self.pointer_assist.shake.scale(self.frame_time);
        let icon = match &self.protocols.cursor {
            CursorImageStatus::Hidden => return ring.into_iter().collect(),
            CursorImageStatus::Surface(surface) if grow <= 1.0 => {
                let hotspot = with_states(surface, |s| s.data_map.get::<CursorImageSurfaceData>().and_then(|d| d.lock().ok().map(|d| d.hotspot)));
                let at = (at - hotspot.unwrap_or_default().to_f64().to_physical(scale)).to_i32_round();
                let surface = render_elements_from_surface_tree(renderer, surface, at, scale, 1.0, Kind::Cursor).into_iter().map(SceneElement::Surface);
                return surface.chain(ring).collect();
            }
            CursorImageStatus::Surface(_) => CursorIcon::Default,
            CursorImageStatus::Named(icon) => *icon,
        };
        let base = f64::from(cursor::size(self.config.appearance.pointer_size)) * scale;
        // Growing: the theme's biggest fitting picture, resampled down to the size of the moment.
        let px = (base * if grow > 1.0 { crate::pointer_assist::SHAKE_GROW } else { 1.0 }).round() as u32;
        let entry = cache.entry((icon, px)).or_insert_with(|| {
            let names: Vec<&str> = [icon.name()].into_iter().chain(icon.alt_names().iter().copied()).chain(["default", "left_ptr"]).collect();
            let image = cursor::load(&names, px)?;
            let (w, h) = (image.size.0 as i32, image.size.1 as i32);
            let buffer = MemoryRenderBuffer::from_slice(&image.pixels, Fourcc::Argb8888, (w, h), 1, Transform::Normal, None);
            Some((buffer, Point::from((image.hotspot.0 as i32, image.hotspot.1 as i32)), Size::from((w, h))))
        });
        let Some((buffer, hotspot, size)) = entry else { return Vec::new() };
        // Drawn 1:1 in physical px: the theme's picture nearest the scaled size, never resampled (unless growing).
        let factor = if grow > 1.0 { grow * base / f64::from(size.w) } else { 1.0 };
        let logical = size.to_f64().upscale(factor).to_logical(scale).to_i32_round();
        let src = Rectangle::from_size(size.to_f64().to_logical(1.0));
        MemoryRenderBufferRenderElement::from_buffer(renderer, at - hotspot.to_f64().upscale(factor), buffer, None, Some(src), Some(logical), Kind::Cursor)
            .ok()
            .map(SceneElement::Memory)
            .into_iter()
            .chain(ring)
            .collect()
    }

    fn vblank(&mut self, node: DrmNode, crtc: crtc::Handle, meta: Option<&DrmEventMetadata>) {
        let now = self.protocols.clock.now();
        let Some(Device::Udev(udev)) = self.device.as_mut() else { return };
        let Some(screen) = udev.cards.get_mut(&node).and_then(|c| c.screens.get_mut(&crtc)) else { return };
        match screen.compositor.frame_submitted() {
            Ok(Some(Some(mut feedback))) => {
                let (time, seq) = match meta {
                    Some(DrmEventMetadata { time: DrmEventTime::Monotonic(time), sequence }) => (Time::<Monotonic>::from(*time), u64::from(*sequence)),
                    _ => (now, 0),
                };
                use wp_presentation_feedback::Kind as K;
                let period = refresh(&screen.output);
                let rate = if screen.compositor.vrr_enabled() { Refresh::variable(period) } else { Refresh::fixed(period) };
                feedback.presented(time, rate, seq, K::Vsync | K::HwClock | K::HwCompletion);
            }
            Ok(_) => {}
            Err(err) => tracing::warn!("{}: frame: {err}", screen.output.name()),
        }
        screen.clock.presented(Instant::now(), screen.render_time);
        self.flip_done(node, crtc);
    }

    /// The display's frame is up (or its no-damage wait is over): draw the next one if anything asked.
    fn flip_done(&mut self, node: DrmNode, crtc: crtc::Handle) {
        let Some(Device::Udev(udev)) = self.device.as_mut() else { return };
        let Some(screen) = udev.cards.get_mut(&node).and_then(|c| c.screens.get_mut(&crtc)) else { return };
        screen.phase = Phase::Idle;
        if std::mem::take(&mut screen.again) {
            screen.request(&self.loop_handle, node, crtc);
        }
    }

    /// VT switched away (Ctrl+Alt+F<n>) or back: devices pause and resume with the session.
    fn session_event(&mut self, event: SessionEvent) {
        let Some(udev) = self.udev() else { return };
        match event {
            SessionEvent::PauseSession => {
                tracing::info!("session paused");
                udev.libinput.suspend();
                for card in udev.cards.values_mut() {
                    card.drm.pause();
                }
            }
            SessionEvent::ActivateSession => {
                tracing::info!("session active");
                if udev.libinput.resume().is_err() {
                    tracing::warn!("libinput did not resume");
                }
                for card in udev.cards.values_mut() {
                    if let Err(err) = card.drm.activate(false) {
                        tracing::warn!("drm activate: {err}");
                    }
                    // A frame queued before the pause never got its vblank.
                    for screen in card.screens.values_mut() {
                        if let Err(err) = screen.compositor.reset_state() {
                            tracing::warn!("{}: reset: {err}", screen.output.name());
                        }
                        screen.phase = Phase::Idle;
                        screen.again = false;
                    }
                }
                // Monitors may have changed while away.
                for node in self.cards() {
                    self.scan_connectors(node);
                }
                self.apply_displays();
            }
        }
    }

    fn libinput_event(&mut self, event: InputEvent<LibinputInputBackend>) {
        match event {
            InputEvent::DeviceAdded { mut device } => {
                configure(&mut device, &self.config.touchpad, &self.config.mouse);
                if let Some(Device::Udev(udev)) = self.device.as_mut() {
                    udev.inputs.push(device);
                }
            }
            InputEvent::DeviceRemoved { device } => {
                if let Some(Device::Udev(udev)) = self.device.as_mut() {
                    udev.inputs.retain(|d| *d != device);
                }
            }
            InputEvent::PointerMotion { event } => self.relative_motion(event.delta(), event.time()),
            // Clamshell: the panel goes off with the lid closed while another display is on (`displays::plan`).
            // libinput sends the lid's state as its switch is added, so a lid already closed at start counts too.
            InputEvent::SwitchToggle { event } if SwitchToggleEvent::switch(&event) == Some(Switch::Lid) => {
                let closed = SwitchToggleEvent::state(&event) == SwitchState::On;
                if closed != self.lid_closed {
                    tracing::info!(closed, "lid");
                    self.lid_closed = closed;
                    self.apply_displays();
                }
            }
            // A touchscreen or tablet maps onto its own display, not the first.
            InputEvent::PointerMotionAbsolute { event } => {
                let outputs: Vec<(String, Rectangle<i32, Logical>)> =
                    self.space.outputs().filter_map(|o| Some((o.name(), self.space.output_geometry(o)?))).collect();
                let Some(geo) = absolute_output(event.device().output_name(), &outputs) else { return };
                self.pointer_motion(event.position_transformed(geo.size) + geo.loc.to_f64(), event.time());
                // The pointer is ours to draw.
                self.schedule_redraw();
            }
            event => self.process_input_event(event),
        }
    }

    /// A mouse or touchpad moved: the pointer moves by `delta`, kept on the displays.
    pub fn relative_motion(&mut self, delta: Point<f64, Logical>, utime: u64) {
        let Some(pointer) = self.seat.get_pointer() else { return };
        let current = pointer.current_location();
        let screens: Vec<_> = self.space.outputs().filter_map(|o| self.space.output_geometry(o)).collect();
        let target = clamp_pointer(&screens, current, current + delta);
        // pointer_motion takes the step from here, so apps reading relative motion get the whole delta.
        self.protocols.last_host_pointer = Some(target - delta);
        self.pointer_motion(target, utime);
        self.schedule_redraw();
    }

    /// Ctrl+Alt+F<n> (xkb gives XF86Switch_VT_<n>) switches to virtual terminal n; only on the real screen.
    pub fn switch_vt(&mut self, sym: Keysym) -> bool {
        let Some(vt) = vt_of(sym) else { return false };
        let Some(udev) = self.udev() else { return false };
        if let Err(err) = udev.session.change_vt(vt) {
            tracing::warn!("switch to VT {vt}: {err}");
        }
        true
    }
}

/// The connector's name (`eDP-1`), make, model, size and modes, as `displays::plan` and `displays.json` want them.
fn describe(drm: &DrmDevice, info: &connector::Info) -> Display {
    let name = format!("{}-{}", info.interface().as_str(), info.interface_id());
    let (width_mm, height_mm) = info.size().unwrap_or((0, 0));
    let edid = edid(drm, info.handle()).unwrap_or_default();
    let ((make, model), serial) = (displays::edid_names(&edid), displays::edid_serial(&edid));
    let modes = info
        .modes()
        .iter()
        .map(|m| {
            let mode = Mode::from(*m);
            display_info::Mode { width: mode.size.w, height: mode.size.h, refresh: mode.refresh, preferred: m.mode_type().contains(ModeTypeFlags::PREFERRED) }
        })
        .collect();
    let vrr_capable = property(drm, info.handle(), b"vrr_capable") == Some(1);
    Display { built_in: display_info::built_in(&name), name, make, model, serial, width_mm, height_mm, modes, vrr_capable, ..Default::default() }
}

/// The connector's EDID blob.
fn edid(drm: &DrmDevice, connector: connector::Handle) -> Option<Vec<u8>> {
    drm.get_property_blob(property(drm, connector, b"EDID")?).ok().filter(|b| !b.is_empty())
}

/// A connector property's raw value, by name.
fn property(drm: &DrmDevice, connector: connector::Handle, name: &[u8]) -> Option<u64> {
    let props = drm.get_properties(connector).ok()?;
    props.iter().find_map(|(id, value)| (drm.get_property(*id).ok()?.name().to_bytes() == name).then_some(*value))
}

/// Picks a CRTC, and makes the output (in `drm_mode`), its global and its DRM compositor; `apply_displays` sets its
/// scale, rotation and place.
fn light(
    card: &mut Card,
    gpus: &mut GpuManager<Api>,
    info: &connector::Info,
    res: &ResourceHandles,
    drm_mode: drm_control::Mode,
    dh: &DisplayHandle,
) -> Result<(crtc::Handle, Screen), Error> {
    let crtc = info
        .encoders()
        .iter()
        .filter_map(|e| card.drm.get_encoder(*e).ok())
        .flat_map(|e| res.filter_crtcs(e.possible_crtcs()))
        .find(|c| !card.screens.contains_key(c))
        .ok_or("no free CRTC")?;
    let surface = card.drm.create_surface(crtc, drm_mode, &[info.handle()])?;

    let (mm_w, mm_h) = info.size().unwrap_or((0, 0));
    let (make, model) = edid(&card.drm, info.handle()).map(|e| displays::edid_names(&e)).unwrap_or_default();
    let unknown = |s: String| if s.is_empty() { "Unknown".into() } else { s };
    let properties =
        PhysicalProperties { size: (mm_w as i32, mm_h as i32).into(), subpixel: subpixel(info.subpixel()), make: unknown(make), model: unknown(model) };
    let output = Output::new(format!("{}-{}", info.interface().as_str(), info.interface_id()), properties);
    let mode = Mode::from(drm_mode);
    for m in info.modes() {
        output.add_mode(Mode::from(*m));
    }
    if let Some(preferred) = info.modes().iter().find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED)) {
        output.set_preferred(Mode::from(*preferred));
    }
    output.change_current_state(Some(mode), Some(Transform::Normal), None, None);
    let global = output.create_global::<State>(dh);

    let formats = gpus.single_renderer(&card.render)?.as_mut().egl_context().dmabuf_render_formats().clone();
    let allocator = GbmAllocator::new(card.gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
    let exporter = GbmFramebufferExporter::new(card.gbm.clone(), Some(card.render));
    let compositor = DrmCompositor::new(&output, surface, None, allocator, exporter, FORMATS, formats, card.drm.cursor_size(), Some(card.gbm.clone()))
        .map_err(|e| format!("{e}"))?;
    let clock = FrameClock::new(refresh(&output));
    Ok((
        crtc,
        Screen {
            output,
            global,
            connector: info.handle(),
            compositor,
            clock,
            phase: Phase::Idle,
            again: false,
            render_time: Duration::ZERO,
            mirror_of: None,
            mirrored: None,
            underscan: 0,
            failures: 0,
            failed: false,
        },
    ))
}

/// `scene` (the `source` display's frame, front to back) drawn offscreen at the source's size into `cache`, then
/// shown letterboxed in the `mirror` display's own pixels (its scale is 1). Only the source's changes are redrawn.
// ponytail: the whole mirror is redrawn whenever the source changed; pass the offscreen damage on if a mirror ever
// shows up in the frame budget.
fn mirror(renderer: &mut GlesRenderer, cache: &mut Option<Mirrored>, source: &Output, mirror: &Output, scene: &[SceneElement]) -> Option<SceneElement> {
    let size = source.current_transform().transform_size(source.current_mode()?.size);
    let buffer = Size::from((size.w, size.h));
    let fresh = cache.as_ref().is_none_or(|m| m.texture.size() != buffer);
    if fresh {
        let texture = renderer.create_buffer(Fourcc::Abgr8888, buffer).inspect_err(|err| tracing::warn!("mirror: {err}")).ok()?;
        let damage = OutputDamageTracker::new(size, source.current_scale().fractional_scale(), Transform::Normal);
        *cache = Some(Mirrored { texture, damage, id: Id::new() });
    }
    let m = cache.as_mut()?;
    let mut target = renderer.bind(&mut m.texture).ok()?;
    let drawn = m.damage.render_output(renderer, &mut target, if fresh { 0 } else { 1 }, scene, CLEAR);
    let changed = drawn.inspect_err(|err| tracing::warn!("mirror: {err}")).ok()?.damage.is_some();
    drop(target);
    if changed {
        m.id = Id::new();
    }
    let rect = displays::letterbox(size, mirror.current_transform().transform_size(mirror.current_mode()?.size));
    Some(SceneElement::Texture(TextureRenderElement::from_static_texture(
        m.id.clone(),
        renderer.context_id(),
        rect.loc.to_f64(),
        m.texture.clone(),
        1,
        Transform::Normal,
        None,
        None,
        Some((rect.size.w, rect.size.h).into()),
        None,
        Kind::Unspecified,
    )))
}

/// The output's refresh period.
fn refresh(output: &Output) -> Duration {
    // A mode that reports no refresh (0 Hz on some connectors) counts as 60 Hz, not 1.
    let mhz = output.current_mode().map(|m| m.refresh).filter(|r| *r >= 1_000).unwrap_or(60_000);
    Duration::from_nanos(1_000_000_000_000 / mhz as u64)
}

impl Udev {
    /// The trackpad or mouse settings changed: apply them to every plugged-in device.
    pub fn configure_inputs(&mut self, touchpad: &Touchpad, mouse: &Mouse) {
        for device in &mut self.inputs {
            configure(device, touchpad, mouse);
        }
    }
}

/// The trackpad (E1.7) and mouse settings, applied as a device appears and on reload. A setting a device lacks is
/// skipped by libinput.
fn configure(device: &mut InputDevice, touchpad: &Touchpad, mouse: &Mouse) {
    if device.config_tap_finger_count() > 0 {
        let _ = device.config_tap_set_enabled(touchpad.tap_to_click);
        let _ = device.config_tap_set_drag_enabled(touchpad.tap_to_drag);
        let _ = device.config_tap_set_drag_lock_enabled(touchpad.drag_lock);
        let _ = device.config_scroll_set_natural_scroll_enabled(touchpad.natural_scroll);
        let _ = device.config_accel_set_speed(touchpad.speed.clamp(-1.0, 1.0));
        let _ = device.config_dwt_set_enabled(touchpad.disable_while_typing);
        let _ = device.config_left_handed_set(mouse.left_handed);
        let _ = device.config_scroll_set_method(scroll_method(touchpad.scroll_method));
        match click_method(touchpad.secondary_click) {
            Some(method) => {
                let _ = device.config_click_set_method(method);
            }
            // SAFETY: a live device; the `input` crate has no "none" variant, libinput takes 0.
            None => unsafe {
                use smithay::reexports::input::{AsRaw, ffi};
                ffi::libinput_device_config_click_set_method(device.as_raw_mut(), ffi::libinput_config_click_method_LIBINPUT_CONFIG_CLICK_METHOD_NONE);
            },
        }
        // SAFETY: a live device. Fewer than 3 fingers: no 3-finger drag on this pad.
        unsafe {
            use smithay::reexports::input::AsRaw;
            if libinput_device_config_3fg_drag_get_finger_count(device.as_raw_mut()) >= 3 {
                // 1 = LIBINPUT_CONFIG_3FG_DRAG_ENABLED_3FG.
                libinput_device_config_3fg_drag_set_enabled(device.as_raw_mut(), touchpad.three_finger_drag.into());
            }
        }
        let mode = match (touchpad.enabled, touchpad.ignore_with_mouse) {
            (false, _) => SendEventsMode::DISABLED,
            (true, true) => SendEventsMode::DISABLED_ON_EXTERNAL_MOUSE,
            (true, false) => SendEventsMode::ENABLED,
        };
        let _ = device.config_send_events_set_mode(mode);
    } else if device.has_capability(DeviceCapability::Pointer) {
        let _ = device.config_accel_set_speed(mouse.speed.clamp(-1.0, 1.0));
        let _ = device.config_accel_set_profile(if mouse.acceleration { AccelProfile::Adaptive } else { AccelProfile::Flat });
        let _ = device.config_left_handed_set(mouse.left_handed);
        let _ = device.config_scroll_set_natural_scroll_enabled(mouse.natural_scroll);
        let _ = device.config_middle_emulation_set_enabled(mouse.middle_click_emulation);
    }
}

// libinput's 3-finger drag (since 1.27), which input-sys (bound to libinput 1.21) lacks.
// ponytail: linked directly, so a libinput older than 1.27 can't start the desktop; Arch ships newer. Look the symbols
// up with dlsym if older systems matter.
unsafe extern "C" {
    fn libinput_device_config_3fg_drag_get_finger_count(device: *mut smithay::reexports::input::ffi::libinput_device) -> std::ffi::c_int;
    fn libinput_device_config_3fg_drag_set_enabled(device: *mut smithay::reexports::input::ffi::libinput_device, state: std::ffi::c_uint) -> std::ffi::c_uint;
}

/// libinput's click method for a secondary-click choice; `None`: no method (every click is a left click).
fn click_method(secondary: SecondaryClick) -> Option<ClickMethod> {
    match secondary {
        SecondaryClick::TwoFingers => Some(ClickMethod::Clickfinger),
        SecondaryClick::Corner => Some(ClickMethod::ButtonAreas),
        SecondaryClick::Off => None,
    }
}

fn scroll_method(method: ScrollMethod) -> smithay::reexports::input::ScrollMethod {
    use smithay::reexports::input::ScrollMethod as Libinput;
    match method {
        ScrollMethod::TwoFingers => Libinput::TwoFinger,
        ScrollMethod::Edge => Libinput::Edge,
        ScrollMethod::None => Libinput::NoScroll,
    }
}

fn subpixel(subpixel: connector::SubPixel) -> Subpixel {
    match subpixel {
        connector::SubPixel::HorizontalRgb => Subpixel::HorizontalRgb,
        connector::SubPixel::HorizontalBgr => Subpixel::HorizontalBgr,
        connector::SubPixel::VerticalRgb => Subpixel::VerticalRgb,
        connector::SubPixel::VerticalBgr => Subpixel::VerticalBgr,
        connector::SubPixel::None => Subpixel::None,
        _ => Subpixel::Unknown,
    }
}

/// The GPU that draws (E6.1): the configured one if it exists, else the boot VGA device, else the first.
fn pick_gpu(configured: &str, boot_vga: Option<PathBuf>, all: &[PathBuf]) -> Option<PathBuf> {
    all.iter().find(|p| !configured.is_empty() && p.as_path() == Path::new(configured)).cloned().or(boot_vga).or_else(|| all.first().cloned())
}

/// `to`, if it is on a display; else `to` held inside the display the pointer is on (or, off every display after an
/// unplug, the nearest one).
pub(crate) fn clamp_pointer(screens: &[Rectangle<i32, Logical>], from: Point<f64, Logical>, to: Point<f64, Logical>) -> Point<f64, Logical> {
    if screens.iter().any(|g| g.to_f64().contains(to)) {
        return to;
    }
    let held = |g: &Rectangle<f64, Logical>| {
        Point::<f64, Logical>::from((to.x.clamp(g.loc.x, g.loc.x + g.size.w - 1.0), to.y.clamp(g.loc.y, g.loc.y + g.size.h - 1.0)))
    };
    let distance = |g: &Rectangle<f64, Logical>| {
        let p = held(g) - to;
        (p.x * p.x + p.y * p.y) as i64
    };
    let on = screens.iter().map(|g| g.to_f64()).find(|g| g.contains(from));
    on.or_else(|| screens.iter().map(|g| g.to_f64()).min_by_key(distance)).map_or(to, |g| held(&g))
}

/// The display an absolute device (touchscreen, tablet) covers: the one libinput names for it (udev's `WL_OUTPUT`),
/// else a laptop's own panel (where its touchscreen is), else the first.
fn absolute_output(name: Option<&str>, outputs: &[(String, Rectangle<i32, Logical>)]) -> Option<Rectangle<i32, Logical>> {
    let named = outputs.iter().find(|(n, _)| Some(n.as_str()) == name);
    named.or_else(|| outputs.iter().find(|(n, _)| display_info::built_in(n))).or(outputs.first()).map(|(_, geo)| *geo)
}

/// The VT of an XF86Switch_VT_<n> keysym.
fn vt_of(sym: Keysym) -> Option<i32> {
    let first = Keysym::XF86_Switch_VT_1.raw();
    (first..=Keysym::XF86_Switch_VT_12.raw()).contains(&sym.raw()).then(|| (sym.raw() - first + 1) as i32)
}

/// A scene element drawn by the render GPU's GLES frame inside a multi-GPU frame.
struct OnGpu<'e>(&'e SceneElement);

impl Element for OnGpu<'_> {
    fn id(&self) -> &Id {
        self.0.id()
    }
    fn current_commit(&self) -> CommitCounter {
        self.0.current_commit()
    }
    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.0.location(scale)
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        self.0.src()
    }
    fn transform(&self) -> Transform {
        self.0.transform()
    }
    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.0.geometry(scale)
    }
    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        self.0.damage_since(scale, commit)
    }
    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        self.0.opaque_regions(scale)
    }
    fn alpha(&self) -> f32 {
        self.0.alpha()
    }
    fn kind(&self) -> Kind {
        self.0.kind()
    }
}

impl<'a> RenderElement<Multi<'a>> for OnGpu<'_> {
    fn draw(
        &self,
        frame: &mut <Multi<'a> as RendererSuper>::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), <Multi<'a> as RendererSuper>::Error> {
        RenderElement::<GlesRenderer>::draw(&self.0, frame.as_mut(), src, dst, damage, opaque_regions).map_err(Into::into)
    }

    /// A client buffer can go straight to a display plane (direct scanout, E6.3).
    fn underlying_storage(&self, renderer: &mut Multi<'a>) -> Option<UnderlyingStorage<'_>> {
        self.0.underlying_storage(renderer.as_mut())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::backend::{
        egl,
        renderer::{gles::GlesError, multigpu::gbm},
    };

    #[test]
    fn secondary_click_maps_to_libinput() {
        assert_eq!(click_method(SecondaryClick::TwoFingers), Some(ClickMethod::Clickfinger));
        assert_eq!(click_method(SecondaryClick::Corner), Some(ClickMethod::ButtonAreas));
        assert_eq!(click_method(SecondaryClick::Off), None);
    }

    #[test]
    fn scroll_method_maps_to_libinput() {
        use smithay::reexports::input::ScrollMethod as Libinput;
        assert_eq!(scroll_method(ScrollMethod::TwoFingers), Libinput::TwoFinger);
        assert_eq!(scroll_method(ScrollMethod::Edge), Libinput::Edge);
        assert_eq!(scroll_method(ScrollMethod::None), Libinput::NoScroll);
    }

    #[test]
    fn primary_gpu_choice() {
        let all = [PathBuf::from("/dev/dri/card0"), PathBuf::from("/dev/dri/card1")];
        let boot = Some(PathBuf::from("/dev/dri/card1"));
        assert_eq!(pick_gpu("", boot.clone(), &all), boot, "boot VGA by default");
        assert_eq!(pick_gpu("/dev/dri/card0", boot.clone(), &all), Some(all[0].clone()), "config wins");
        assert_eq!(pick_gpu("/dev/dri/card7", boot.clone(), &all), boot, "a missing configured GPU is ignored");
        assert_eq!(pick_gpu("", None, &all), Some(all[0].clone()));
        assert_eq!(pick_gpu("", None, &[]), None);
    }

    #[test]
    fn pointer_stays_on_displays() {
        let screens = [Rectangle::new((0, 0).into(), (1280, 800).into()), Rectangle::new((1280, 0).into(), (1920, 1080).into())];
        let p = |x, y| Point::<f64, Logical>::from((x, y));
        assert_eq!(clamp_pointer(&screens, p(1270.0, 10.0), p(1290.0, 10.0)), p(1290.0, 10.0), "across to the next display");
        assert_eq!(clamp_pointer(&screens, p(100.0, 790.0), p(100.0, 900.0)), p(100.0, 799.0), "the first is shorter");
        assert_eq!(clamp_pointer(&screens, p(5.0, 5.0), p(-20.0, -3.0)), p(0.0, 0.0));
        assert_eq!(clamp_pointer(&[], p(5.0, 5.0), p(-20.0, -3.0)), p(-20.0, -3.0));
        // Off every display (the one it was on got unplugged): onto the nearest one.
        assert_eq!(clamp_pointer(&screens, p(3300.0, 900.0), p(3300.0, 900.0)), p(3199.0, 900.0));
        assert_eq!(clamp_pointer(&screens[1..], p(100.0, 100.0), p(100.0, 100.0)), p(1280.0, 100.0));
    }

    #[test]
    fn absolute_devices_map_onto_their_display() {
        let geo = |x| Rectangle::<i32, Logical>::new((x, 0).into(), (1000, 1000).into());
        let outputs = [("HDMI-A-1".to_string(), geo(0)), ("eDP-1".to_string(), geo(1000)), ("DP-2".to_string(), geo(2000))];
        assert_eq!(absolute_output(Some("DP-2"), &outputs), Some(geo(2000)), "named by udev");
        assert_eq!(absolute_output(None, &outputs), Some(geo(1000)), "the laptop's own panel");
        assert_eq!(absolute_output(Some("DP-9"), &outputs[..1]), Some(geo(0)), "else the first");
        assert_eq!(absolute_output(None, &[]), None);
    }

    #[test]
    fn gpu_resets_rebuild_then_give_up() {
        let now = Instant::now() + Duration::from_secs(600);
        assert_eq!(gpu_verdict(Fail::Render, 1, &[], now), Gpu::Retry);
        assert_eq!(gpu_verdict(Fail::Render, FAILURES, &[], now), Gpu::Rebuild);
        assert_eq!(gpu_verdict(Fail::Lost, 0, &[], now), Gpu::Rebuild);
        let recent = [now - Duration::from_secs(50), now - Duration::from_secs(20), now - Duration::from_secs(1)];
        assert_eq!(gpu_verdict(Fail::Lost, 0, &recent, now), Gpu::GiveUp);
        assert_eq!(gpu_verdict(Fail::Render, 1, &recent, now), Gpu::Retry);
        // Resets more than a minute ago no longer count.
        let old = [now - Duration::from_secs(120), now - Duration::from_secs(20), now - Duration::from_secs(1)];
        assert_eq!(gpu_verdict(Fail::Lost, 0, &old, now), Gpu::Rebuild);
    }

    #[test]
    fn display_and_temporary_failures_never_rebuild() {
        let now = Instant::now();
        // A DRM error on one display (addfb2 EINVAL every frame) leaves it dark, however often.
        assert_eq!(gpu_verdict(Fail::Display, 1000, &[], now), Gpu::Skip);
        assert_eq!(gpu_verdict(Fail::Temporary, 1000, &[], now), Gpu::Retry);
        assert_eq!(multi_fail(MultiError::DeviceMissing), Fail::Display);
        assert_eq!(multi_fail(MultiError::ImportFailed), Fail::Temporary);
        assert_eq!(multi_fail(MultiError::Render(GlesError::ShaderCompileError)), Fail::Render);
        let lost = GlesError::ContextActivationError(EGLError::ContextLost.into());
        assert_eq!(multi_fail(MultiError::Render(lost)), Fail::Lost);
        assert_eq!(multi_fail(MultiError::RenderApiError(gbm::Error::Egl(egl::Error::CreationFailed(EGLError::ContextLost)))), Fail::Lost);
        assert_eq!(multi_fail(MultiError::RenderApiError(gbm::Error::Egl(egl::Error::CreationFailed(EGLError::BadAlloc)))), Fail::Render);
    }

    #[test]
    fn hotplug_settles_before_the_displays_are_planned() {
        let (mut settle, t0) = (Settle::default(), Instant::now());
        // The dock's first monitor starts the timer; its second, 300 ms later, doesn't start another.
        assert!(settle.changed(1, t0));
        assert!(!settle.changed(1, t0 + Duration::from_millis(300)));
        assert!(!settle.changed(2, t0 + Duration::from_millis(400)));
        // Due at 500 ms, but the last change was at 400 ms: wait on until 900 ms.
        assert_eq!(settle.due(t0 + SETTLE), Err(t0 + Duration::from_millis(900)));
        assert_eq!(settle.due(t0 + Duration::from_millis(900)), Ok(vec![1, 2]));
        // Settled: the next change starts a timer again, with only its own card.
        assert!(settle.changed(2, t0 + Duration::from_secs(5)));
        assert_eq!(settle.due(t0 + Duration::from_secs(6)), Ok(vec![2]));
    }

    #[test]
    fn vt_keys() {
        assert_eq!(vt_of(Keysym::XF86_Switch_VT_1), Some(1));
        assert_eq!(vt_of(Keysym::XF86_Switch_VT_12), Some(12));
        assert_eq!(vt_of(Keysym::F1), None);
    }
}
