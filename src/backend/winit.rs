//! Development backend: the desktop runs as a window on the current session (`--nested`).

use std::time::Duration;

use smithay::{
    backend::{
        renderer::{ImportDma, damage::OutputDamageTracker, element::surface::WaylandSurfaceRenderElement, gles::GlesRenderer},
        winit::{self, WinitEvent},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    utils::Transform,
};

use crate::state::State;

const REFRESH_MHZ: i32 = 60_000;

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
    state.backend = Some(backend);

    let mut damage_tracker = OutputDamageTracker::from_output(&output);

    event_loop.handle().insert_source(winit, move |event, _, state| match event {
        WinitEvent::Resized { size, .. } => {
            let mode = Mode { size, refresh: REFRESH_MHZ };
            output.change_current_state(Some(mode), None, None, None);
            output.set_preferred(mode);
        }
        WinitEvent::Input(event) => state.process_input_event(event),
        WinitEvent::Redraw => {
            let Some(backend) = state.backend.as_mut() else { return };
            let age = backend.buffer_age().unwrap_or(0);
            let damage = match backend.bind() {
                Ok((renderer, mut framebuffer)) => smithay::desktop::space::render_output::<_, WaylandSurfaceRenderElement<GlesRenderer>, _, _>(
                    &output,
                    renderer,
                    &mut framebuffer,
                    1.0,
                    age,
                    [&state.space],
                    &[],
                    &mut damage_tracker,
                    [0.1, 0.1, 0.1, 1.0],
                )
                .map(|result| result.damage.cloned()),
                Err(err) => {
                    tracing::warn!("bind: {err}");
                    return;
                }
            };
            // Always swap: the swap's vsync wait is what paces this loop (skipping it would spin).
            match damage {
                Ok(damage) => {
                    if let Err(err) = backend.submit(damage.as_deref()) {
                        tracing::warn!("submit: {err}");
                    }
                }
                Err(err) => tracing::warn!("render: {err}"),
            }
            backend.window().request_redraw();

            let elapsed = state.start_time.elapsed();
            state.space.elements().for_each(|window| window.send_frame(&output, elapsed, Some(Duration::ZERO), |_, _| Some(output.clone())));
            state.space.refresh();
            state.popups.cleanup();
            let _ = state.display_handle.flush_clients();
        }
        WinitEvent::CloseRequested => state.loop_signal.stop(),
        _ => {}
    })?;

    Ok(())
}
