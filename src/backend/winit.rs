//! Development backend: the desktop runs as a window on the current session (`--nested`).

use std::time::{Duration, Instant};

use smithay::{
    backend::{
        egl::EGLDevice,
        renderer::{ImportDma, damage::OutputDamageTracker, gles::GlesRenderer},
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    desktop::{layer_map_for_output, utils::OutputPresentationFeedback},
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{EventLoop, channel},
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::Transform,
    wayland::{compositor::SurfaceData, dmabuf::DmabufFeedbackBuilder, presentation::Refresh},
};

use crate::{
    frame::FrameClock,
    render::{self, CalendarPopover, Gallery, MenuBar, Shaders, Wallpaper},
    state::State,
};

const REFRESH_MHZ: i32 = 60_000;

pub struct Winit {
    pub backend: WinitGraphicsBackend<GlesRenderer>,
    output: Output,
    damage_tracker: OutputDamageTracker,
    shaders: Shaders,
    pub wallpaper: Wallpaper,
    pub clock: FrameClock,
    /// The `--test-scene` material gallery (E1.4), drawn over the wallpaper when set.
    pub gallery: Option<Gallery>,
    pub menu_bar: MenuBar,
    calendar: CalendarPopover,
}

pub fn init(event_loop: &mut EventLoop<State>, state: &mut State) -> Result<(), Box<dyn std::error::Error>> {
    let (mut backend, winit) = winit::init::<GlesRenderer>()?;
    backend.window().set_title("Noobsbot Desktop");

    // linux-dmabuf v4 tells apps which GPU and formats we import (E1.1); without a render node, v3 still works.
    let formats = backend.renderer().dmabuf_formats();
    let node = EGLDevice::device_for_display(backend.renderer().egl_context().display()).and_then(|device| device.try_get_render_node());
    if let Ok(Some(node)) = &node {
        crate::sync::init(state, *node);
    }
    match node.map(|node| node.map(|node| DmabufFeedbackBuilder::new(node.dev_id(), formats.clone()).build())) {
        Ok(Some(Ok(feedback))) => {
            state.dmabuf_state.create_global_with_default_feedback::<State>(&state.display_handle, &feedback);
        }
        other => {
            tracing::warn!("no render node for dmabuf feedback ({other:?}), using linux-dmabuf v3");
            state.dmabuf_state.create_global::<State>(&state.display_handle, formats);
        }
    }

    let mode = Mode { size: backend.window_size(), refresh: REFRESH_MHZ };
    let output =
        Output::new("winit".into(), PhysicalProperties { size: (0, 0).into(), subpixel: Subpixel::Unknown, make: "Noobsbot".into(), model: "Nested".into() });
    let _global = output.create_global::<State>(&state.display_handle);
    // winit's GL surface is upside down relative to Wayland; Flipped180 corrects it.
    output.change_current_state(Some(mode), Some(Transform::Flipped180), None, Some((0, 0).into()));
    output.set_preferred(mode);
    state.space.map_output(&output, (0, 0));
    let damage_tracker = OutputDamageTracker::from_output(&output);

    // Shaders compile once; wallpapers decode on a worker thread and upload when they arrive.
    let shaders = Shaders::new(backend.renderer())?;
    let (tx, rx) = channel::channel();
    event_loop.handle().insert_source(rx, |event, _, state| {
        if let (channel::Event::Msg(decoded), Some(winit)) = (event, state.backend.as_mut()) {
            winit.wallpaper.receive(winit.backend.renderer(), decoded);
            state.schedule_redraw();
        }
    })?;
    let mut wallpaper = Wallpaper::new(tx);
    wallpaper.set(&state.config.appearance);
    let clock = FrameClock::new(Duration::from_nanos(1_000_000_000_000 / REFRESH_MHZ as u64));
    state.backend = Some(Winit { backend, output, damage_tracker, shaders, wallpaper, clock, gallery: None, menu_bar: MenuBar::default(), calendar: CalendarPopover::default() });

    event_loop.handle().insert_source(winit, |event, _, state| match event {
        WinitEvent::Resized { size, .. } => {
            if let Some(winit) = &state.backend {
                let mode = Mode { size, refresh: REFRESH_MHZ };
                winit.output.change_current_state(Some(mode), None, None, None);
                winit.output.set_preferred(mode);
                layer_map_for_output(&winit.output).arrange();
            }
            state.schedule_redraw();
        }
        WinitEvent::Input(event) => state.process_input_event(event),
        WinitEvent::Redraw => state.schedule_redraw(),
        WinitEvent::CloseRequested => state.loop_signal.stop(),
        _ => {}
    })?;

    Ok(())
}

impl State {
    /// Draws one frame and tells apps it was shown. Runs only when something changed (see `schedule_redraw`).
    pub fn render(&mut self) {
        let content = self.menu_bar();
        let Some(Winit { backend, output, damage_tracker, shaders, wallpaper, clock, gallery, menu_bar, calendar }) = self.backend.as_mut() else { return };
        let started = Instant::now();
        let age = backend.buffer_age().unwrap_or(0);
        let damage = match backend.bind() {
            Ok((renderer, mut framebuffer)) => {
                // Wallpaper, layers, windows with rounded corners and shadows (E1.4, E1.5): see render::scene.
                let scale = output.current_scale().fractional_scale();
                let appearance = &self.config.appearance;
                let dark = render::is_dark(appearance);
                let width = self.space.output_geometry(output).map_or(0, |g| g.size.w);
                // Popups in front of the bar, the bar in front of windows.
                let mut bar = Vec::new();
                if let Some(crate::shell::Popup::Calendar { year, month }) = self.popup {
                    let shown = render::calendar::Month { year, month, week_start: self.config.clock.week_start.into(), today: chrono::Local::now().date_naive() };
                    let accent = render::accent(appearance.accent, dark);
                    bar.extend(calendar.elements(renderer, shaders, &mut self.text, &mut self.symbols, &shown, width, dark, accent, appearance.glass_opacity, scale));
                }
                let active = self.popup.map(|crate::shell::Popup::Calendar { .. }| render::menubar::BarItem::Clock);
                match &content {
                    Some(content) => bar.extend(menu_bar.elements(
                        renderer,
                        shaders,
                        &mut self.text,
                        &mut self.symbols,
                        content,
                        active,
                        width,
                        dark,
                        appearance.glass_opacity,
                        scale,
                    )),
                    None => menu_bar.hide(),
                }
                let mut elements = render::scene(renderer, shaders, wallpaper, &mut self.text, &self.space, output, appearance, bar);
                if let (Some(gallery), Some(geo)) = (gallery.as_mut(), self.space.output_geometry(output)) {
                    let cards = gallery.elements(renderer, shaders, &mut self.text, geo.size, self.config.appearance.glass_opacity, scale);
                    // Above the wallpaper (the last element), below every window.
                    elements.splice(elements.len().saturating_sub(1)..elements.len().saturating_sub(1), cards);
                }
                damage_tracker.render_output(renderer, &mut framebuffer, age, &elements, [0.1, 0.1, 0.1, 1.0]).map(|result| {
                    if gallery.is_some() {
                        // The test scene is a benchmark: wait for the GPU so the time includes its work (E1.4 budget).
                        // Not glFinish via with_context: that unbinds the window surface and the swap then fails.
                        let cpu = started.elapsed();
                        let _ = result.sync.wait();
                        tracing::debug!(?cpu, cpu_gpu = ?started.elapsed(), "test scene frame");
                    }
                    result.damage.cloned()
                })
            }
            Err(err) => {
                tracing::warn!("bind: {err}");
                return;
            }
        };
        // Always swap, even without damage: the swap waits for vblank, which keeps an app that commits
        // in a loop at the refresh rate. When drawing starts is up to frame.rs (E1.10, see schedule_redraw).
        let render_time = started.elapsed();
        let shown = match damage {
            Ok(damage) => backend.submit(damage.as_deref()).inspect_err(|err| tracing::warn!("submit: {err}")).is_ok(),
            Err(err) => {
                tracing::warn!("render: {err}");
                false
            }
        };

        let benchmark = gallery.is_some();
        // Everything on this output was shown. ponytail: stamped after the swap returns, without a vblank counter;
        // the real flip time and sequence come from DRM page-flip events (M6). Feedback dropped unsent is "discarded".
        let mut feedback = OutputPresentationFeedback::new(output);
        let on_output = |_: &WlSurface, _: &SurfaceData| Some(output.clone());
        let elapsed = self.start_time.elapsed();
        for window in self.space.elements() {
            window.send_frame(output, elapsed, Some(Duration::ZERO), on_output);
            window.take_presentation_feedback(&mut feedback, on_output, |_, _| wp_presentation_feedback::Kind::empty());
        }
        let mut layers = layer_map_for_output(output);
        for layer in layers.layers() {
            layer.send_frame(output, elapsed, Some(Duration::ZERO), on_output);
            layer.take_presentation_feedback(&mut feedback, on_output, |_, _| wp_presentation_feedback::Kind::empty());
        }
        layers.cleanup();
        drop(layers);
        if shown {
            // ponytail: nested, the swap returning stands in for the vblank and only CPU render time is measured; DRM
            // page-flip events and GPU timestamps give the real ones (M6).
            clock.presented(Instant::now(), render_time);
            tracing::trace!(?render_time, "frame");
            let refresh = Refresh::fixed(Duration::from_nanos(1_000_000_000_000 / REFRESH_MHZ as u64));
            feedback.presented(self.protocols.clock.now(), refresh, 0, wp_presentation_feedback::Kind::empty());
        }
        self.space.refresh();
        self.popups.cleanup();
        let _ = self.display_handle.flush_clients();
        // The test scene redraws every frame (its cards are rebuilt each time, so each frame repaints them).
        if benchmark {
            self.schedule_redraw();
        }
    }
}
