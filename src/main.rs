//! Noobsbot Desktop: start-up, logging and backend selection.

mod apps;
mod backend;
mod calendar;
mod clipboard;
mod clock;
mod config;
mod decorations;
mod frame;
mod grabs;
mod input;
mod protocols;
mod render;
mod rules;
mod services;
mod shell;
mod shortcuts;
mod state;
mod symbols;
mod sync;
mod text;
mod theme;
mod ui;
mod wm;

use smithay::reexports::{
    calloop::{EventLoop, channel},
    wayland_server::Display,
};
use tracing_subscriber::EnvFilter;

use crate::state::State;

const USAGE: &str = "usage: noobsbot-desktop [--nested] [--test-scene] [--show notification|notification-center|logo-menu|app-menu|control|control-sound|control-wifi|control-bluetooth|osd|alert|alert-pair|alert-code|alert-wifi|alert-pin|dock-zoom|gallery|switcher|force-quit] [-- command [args...]]\n       noobsbot-desktop theme --write [--gsettings]";

/// `1.3.0 (Opal) · 2026-10-17 · a1b2c3d` (E6.13); the date and commit come from the release build (release.yml).
// ponytail: the codename is fixed until 2.0 (Pearl) exists.
fn version() -> String {
    let parts = [option_env!("NOOBSBOT_DATE"), option_env!("NOOBSBOT_COMMIT")];
    parts.into_iter().flatten().fold(format!("{} (Opal)", env!("CARGO_PKG_VERSION")), |v, part| format!("{v} · {part}"))
}

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

    // `theme --write`: the app theme generator (E2.9), no compositor.
    if std::env::args().nth(1).as_deref() == Some("theme") {
        std::process::exit(theme::cli(std::env::args().skip(2)));
    }

    let mut args = std::env::args().skip(1);
    let mut command = Vec::new();
    let mut nested = false;
    let mut test_scene = false;
    let mut show = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--nested" => nested = true,
            // Every glass material in light and dark over the wallpaper (E1.4 done check).
            "--test-scene" => test_scene = true,
            // Opens a popup at start, for screenshots of states that otherwise need a click.
            "--show" => show = args.next(),
            "--" => command.extend(args.by_ref()),
            "-h" | "--help" => return println!("{USAGE}"),
            "-V" | "--version" => return println!("{}", version()),
            other => {
                eprintln!("unknown argument: {other}\n{USAGE}");
                std::process::exit(2);
            }
        }
    }

    if let Err(err) = run(command, nested, test_scene, show.as_deref()) {
        tracing::error!("{err}");
        std::process::exit(1);
    }
}

fn run(command: Vec<String>, nested: bool, test_scene: bool, show: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
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
    match show {
        // Sample notifications: one banner (hovered, so its buttons show), or Notification Center with DND on.
        Some("notification") => {
            state.restore_notifications(shell::notify::samples());
            state.notifications.banners = vec![4, 1];
            state.notifications.hovered = Some(4);
        }
        Some("notification-center") => {
            state.restore_notifications(shell::notify::samples());
            state.dnd = true;
            let (year, month) = shell::this_month();
            state.popup = Some(shell::Popup::Notifications { year, month });
        }
        Some("logo-menu") => state.popup = Some(shell::Popup::Menu(shell::Menu::Logo)),
        Some("app-menu") => state.popup = Some(shell::Popup::Menu(shell::Menu::App)),
        Some("control") => state.popup = Some(shell::Popup::Control),
        Some("control-wifi") => {
            state.popup = Some(shell::Popup::Control);
            state.expanded = Some(shell::control::Module::Wifi);
        }
        Some("control-bluetooth") => {
            state.popup = Some(shell::Popup::Control);
            state.expanded = Some(shell::control::Module::Bluetooth);
        }
        Some("control-sound") => {
            state.popup = Some(shell::Popup::Control);
            state.expanded = Some(shell::control::Module::Sound);
        }
        // No countdown timer: it never acts by itself.
        Some("alert") => state.alert = Some(shell::alert::Alert::power(shell::alert::Power::Restart, std::time::Instant::now())),
        Some("alert-pair") => {
            let pairing = services::bluetooth::Pairing { device: "Magic Keyboard".into(), code: Some("012345".into()), answer: true };
            state.alert = Some(shell::alert::Alert { kind: shell::alert::Kind::Pair(pairing), deadline: None });
        }
        Some("alert-wifi") => {
            let field = ui::field::TextField { text: "hunter22".into(), caret: 8, secure: true, all: false };
            state.alert = Some(shell::alert::Alert { kind: shell::alert::Kind::Wifi { ssid: "Home Network".into(), join: None, field }, deadline: None });
        }
        Some("alert-pin") => {
            let field = ui::field::TextField { text: "0000".into(), caret: 4, secure: false, all: false };
            state.alert = Some(shell::alert::Alert { kind: shell::alert::Kind::Pin { device: "Old Keyboard".into(), passkey: false, field }, deadline: None });
        }
        Some("alert-code") => {
            let pairing = services::bluetooth::Pairing { device: "Magic Keyboard".into(), code: Some("012345".into()), answer: false };
            state.alert = Some(shell::alert::Alert { kind: shell::alert::Kind::Pair(pairing), deadline: None });
        }
        // Both list the windows open by then: apps started with the compositor need a moment to map.
        Some("switcher") => {
            let open = |_: std::time::Instant, _: &mut (), state: &mut State| {
                state.switcher_preview();
                smithay::reexports::calloop::timer::TimeoutAction::Drop
            };
            event_loop.handle().insert_source(smithay::reexports::calloop::timer::Timer::from_duration(std::time::Duration::from_secs(5)), open)?;
        }
        Some("force-quit") => state.open_force_quit(),
        // Stays up: no hide timer.
        Some("osd") => state.osd = Some(shell::Osd { symbol: symbols::Symbol::SpeakerHigh, level: Some(65), text: None }),
        // The dock zoomed as if the pointer were over it, left of centre.
        Some("dock-zoom") => {
            let size = state.space.outputs().next().and_then(|o| state.space.output_geometry(o)).map(|g| g.size).unwrap_or_default();
            state.dock_pointer = Some((f64::from(size.w) / 2.0 - 60.0, f64::from(size.h) - 40.0).into());
        }
        // Every D8 component in its D7 states (E2.1 done check).
        Some("gallery") => {
            if let Some(winit) = state.backend.as_mut() {
                winit.components = Some(Default::default());
            }
        }
        Some(other) => tracing::warn!("--show {other}: unknown popup\n{USAGE}"),
        None => {}
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
