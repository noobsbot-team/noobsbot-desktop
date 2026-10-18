//! Noobsbot Desktop: start-up, logging and backend selection.

mod anim;
mod apps;
mod backend;
mod calendar;
mod capture;
mod clipboard;
mod clock;
mod config;
mod decorations;
mod frame;
mod gestures;
mod grabs;
mod idle;
mod ime;
mod input;
mod mission;
mod pam;
mod perms;
mod protocols;
mod render;
mod rules;
mod services;
mod shell;
mod shortcuts;
mod spaces;
mod state;
mod symbols;
mod sync;
mod text;
mod theme;
mod ui;
mod wm;
mod xwayland;

use smithay::reexports::{
    calloop::{EventLoop, channel},
    wayland_server::Display,
};
use tracing_subscriber::EnvFilter;

use crate::state::State;

const USAGE: &str = "usage: noobsbot-desktop [--nested] [--greeter] [--test-scene] [--show notification|notification-center|logo-menu|app-menu|input-menu|control|control-sound|control-wifi|control-bluetooth|osd|alert|alert-pair|alert-code|alert-wifi|alert-pin|dock-zoom|dock-badges|dock-menu|gallery|switcher|force-quit|spotlight|spotlight-calc|launchpad|launchpad-search|mission-control|mission-control-bar|hot-corner|update|lock|lock-typing|lock-wrong|greeter|greeter-users|greeter-wrong|screenshot-toolbar|screenshot-region|screenshot-window|screenshot-options|screenshot-thumbnail] [-- command [args...]]\n       noobsbot-desktop theme --write [--gsettings]";

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
    // `dock --import-plank`: the one-time Plank importer (E3.1).
    if std::env::args().nth(1).as_deref() == Some("dock") {
        std::process::exit(shell::plank::cli(std::env::args().skip(2)));
    }
    // `--trusted <command>`: runs it on the trusted socket of the running desktop (E6.11).
    if std::env::args().nth(1).as_deref() == Some("--trusted") {
        std::process::exit(perms::exec_trusted(std::env::args().skip(2)));
    }

    let mut args = std::env::args().skip(1);
    let mut command = Vec::new();
    let mut nested = false;
    let mut test_scene = false;
    let mut greeter = false;
    let mut show = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--nested" => nested = true,
            // The login screen, started by greetd (E7.2).
            "--greeter" => greeter = true,
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

    // Inside another desktop, the real screen belongs to it: run nested rather than fight it for the GPU.
    if !nested && ["WAYLAND_DISPLAY", "DISPLAY"].iter().any(|v| std::env::var_os(v).is_some()) {
        tracing::info!("started inside another desktop, running nested");
        nested = true;
    }
    if let Err(err) = run(command, nested, greeter, test_scene, show.as_deref()) {
        tracing::error!("{err}");
        std::process::exit(1);
    }
}

fn run(mut command: Vec<String>, nested: bool, greeter: bool, test_scene: bool, show: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<'static, State> = EventLoop::try_new()?;
    let display: Display<State> = Display::new()?;
    let config_path = config::path();
    let mut state = State::new(&mut event_loop, display, nested, config::load(&config_path))?;
    let codes = state.config.keyboard.resolved().layout;
    state.read_layouts(&codes);

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
                } else {
                    state.read_layouts(&resolved.layout);
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

    if nested {
        backend::winit::init(&mut event_loop, &mut state)?;
    } else {
        backend::udev::init(&mut event_loop, &mut state)?;
    }
    state.idle_arm();
    if greeter {
        state.start_greeter(shell::greeter::Greeter::load());
    }
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
        Some("usb") => shell::disks::sample(&mut state),
        Some("update") => {
            let release =
                services::update::Release { version: semver::Version::new(1, 3, 0), url: "https://github.com/anil0403/noobsbot-desktop/releases".into() };
            // Kept up and hovered (buttons showing) for the screenshot.
            let notice = services::notifications::Notification { expire: 0, ..services::update::notice(&release) };
            state.notifications.hovered = Some(notice.id);
            state.notify(notice);
            state.update = Some(release);
        }
        Some("logo-menu") => state.popup = Some(shell::Popup::Menu(shell::Menu::Logo)),
        Some("app-menu") => state.popup = Some(shell::Popup::Menu(shell::Menu::App)),
        Some("input-menu") => {
            let names = ["English (US)", "Nepali", "Hindi (Bolnagri)"].map(String::from).to_vec();
            state.layouts.list = ["us", "np", "in"].map(String::from).into_iter().zip(names).collect();
            state.layouts.active = 1;
            state.layouts.fcitx = true;
            state.popup = Some(shell::Popup::Menu(shell::Menu::Input));
        }
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
        // A sample query; the rows fill in when the app index arrives.
        Some(show @ ("spotlight" | "spotlight-calc")) => {
            let text = if show == "spotlight" { "te" } else { "12*7" };
            let field = ui::field::TextField { text: text.into(), caret: text.len(), ..Default::default() };
            state.spotlight = Some(shell::spotlight::Spotlight { field, selected: 0 });
        }
        // Launchpad, or Launchpad searching "k".
        Some("launchpad") => state.open_launchpad(),
        Some("launchpad-search") => {
            state.open_launchpad();
            if let Some(l) = state.launchpad.as_mut() {
                l.field = ui::field::TextField { text: "k".into(), caret: 1, ..Default::default() };
            }
        }
        // Opened once the apps started with up.sh have mapped; `mission-control-bar` with the Spaces bar expanded and
        // a second Space.
        Some(name @ ("mission-control" | "mission-control-bar")) => {
            let bar = name == "mission-control-bar";
            let open = move |_: std::time::Instant, _: &mut (), state: &mut State| {
                if bar {
                    state.add_space();
                }
                state.toggle_mission_control();
                if let Some(m) = state.mission.as_mut() {
                    m.expanded = bar;
                }
                smithay::reexports::calloop::timer::TimeoutAction::Drop
            };
            event_loop.handle().insert_source(smithay::reexports::calloop::timer::Timer::from_duration(std::time::Duration::from_secs(5)), open)?;
        }
        // Stays up: no hide timer.
        // The hot-corner ripple, held half way.
        Some("hot-corner") => state.corners.ripple = Some((gestures::Corner::TopRight, std::time::Instant::now() + std::time::Duration::from_secs(3600))),
        Some("osd") => state.osd = Some(shell::Osd { symbol: symbols::Symbol::SpeakerHigh, level: Some(65), text: None }),
        // The dock zoomed as if the pointer were over it, left of centre.
        Some("dock-zoom") => {
            let size = state.space.outputs().next().and_then(|o| state.space.output_geometry(o)).map(|g| g.size).unwrap_or_default();
            state.dock_pointer = Some((f64::from(size.w) / 2.0 - 60.0, f64::from(size.h) - 40.0).into());
        }
        // Sample LauncherEntry counts and progress bars on the default pins.
        Some("dock-badges") => state.sample_badges(),
        // The right-click menu of the first running app's icon (else the first icon), once apps are scanned and mapped.
        Some("dock-menu") => {
            let open = |_: std::time::Instant, _: &mut (), state: &mut State| {
                state.dock_show_menu();
                smithay::reexports::calloop::timer::TimeoutAction::Drop
            };
            event_loop.handle().insert_source(smithay::reexports::calloop::timer::Timer::from_duration(std::time::Duration::from_secs(5)), open)?;
        }
        // Every D8 component in its D7 states (E2.1 done check).
        Some("gallery") => {
            if let Some(winit) = state.backend.as_mut() {
                winit.components = Some(Default::default());
            }
        }
        // The lock screen (E6.4): empty, with a password typed and two missed notifications, or just after a wrong
        // one (frozen mid-shake, with its hint).
        Some(show @ ("lock" | "lock-typing" | "lock-wrong")) => {
            state.lock_screen();
            if let Some(lock) = state.lock.as_mut() {
                match show {
                    "lock-typing" => {
                        lock.field.text = "hunter2".into();
                        lock.field.caret = 7;
                        lock.missed = 2;
                    }
                    "lock-wrong" => lock.wrong = Some(std::time::Instant::now() + std::time::Duration::from_secs(3600)),
                    _ => {}
                }
            }
        }
        // The login screen (E7.2): this machine's users, or three samples with the second picked and a password
        // typed, or a greetd error.
        Some("greeter") => state.start_greeter(shell::greeter::Greeter::load()),
        Some(show @ ("greeter-users" | "greeter-wrong")) => {
            let mut greeter = shell::greeter::Greeter { user: 1, ..shell::greeter::Greeter::sample() };
            if show == "greeter-wrong" {
                greeter.error = Some("Your account has expired; please contact your system administrator".into());
            }
            state.start_greeter(greeter);
            if let Some(lock) = state.lock.as_mut().filter(|_| show == "greeter-users") {
                lock.field.text = "hunter2".into();
                lock.field.caret = 7;
            }
        }
        // The screenshot toolbar (E6.8): in Region mode with a sample selection, with Options open, in Window mode
        // over the first app's window, Super+Shift+4's drag; and a floating thumbnail.
        Some(name @ ("screenshot-toolbar" | "screenshot-region" | "screenshot-window" | "screenshot-options")) => {
            use shell::screenshot::{Mode, Shot};
            let selection = Some(((340.0, 220.0).into(), (820.0, 520.0).into()));
            let shot = match name {
                "screenshot-toolbar" => Shot {
                    mode: Mode::Region,
                    toolbar: true,
                    region: Some(shell::screenshot::region((340.0, 220.0).into(), (820.0, 520.0).into())),
                    ..Default::default()
                },
                "screenshot-options" => Shot { mode: Mode::Screen, toolbar: true, options: true, ..Default::default() },
                "screenshot-window" => Shot { mode: Mode::Window, toolbar: true, ..Default::default() },
                _ => Shot { mode: Mode::Region, drag: selection, ..Default::default() },
            };
            let open = move |_: std::time::Instant, _: &mut (), state: &mut State| {
                let window = (shot.mode == Mode::Window).then(|| state.space.elements().next().and_then(|w| state.frame(w))).flatten();
                state.screenshots.shot = Some(shell::screenshot::Shot { window, ..shot.clone() });
                state.schedule_redraw();
                smithay::reexports::calloop::timer::TimeoutAction::Drop
            };
            event_loop.handle().insert_source(smithay::reexports::calloop::timer::Timer::from_duration(std::time::Duration::from_secs(5)), open)?;
        }
        Some("screenshot-thumbnail") => {
            let (w, h) = (400u32, 250u32);
            let rgba: Vec<u8> = (0..w * h).flat_map(|i| [(i % w * 255 / w) as u8, (i / w * 255 / h) as u8, 200, 255]).collect();
            // No 5 s timer: it stays for the screenshot.
            state.screenshots.thumbnail = Some((shell::screenshot::shrink(&rgba, w, h, 400), "/dev/null".into()));
        }
        Some(other) => tracing::warn!("--show {other}: unknown popup\n{USAGE}"),
        None => {}
    }

    tracing::info!("listening on WAYLAND_DISPLAY={}", state.socket_name.to_string_lossy());
    // The login screen runs no apps.
    if state.greeter().is_some() {
        command.clear();
    } else {
        state.x_display = xwayland::spawn(&state.socket_name);
    }
    if !command.is_empty() {
        state.spawn(&command);
    }

    // Replies queued while handling events go out once per loop turn.
    event_loop.run(None, &mut state, |state| {
        let _ = state.display_handle.flush_clients();
    })?;
    Ok(())
}
