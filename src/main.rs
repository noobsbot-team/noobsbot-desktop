//! Noobsbot Desktop: start-up, logging and backend selection.

mod backend;
mod clipboard;
mod input;
mod state;

use smithay::reexports::{calloop::EventLoop, wayland_server::Display};
use tracing_subscriber::EnvFilter;

use crate::state::State;

const USAGE: &str = "usage: noobsbot-desktop [--nested] [-- command [args...]]";

fn main() {
    tracing_subscriber::fmt().with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))).init();

    let mut args = std::env::args().skip(1);
    let mut command = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--nested" => {}
            "--" => command.extend(args.by_ref()),
            "-h" | "--help" => return println!("{USAGE}"),
            other => {
                eprintln!("unknown argument: {other}\n{USAGE}");
                std::process::exit(2);
            }
        }
    }

    if let Err(err) = run(command) {
        tracing::error!("{err}");
        std::process::exit(1);
    }
}

fn run(command: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<'static, State> = EventLoop::try_new()?;
    let display: Display<State> = Display::new()?;
    let mut state = State::new(&mut event_loop, display)?;

    backend::winit::init(&mut event_loop, &mut state)?;

    let socket = state.socket_name.to_string_lossy().into_owned();
    tracing::info!("listening on WAYLAND_DISPLAY={socket}");
    if let Some((program, args)) = command.split_first() {
        // ponytail: fire and forget; supervised launching lives in apps.rs (M3).
        if let Err(err) = std::process::Command::new(program).args(args).env("WAYLAND_DISPLAY", &socket).spawn() {
            tracing::warn!("could not start {program}: {err}");
        }
    }

    event_loop.run(None, &mut state, |_| {})?;
    Ok(())
}
