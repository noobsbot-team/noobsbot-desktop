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
        allocator::{
            Fourcc,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmEventMetadata, DrmEventTime, DrmNode, NodeType,
            compositor::{DrmCompositor, FrameFlags},
            exporter::gbm::GbmFramebufferExporter,
        },
        input::{Event, InputEvent, PointerMotionEvent},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            ImportDma, RendererSuper,
            element::{
                Element, Id, Kind, RenderElement, UnderlyingStorage,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                surface::render_elements_from_surface_tree,
            },
            gles::GlesRenderer,
            multigpu::{ApiDevice, GpuManager, MultiRenderer, gbm::GbmGlesBackend},
            utils::{CommitCounter, DamageSet, OpaqueRegions},
        },
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::{UdevBackend, UdevEvent, all_gpus, primary_gpu},
    },
    desktop::layer_map_for_output,
    input::{
        keyboard::Keysym,
        pointer::{CursorIcon, CursorImageStatus, CursorImageSurfaceData},
    },
    output::{Mode, Output, PhysicalProperties, Scale as OutputScale, Subpixel},
    reexports::{
        calloop::{
            EventLoop, LoopHandle, RegistrationToken,
            timer::{TimeoutAction, Timer},
        },
        drm::control::{Device as _, ModeTypeFlags, ResourceHandles, connector, crtc},
        input::{Device as InputDevice, Libinput},
        rustix::fs::OFlags,
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::{DisplayHandle, backend::GlobalId},
    },
    utils::{Buffer, DeviceFd, Logical, Monotonic, Physical, Point, Rectangle, Scale, Size, Time, Transform},
    wayland::{compositor::with_states, presentation::Refresh},
};

use super::{Device, Views, cursor};
use crate::{config::Touchpad, frame::FrameClock, render::SceneElement, state::State};

type Api = GbmGlesBackend<GlesRenderer, DrmDeviceFd>;
type Multi<'a> = MultiRenderer<'a, 'a, Api, Api>;
type Compositor =
    DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, Option<smithay::desktop::utils::OutputPresentationFeedback>, DrmDeviceFd>;
type Error = Box<dyn std::error::Error>;
/// Named cursors by (icon, px size): picture, hotspot and size in physical px; `None`: the theme has none.
type Cursors = HashMap<(CursorIcon, u32), Option<(MemoryRenderBuffer, Point<i32, Physical>, Size<i32, Physical>)>>;

/// 8-bit only in v1 (no HDR or 10-bit colour, E6.2).
const FORMATS: [Fourcc; 2] = [Fourcc::Argb8888, Fourcc::Xrgb8888];
const CLEAR: [f32; 4] = [0.1, 0.1, 0.1, 1.0];

pub struct Udev {
    session: LibSeatSession,
    libinput: Libinput,
    /// The render node everything is drawn on.
    primary: DrmNode,
    gpus: GpuManager<Api>,
    cards: HashMap<DrmNode, Card>,
    cursors: Cursors,
}

/// One GPU and the displays wired to it.
struct Card {
    drm: DrmDevice,
    gbm: GbmDevice<DrmDeviceFd>,
    render: DrmNode,
    token: RegistrationToken,
    screens: HashMap<crtc::Handle, Screen>,
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
    state.device = Some(Device::Udev(Box::new(Udev { session, libinput, primary, gpus, cards: HashMap::new(), cursors: HashMap::new() })));
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

    event_loop.handle().insert_source(udev, |event, _, state| match event {
        UdevEvent::Added { device_id, path } => match state.card_added(device_id, &path) {
            Ok(node) => state.scan_connectors(node),
            Err(err) => tracing::warn!("{}: {err}", path.display()),
        },
        UdevEvent::Changed { device_id } => {
            if let Ok(node) = DrmNode::from_dev_id(device_id) {
                state.scan_connectors(node);
            }
        }
        UdevEvent::Removed { device_id } => {
            if let Ok(node) = DrmNode::from_dev_id(device_id) {
                state.card_removed(node);
            }
        }
    })?;
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

    /// Opens a GPU through the session and adds it to the renderers; its displays light in `scan_connectors`.
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
        udev.cards.insert(node, Card { drm, gbm, render, token, screens: HashMap::new() });
        Ok(node)
    }

    /// A GPU went away (an eGPU unplugged): its displays go dark; their windows stay where they are.
    fn card_removed(&mut self, node: DrmNode) {
        let Some(udev) = self.udev() else { return };
        let Some(card) = udev.cards.remove(&node) else { return };
        udev.gpus.as_mut().remove_node(&card.render);
        self.loop_handle.remove(card.token);
        for screen in card.screens.into_values() {
            self.space.unmap_output(&screen.output);
            self.display_handle.remove_global::<State>(screen.global);
        }
        self.schedule_redraw();
    }

    /// Lights newly connected displays and drops unplugged ones (E6.2 hotplug). New ones go right of the others.
    // ponytail: no remembered layouts per monitor set, no 500 ms settle for docks, no clamshell; windows of an
    // unplugged display stay at their place (clamp them onto a remaining output when that matters).
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
        let new: Vec<&connector::Info> = connected.iter().filter(|i| !card.screens.values().any(|s| s.connector == i.handle())).collect();
        for info in new {
            let others: Vec<_> = self.space.outputs().filter_map(|o| self.space.output_geometry(o)).collect();
            match light(card, &mut udev.gpus, info, &res, place_right(&others), &self.display_handle) {
                Ok((crtc, screen)) => {
                    let geo = screen.output.current_mode().map(|m| m.size);
                    tracing::info!(?geo, scale = screen.output.current_scale().fractional_scale(), "display {} lit", screen.output.name());
                    self.space.map_output(&screen.output, place_right(&others));
                    layer_map_for_output(&screen.output).arrange();
                    card.screens.insert(crtc, screen);
                }
                Err(err) => tracing::warn!("display {}-{}: {err}", info.interface().as_str(), info.interface_id()),
            }
        }
        self.schedule_redraw();
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
        let Some(card) = udev.cards.get_mut(&node) else { return };
        let Some(screen) = card.screens.get_mut(&crtc) else { return };
        screen.phase = Phase::Idle;
        if !udev.session.is_active() {
            return;
        }
        let started = Instant::now();
        let renderer = match card.render == primary {
            true => udev.gpus.single_renderer(&primary),
            false => udev.gpus.renderer(&primary, &card.render, screen.compositor.format()),
        };
        let mut renderer = match renderer {
            Ok(renderer) => renderer,
            Err(err) => return tracing::warn!("{}: no renderer: {err}", screen.output.name()),
        };
        let Some((elements, animating)) = self.frame_elements(renderer.as_mut(), &screen.output) else { return };
        let mut all: Vec<OnGpu> = self.cursor_elements(renderer.as_mut(), &mut udev.cursors, &screen.output).into_iter().map(OnGpu).collect();
        all.extend(elements.into_iter().map(OnGpu));
        let frame = screen.compositor.render_frame(&mut renderer, &all, CLEAR, FrameFlags::DEFAULT).map(|f| f.is_empty);
        drop(renderer);
        screen.render_time = started.elapsed();
        match frame {
            Ok(false) => {
                let feedback = self.frame_callbacks(&screen.output);
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
                let _ = self.frame_callbacks(&screen.output);
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
            Err(err) => tracing::warn!("{}: render: {err}", screen.output.name()),
        }
        if animating {
            screen.again = true;
        }
        self.frame_done();
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
        match &self.protocols.cursor {
            CursorImageStatus::Hidden => Vec::new(),
            CursorImageStatus::Surface(surface) => {
                let hotspot = with_states(surface, |s| s.data_map.get::<CursorImageSurfaceData>().and_then(|d| d.lock().ok().map(|d| d.hotspot)));
                let at = (at - hotspot.unwrap_or_default().to_f64().to_physical(scale)).to_i32_round();
                render_elements_from_surface_tree(renderer, surface, at, scale, 1.0, Kind::Cursor).into_iter().map(SceneElement::Surface).collect()
            }
            CursorImageStatus::Named(icon) => {
                let px = (f64::from(cursor::size()) * scale).round() as u32;
                let entry = cache.entry((*icon, px)).or_insert_with(|| {
                    let names: Vec<&str> = [icon.name()].into_iter().chain(icon.alt_names().iter().copied()).chain(["default", "left_ptr"]).collect();
                    let image = cursor::load(&names, px)?;
                    let (w, h) = (image.size.0 as i32, image.size.1 as i32);
                    let buffer = MemoryRenderBuffer::from_slice(&image.pixels, Fourcc::Argb8888, (w, h), 1, Transform::Normal, None);
                    Some((buffer, Point::from((image.hotspot.0 as i32, image.hotspot.1 as i32)), Size::from((w, h))))
                });
                let Some((buffer, hotspot, size)) = entry else { return Vec::new() };
                // Drawn 1:1 in physical px: the theme's picture nearest the scaled size, never resampled.
                let logical = size.to_f64().to_logical(scale).to_i32_round();
                MemoryRenderBufferRenderElement::from_buffer(renderer, at - hotspot.to_f64(), buffer, None, None, Some(logical), Kind::Cursor)
                    .ok()
                    .map(SceneElement::Memory)
                    .into_iter()
                    .collect()
            }
        }
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
                feedback.presented(time, Refresh::fixed(refresh(&screen.output)), seq, K::Vsync | K::HwClock | K::HwCompletion);
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
            }
        }
    }

    fn libinput_event(&mut self, event: InputEvent<LibinputInputBackend>) {
        match event {
            InputEvent::DeviceAdded { mut device } => configure(&mut device, &self.config.touchpad),
            InputEvent::PointerMotion { event } => self.relative_motion(event.delta(), event.time()),
            event => {
                let moved = matches!(event, InputEvent::PointerMotionAbsolute { .. });
                self.process_input_event(event);
                // The pointer is ours to draw.
                if moved {
                    self.schedule_redraw();
                }
            }
        }
    }

    /// A mouse or touchpad moved: the pointer moves by `delta`, kept on the displays.
    fn relative_motion(&mut self, delta: Point<f64, Logical>, utime: u64) {
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

/// Picks a CRTC and mode, and makes the output, its global and its DRM compositor.
fn light(
    card: &mut Card,
    gpus: &mut GpuManager<Api>,
    info: &connector::Info,
    res: &ResourceHandles,
    at: Point<i32, Logical>,
    dh: &DisplayHandle,
) -> Result<(crtc::Handle, Screen), Error> {
    let crtc = info
        .encoders()
        .iter()
        .filter_map(|e| card.drm.get_encoder(*e).ok())
        .flat_map(|e| res.filter_crtcs(e.possible_crtcs()))
        .find(|c| !card.screens.contains_key(c))
        .ok_or("no free CRTC")?;
    let modes = info.modes();
    let list: Vec<_> = modes
        .iter()
        .map(|m| {
            let mode = Mode::from(*m);
            (mode.size.w, mode.size.h, mode.refresh, m.mode_type().contains(ModeTypeFlags::PREFERRED))
        })
        .collect();
    let drm_mode = modes[pick_mode(&list).ok_or("no modes")?];
    let surface = card.drm.create_surface(crtc, drm_mode, &[info.handle()])?;

    let (mm_w, mm_h) = info.size().unwrap_or((0, 0));
    // ponytail: make and model need the EDID (libdisplay-info); the connector name tells displays apart meanwhile.
    let properties =
        PhysicalProperties { size: (mm_w as i32, mm_h as i32).into(), subpixel: subpixel(info.subpixel()), make: "Unknown".into(), model: "Unknown".into() };
    let output = Output::new(format!("{}-{}", info.interface().as_str(), info.interface_id()), properties);
    let mode = Mode::from(drm_mode);
    for m in modes {
        output.add_mode(Mode::from(*m));
    }
    output.set_preferred(mode);
    let scale = pick_scale((mode.size.w, mode.size.h), (mm_w, mm_h));
    output.change_current_state(Some(mode), Some(Transform::Normal), Some(OutputScale::Fractional(scale)), Some(at));
    let global = output.create_global::<State>(dh);

    let formats = gpus.single_renderer(&card.render)?.as_mut().egl_context().dmabuf_render_formats().clone();
    let allocator = GbmAllocator::new(card.gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
    let exporter = GbmFramebufferExporter::new(card.gbm.clone(), Some(card.render));
    let compositor = DrmCompositor::new(&output, surface, None, allocator, exporter, FORMATS, formats, card.drm.cursor_size(), Some(card.gbm.clone()))
        .map_err(|e| format!("{e}"))?;
    let clock = FrameClock::new(refresh(&output));
    Ok((crtc, Screen { output, global, connector: info.handle(), compositor, clock, phase: Phase::Idle, again: false, render_time: Duration::ZERO }))
}

/// The output's refresh period.
fn refresh(output: &Output) -> Duration {
    let mhz = output.current_mode().map_or(60_000, |m| m.refresh).max(1_000);
    Duration::from_nanos(1_000_000_000_000 / mhz as u64)
}

/// The touchpad settings (E1.7), applied as a device appears.
// ponytail: a config reload reaches touchpads at their next plug-in; keep the devices to re-apply it live.
fn configure(device: &mut InputDevice, touchpad: &Touchpad) {
    if device.config_tap_finger_count() > 0 {
        let _ = device.config_tap_set_enabled(touchpad.tap_to_click);
        let _ = device.config_scroll_set_natural_scroll_enabled(touchpad.natural_scroll);
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

/// The mode to light a display with (E6.2): its preferred resolution (else the largest) at the highest refresh rate.
/// `modes`: (width, height, refresh in mHz, preferred).
fn pick_mode(modes: &[(i32, i32, i32, bool)]) -> Option<usize> {
    let &(w, h, ..) = modes.iter().find(|m| m.3).or_else(|| modes.iter().max_by_key(|m| m.0 * m.1))?;
    modes.iter().enumerate().filter(|(_, m)| (m.0, m.1) == (w, h)).max_by_key(|(_, m)| m.2).map(|(i, _)| i)
}

/// The scale a display starts at (E6.2, D13): about 110 px per inch per 100%, in 25% steps from 100 to 200%, never
/// leaving less than 1280 × 720 logical px. Without a believable physical size (projectors, TVs): 100%.
fn pick_scale((w, h): (i32, i32), (mm_w, _): (u32, u32)) -> f64 {
    if mm_w < 100 || w <= 0 {
        return 1.0;
    }
    let dpi = f64::from(w) * 25.4 / f64::from(mm_w);
    let mut scale = ((dpi / 110.0) * 4.0).round().clamp(4.0, 8.0) / 4.0;
    while scale > 1.0 && (f64::from(w) / scale < 1280.0 || f64::from(h) / scale < 720.0) {
        scale -= 0.25;
    }
    scale
}

/// Where a new display goes: right of the rightmost one, tops aligned.
fn place_right(others: &[Rectangle<i32, Logical>]) -> Point<i32, Logical> {
    (others.iter().map(|g| g.loc.x + g.size.w).max().unwrap_or(0), 0).into()
}

/// `to`, if it is on a display; else `to` held inside the display the pointer is on (or the first).
fn clamp_pointer(screens: &[Rectangle<i32, Logical>], from: Point<f64, Logical>, to: Point<f64, Logical>) -> Point<f64, Logical> {
    if screens.iter().any(|g| g.to_f64().contains(to)) {
        return to;
    }
    let Some(g) = screens.iter().find(|g| g.to_f64().contains(from)).or(screens.first()).map(|g| g.to_f64()) else { return to };
    (to.x.clamp(g.loc.x, g.loc.x + g.size.w - 1.0), to.y.clamp(g.loc.y, g.loc.y + g.size.h - 1.0)).into()
}

/// The VT of an XF86Switch_VT_<n> keysym.
fn vt_of(sym: Keysym) -> Option<i32> {
    let first = Keysym::XF86_Switch_VT_1.raw();
    (first..=Keysym::XF86_Switch_VT_12.raw()).contains(&sym.raw()).then(|| (sym.raw() - first + 1) as i32)
}

/// A scene element drawn by the render GPU's GLES frame inside a multi-GPU frame.
struct OnGpu(SceneElement);

impl Element for OnGpu {
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

impl<'a> RenderElement<Multi<'a>> for OnGpu {
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

    #[test]
    fn mode_is_preferred_size_at_highest_refresh() {
        let modes = [(1920, 1080, 60_000, false), (2560, 1440, 59_951, true), (2560, 1440, 143_912, false), (3840, 2160, 30_000, false)];
        assert_eq!(pick_mode(&modes), Some(2));
        // No preferred mode: the largest.
        assert_eq!(pick_mode(&modes.map(|m| (m.0, m.1, m.2, false))), Some(3));
        assert_eq!(pick_mode(&[]), None);
    }

    #[test]
    fn scale_follows_pixel_density() {
        assert_eq!(pick_scale((1920, 1080), (531, 299)), 1.0, "24-inch 1080p");
        assert_eq!(pick_scale((2560, 1440), (597, 336)), 1.0, "27-inch 1440p");
        assert_eq!(pick_scale((3840, 2160), (597, 336)), 1.5, "27-inch 4K");
        assert_eq!(pick_scale((1920, 1080), (344, 194)), 1.25, "15.6-inch 1080p laptop");
        assert_eq!(pick_scale((1920, 1080), (294, 165)), 1.5, "13.3-inch 1080p: 1280 × 720 logical");
        assert_eq!(pick_scale((2560, 1600), (286, 179)), 2.0, "13.3-inch 2560 × 1600");
        assert_eq!(pick_scale((3840, 2160), (344, 194)), 2.0, "15.6-inch 4K, capped at 200%");
        assert_eq!(pick_scale((1366, 768), (160, 90)), 1.0, "never under 1280 × 720 logical");
        assert_eq!(pick_scale((1920, 1080), (16, 9)), 1.0, "an aspect ratio, not a size");
        assert_eq!(pick_scale((1920, 1080), (0, 0)), 1.0);
    }

    #[test]
    fn new_display_goes_right() {
        assert_eq!(place_right(&[]), Point::from((0, 0)));
        let screens = [Rectangle::new((0, 0).into(), (1280, 800).into()), Rectangle::new((1280, 0).into(), (2560, 1440).into())];
        assert_eq!(place_right(&screens), Point::from((3840, 0)));
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
    }

    #[test]
    fn vt_keys() {
        assert_eq!(vt_of(Keysym::XF86_Switch_VT_1), Some(1));
        assert_eq!(vt_of(Keysym::XF86_Switch_VT_12), Some(12));
        assert_eq!(vt_of(Keysym::F1), None);
    }
}
