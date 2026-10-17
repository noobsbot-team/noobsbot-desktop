//! Noobsbot Desktop: start-up, logging and backend selection.

mod backend;
mod calendar;
mod clock;
mod clipboard;
mod config;
mod decorations;
mod frame;
mod grabs;
mod input;
mod protocols;
mod render;
mod rules;
mod services;
mod shortcuts;
mod state;
mod sync;
mod text;
mod wm;

use smithay::reexports::{
    calloop::{EventLoop, channel},
    wayland_server::Display,
};
use tracing_subscriber::EnvFilter;

use crate::state::State;

const USAGE: &str = "usage: noobsbot-desktop [--nested] [--test-scene] [-- command [args...]]";

fn main() {
    tracing_subscriber::fmt().with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))).init();

    // Crash → clean exit (E1.11): the panic and its backtrace reach the log before `panic = "abort"` ends the process.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info.location().map_or_else(|| "?".into(), |l| l.to_string());
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!("panic at {location}: {}\n{backtrace}", info.payload_as_str().unwrap_or("(no message)"));
        default_hook(info);
    }));

    let mut args = std::env::args().skip(1);
    let mut command = Vec::new();
    let mut nested = false;
    let mut test_scene = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--nested" => nested = true,
            // Every glass material in light and dark over the wallpaper (E1.4 done check).
            "--test-scene" => test_scene = true,
            "--" => command.extend(args.by_ref()),
            "-h" | "--help" => return println!("{USAGE}"),
            other => {
                eprintln!("unknown argument: {other}\n{USAGE}");
                std::process::exit(2);
            }
        }
    }

    if let Err(err) = run(command, nested, test_scene) {
        tracing::error!("{err}");
        std::process::exit(1);
    }
}

fn run(command: Vec<String>, nested: bool, test_scene: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<'static, State> = EventLoop::try_new()?;
    let display: Display<State> = Display::new()?;
    let config_path = config::path();
    let mut state = State::new(&mut event_loop, display, nested, config::load(&config_path))?;

    // Saving the settings file applies it at once (E1.8).
    let (tx, rx) = channel::channel();
    config::watch(&config_path, move |config| tx.send(config).is_ok());
    event_loop.handle().insert_source(rx, |event, _, state| {
        if let channel::Event::Msg(config) = event {
            tracing::info!("settings reloaded");
            if config.keyboard != state.config.keyboard
                && let Some(keyboard) = state.seat.get_keyboard()
            {
                let resolved = config.keyboard.resolved();
                if let Err(err) = keyboard.set_xkb_config(state, input::xkb(&resolved)) {
                    tracing::warn!(keyboard = ?resolved, "keyboard layout: {err}, keeping the previous one");
                }
            }
            if config.appearance != state.config.appearance {
                if let Some(winit) = state.backend.as_mut() {
                    winit.wallpaper.set(&config.appearance);
                }
                state.schedule_redraw();
            }
            if config.clock.timezone != state.config.clock.timezone && !config.clock.timezone.is_empty() {
                state.services.send(services::Command::Timezone(config.clock.timezone.clone()));
            }
            state.bindings = shortcuts::from_config(&config);
            state.rules = rules::compile(&config.rules);
            state.config = config;
        }
    })?;

    backend::winit::init(&mut event_loop, &mut state)?;
    if test_scene && let Some(winit) = state.backend.as_mut() {
        winit.gallery = Some(Default::default());
    }

    tracing::info!("listening on WAYLAND_DISPLAY={}", state.socket_name.to_string_lossy());
    if !command.is_empty() {
        state.spawn(&command);
    }

    // Replies queued while handling events go out once per loop turn.
    event_loop.run(None, &mut state, |state| {
        let _ = state.display_handle.flush_clients();
    })?;
    Ok(())
}
