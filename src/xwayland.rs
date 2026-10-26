//! X11 apps (E6.9, C10): xwayland-satellite on a free X display, restarted if it dies (E1.11). It is a normal
//! Wayland client, so X11 windows get our title bars; programs we start get its `DISPLAY`.

use std::{
    ffi::OsStr,
    path::Path,
    process::{Child, Command},
    time::{Duration, Instant},
};

/// A run shorter than this counts as a crash loop.
const QUICK: Duration = Duration::from_secs(10);
/// Quick deaths in a row before giving up.
const TRIES: u32 = 5;

/// Starts xwayland-satellite on the first free display, for the compositor on `socket`; its `DISPLAY` (`:1`), or
/// `None` when it isn't installed. A thread waits on it (no polling) and starts it again when it dies.
/// `cursor_size` goes to it as `XCURSOR_SIZE`.
pub fn spawn(socket: &OsStr, cursor_size: u32) -> Option<String> {
    let name = format!(":{}", free_display(|n| Path::new(&format!("/tmp/.X11-unix/X{n}")).exists() || Path::new(&format!("/tmp/.X{n}-lock")).exists()));
    let start = {
        let (name, socket) = (name.clone(), socket.to_owned());
        move || Command::new("xwayland-satellite").arg(&name).env("WAYLAND_DISPLAY", &socket).env("XCURSOR_SIZE", cursor_size.to_string()).spawn()
    };
    let child = start().inspect_err(|err| tracing::info!("X11 apps off: xwayland-satellite: {err}")).ok()?;
    let spawned = std::thread::Builder::new().name("xwayland".into()).spawn(move || supervise(child, start));
    if let Err(err) = spawned {
        tracing::warn!("xwayland-satellite won't be restarted: {err}");
    }
    tracing::info!("X11 apps on DISPLAY={name}");
    Some(name)
}

fn supervise(mut child: Child, start: impl Fn() -> std::io::Result<Child>) {
    let mut quick = 0;
    loop {
        let started = Instant::now();
        let status = child.wait();
        quick = if started.elapsed() < QUICK { quick + 1 } else { 0 };
        if quick >= TRIES {
            return tracing::error!("xwayland-satellite keeps dying ({status:?}); X11 apps are off until the next login");
        }
        tracing::warn!("xwayland-satellite exited ({status:?}), restarting");
        // A crash loop restarts at most once a second.
        if quick > 0 {
            std::thread::sleep(Duration::from_secs(1));
        }
        match start() {
            Ok(next) => child = next,
            Err(err) => return tracing::error!("restarting xwayland-satellite: {err}"),
        }
    }
}

/// The lowest X display number that `taken` says is free. `:0` first: on a real session no X server runs.
fn free_display(taken: impl Fn(u32) -> bool) -> u32 {
    (0..).find(|&n| !taken(n)).expect("some display number is free")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_taken_displays() {
        assert_eq!(free_display(|_| false), 0);
        // Nested in an X desktop: :0 is the host's, :1 another nested session's.
        assert_eq!(free_display(|n| n < 2), 2);
        assert_eq!(free_display(|n| n == 0 || n == 2), 1);
    }
}
