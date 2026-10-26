//! Privileged protocols (E6.11): what a client may see and do beyond its own windows.
//!
//! | Protocol | Offered to |
//! |---|---|
//! | `wlr-screencopy` (screenshots, recording, screen sharing) | trusted clients; any other unsandboxed app once the user allows it in the "would like to record this computer's screen" alert (remembered in `[privacy]`); never sandboxed apps |
//! | `wlr-layer-shell`, `keyboard-shortcuts-inhibit`, `security-context`, `ext-foreign-toplevel-list` | unsandboxed clients (third-party bars, launchers, VMs) |
//! | `virtual-keyboard` (external on-screen keyboards, remote input) | trusted-socket clients |
//! | `virtual-pointer`, `wlr-output-management`, `ext-workspace`, `ext-image-copy-capture` | not implemented, so nobody |
//!
//! Trusted: clients of the `<WAYLAND_DISPLAY>-trusted` socket (what `noobsbot-desktop --trusted <command>` connects
//! its command to, and our own `wf-recorder`), and the programs in `TRUSTED` by their path (root-owned, so an app
//! can't pose as one). Sandboxed: connected through a security context (Flatpak).
// ponytail: any program of this user can connect to the trusted socket on purpose; Wayland can't tell apart two
// processes of one user. The real wall is the sandbox (security-context); this catches the casual screen grabber.

use std::{
    ffi::{OsStr, OsString},
    os::unix::process::CommandExt,
    path::PathBuf,
};

use smithay::reexports::wayland_server::{Client, DisplayHandle, backend::ClientId};

use crate::{
    config,
    shell::alert::Kind,
    state::{ClientState, State},
};

/// The screen-sharing portal, the X11 bridge and the Settings app (M7) never ask. Arch paths first, then Debian's
/// (its portal lives in `/usr/libexec`, and install.sh puts xwayland-satellite in `/usr/local/bin`).
const TRUSTED: &[&str] = &[
    "/usr/lib/xdg-desktop-portal-wlr",
    "/usr/bin/xwayland-satellite",
    "/usr/bin/noobsbot-settings",
    "/usr/libexec/xdg-desktop-portal-wlr",
    "/usr/local/bin/xwayland-satellite",
];

/// The trusted socket next to `socket` (`wayland-1` → `wayland-1-trusted`).
pub fn trusted_socket(socket: &OsStr) -> OsString {
    let mut name = socket.to_os_string();
    name.push("-trusted");
    name
}

/// `noobsbot-desktop --trusted <command> [args…]`: runs the command on the trusted socket. Returns only on failure.
pub fn exec_trusted(mut args: impl Iterator<Item = String>) -> i32 {
    let Some(program) = args.next() else {
        eprintln!("usage: noobsbot-desktop --trusted <command> [args...]");
        return 2;
    };
    let Some(display) = std::env::var_os("WAYLAND_DISPLAY") else {
        eprintln!("--trusted: WAYLAND_DISPLAY is not set; run it inside Noobsbot Desktop");
        return 1;
    };
    let err = std::process::Command::new(&program).args(args).env("WAYLAND_DISPLAY", trusted_socket(&display)).exec();
    eprintln!("{program}: {err}");
    127
}

/// The client's program and its name (`/proc/<pid>/exe`, `comm`).
fn program(dh: &DisplayHandle, client: &Client) -> Option<(PathBuf, String)> {
    let pid = client.get_credentials(dh).ok()?.pid;
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|c| c.trim().to_owned());
    let name = comm.filter(|c| !c.is_empty()).unwrap_or_else(|| exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    Some((exe, name))
}

/// Connected through the trusted socket. ponytail: `TRUSTED` paths aren't checked here (a global filter has no
/// DisplayHandle); start an external keyboard with `noobsbot-desktop --trusted`.
pub fn trusted(client: &Client) -> bool {
    client.get_data::<ClientState>().is_some_and(|d| d.trusted)
}

/// `Some(true)`: allowed, `Some(false)`: denied, `None`: ask. `program`: the client's path, if known.
pub fn verdict(trusted_client: bool, program: Option<&str>, privacy: &config::Privacy) -> Option<bool> {
    if trusted_client || program.is_some_and(|p| TRUSTED.contains(&p)) {
        return Some(true);
    }
    let program = program?;
    if privacy.screen_recording_denied.iter().any(|p| p == program) {
        Some(false)
    } else if privacy.screen_recording.iter().any(|p| p == program) {
        Some(true)
    } else {
        None
    }
}

impl State {
    /// Whether `client` may record the screen now: `None` until the user answered.
    pub fn may_record(&self, client: &Client) -> Option<bool> {
        if let Some(answer) = self.capture.verdicts.get(&client.id()) {
            return Some(*answer);
        }
        let trusted = client.get_data::<ClientState>().is_some_and(|d| d.trusted);
        let exe = program(&self.display_handle, client).map(|(exe, _)| exe.to_string_lossy().into_owned());
        verdict(trusted, exe.as_deref(), &self.config.privacy)
    }

    /// Asks about the first client waiting for frames that hasn't been answered, once no other alert is up.
    pub fn capture_ask(&mut self) {
        if self.alert.is_some() {
            return;
        }
        let Some(handle) = self.capture.waiting_clients().into_iter().find(|c| !self.capture.verdicts.contains_key(&c.id())) else { return };
        let client = handle.id();
        let (exe, app) = program(&self.display_handle, &handle).map_or((String::new(), "An app".into()), |(e, n)| (e.to_string_lossy().into_owned(), n));
        self.open_ask(Kind::ScreenRecording { app, exe, client });
    }

    /// The user's answer for `client`: remembered for this connection and, by program path, in `[privacy]`.
    pub fn recording_answer(&mut self, client: ClientId, exe: String, allow: bool) {
        self.capture.prune(&self.display_handle);
        self.capture.verdicts.insert(client.clone(), allow);
        if !allow {
            self.capture.fail_client(&client);
        }
        if !exe.is_empty() {
            let privacy = &mut self.config.privacy;
            let (add, remove) = if allow {
                (&mut privacy.screen_recording, &mut privacy.screen_recording_denied)
            } else {
                (&mut privacy.screen_recording_denied, &mut privacy.screen_recording)
            };
            remove.retain(|p| *p != exe);
            if !add.contains(&exe) {
                add.push(exe);
            }
            let path = config::path();
            let path = std::fs::canonicalize(&path).unwrap_or(path);
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let text = config::set_array(&text, "privacy", "screen_recording", &self.config.privacy.screen_recording);
            let text = config::set_array(&text, "privacy", "screen_recording_denied", &self.config.privacy.screen_recording_denied);
            if let Err(err) = config::save_atomic(&path, &text) {
                tracing::warn!("{}: {err}", path.display());
            }
        }
        self.schedule_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn who_may_record() {
        let privacy = config::Privacy { screen_recording: vec!["/usr/bin/grim".into()], screen_recording_denied: vec!["/usr/bin/evil".into()] };
        assert_eq!(verdict(true, Some("/usr/bin/evil"), &privacy), Some(true), "the trusted socket wins");
        assert_eq!(verdict(false, Some("/usr/lib/xdg-desktop-portal-wlr"), &privacy), Some(true));
        assert_eq!(verdict(false, Some("/usr/libexec/xdg-desktop-portal-wlr"), &privacy), Some(true), "Debian's path");
        assert_eq!(verdict(false, Some("/usr/bin/grim"), &privacy), Some(true));
        assert_eq!(verdict(false, Some("/usr/bin/evil"), &privacy), Some(false));
        assert_eq!(verdict(false, Some("/usr/bin/wf-recorder"), &privacy), None);
        assert_eq!(verdict(false, None, &privacy), None);
        assert_eq!(trusted_socket(OsStr::new("wayland-1")), "wayland-1-trusted");
    }

    #[test]
    fn answers_are_written_to_privacy() {
        let text = config::set_array(config::DEFAULT_TOML, "privacy", "screen_recording", &["/usr/bin/grim".into()]);
        let parsed = config::parse(&text).unwrap();
        assert_eq!(parsed.privacy.screen_recording, ["/usr/bin/grim"]);
        assert!(parsed.privacy.screen_recording_denied.is_empty());
    }
}
