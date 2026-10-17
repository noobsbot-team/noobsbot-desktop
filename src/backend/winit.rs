//! Development backend: the desktop runs as a window on the current session (`--nested`).

use std::time::Duration;

use smithay::{
    backend::{
        renderer::{ImportDma, damage::OutputDamageTracker, element::surface::WaylandSurfaceRenderElement, gles::GlesRenderer},
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    input::pointer::{CursorIcon, CursorImageStatus},
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    utils::Transform,
};

use crate::state::State;

const REFRESH_MHZ: i32 = 60_000;

pub struct Winit {
    pub backend: WinitGraphicsBackend<GlesRenderer>,
    output: Output,
    damage_tracker: OutputDamageTracker,
}

pub fn init(event_loop: &mut EventLoop<State>, state: &mut State) -> Result<(), Box<dyn std::error::Error>> {
    let (mut backend, winit) = winit::init::<GlesRenderer>()?;
    backend.window().set_title("Noobsbot Desktop");

    let formats = backend.renderer().dmabuf_formats();
    state.dmabuf_state.create_global::<State>(&state.display_handle, formats);

    let mode = Mode { size: backend.window_size(), refresh: REFRESH_MHZ };
    let output =
        Output::new("winit".into(), PhysicalProperties { size: (0, 0).into(), subpixel: Subpixel::Unknown, make: "Noobsbot".into(), model: "Nested".into() });
    let _global = output.create_global::<State>(&state.display_handle);
    // winit's GL surface is upside down relative to Wayland; Flipped180 corrects it.
    output.change_current_state(Some(mode), Some(Transform::Flipped180), None, Some((0, 0).into()));
    output.set_preferred(mode);
    state.space.map_output(&output, (0, 0));
    let damage_tracker = OutputDamageTracker::from_output(&output);
    state.backend = Some(Winit { backend, output, damage_tracker });

    event_loop.handle().insert_source(winit, |event, _, state| match event {
        WinitEvent::Resized { size, .. } => {
            if let Some(winit) = &state.backend {
                let mode = Mode { size, refresh: REFRESH_MHZ };
                winit.output.change_current_state(Some(mode), None, None, None);
                winit.output.set_preferred(mode);
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
    /// Shows `image` as the host window's cursor.
    pub fn set_cursor(&self, image: CursorImageStatus) {
        let Some(winit) = &self.backend else { return };
        let window = winit.backend.window();
        window.set_cursor_visible(image != CursorImageStatus::Hidden);
        match image {
            CursorImageStatus::Named(icon) => window.set_cursor(icon),
            // ponytail: an app's own cursor surface shows as the host default; apps get named cursors once cursor-shape
            // is enabled, drawing surfaces needs a cursor plane (udev, M6).
            CursorImageStatus::Surface(_) => window.set_cursor(CursorIcon::Default),
            CursorImageStatus::Hidden => {}
        }
    }

    /// Draws one frame and tells apps it was shown. Runs only when something changed (see `schedule_redraw`).
    pub fn render(&mut self) {
        let Some(Winit { backend, output, damage_tracker }) = self.backend.as_mut() else { return };
        let age = backend.buffer_age().unwrap_or(0);
        let damage = match backend.bind() {
            Ok((renderer, mut framebuffer)) => smithay::desktop::space::render_output::<_, WaylandSurfaceRenderElement<GlesRenderer>, _, _>(
                output,
                renderer,
                &mut framebuffer,
                1.0,
                age,
                [&self.space],
                &[],
                damage_tracker,
                [0.1, 0.1, 0.1, 1.0],
            )
            .map(|result| result.damage.cloned()),
            Err(err) => {
                tracing::warn!("bind: {err}");
                return;
            }
        };
        // Always swap, even without damage: the swap waits for vblank, which keeps an app that commits
        // in a loop at the refresh rate. ponytail: the blocking wait is fine nested; frame.rs (E1.10) replaces it.
        match damage {
            Ok(damage) => {
                if let Err(err) = backend.submit(damage.as_deref()) {
                    tracing::warn!("submit: {err}");
                }
            }
            Err(err) => tracing::warn!("render: {err}"),
        }

        let elapsed = self.start_time.elapsed();
        self.space.elements().for_each(|window| window.send_frame(output, elapsed, Some(Duration::ZERO), |_, _| Some(output.clone())));
        self.space.refresh();
        self.popups.cleanup();
        let _ = self.display_handle.flush_clients();
    }
}
