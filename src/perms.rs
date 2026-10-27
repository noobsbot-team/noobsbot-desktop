//! Privileged protocols (E6.11): what a client may see and do beyond its own windows.
//!
//! | Protocol | Offered to |
//! |---|---|
//! | `wlr-screencopy` (screenshots, recording, screen sharing) | trusted clients; any other unsandboxed app once the user allows it in the "would like to record this computer's screen" alert (remembered in `[privacy]`); never sandboxed apps |
//! | `virtual-keyboard`, `virtual-pointer` (external on-screen keyboards, wtype, remote desktops) | unsandboxed clients; trusted ones type and click at once, any other only once the user allows it in the "control this computer's keyboard and mouse" alert (remembered in `[privacy]`), its events going nowhere until then. Never while locked (the login screen too) |
//! | `wlr-layer-shell`, `keyboard-shortcuts-inhibit`, `security-context`, `ext-foreign-toplevel-list`, `wlr-output-management`, `ext-workspace` | unsandboxed clients (third-party bars, launchers, `wlr-randr`, VMs): they already run as the user, and sandboxed apps never see them |
//! | `ext-image-copy-capture` | not implemented, so nobody |
//!
//! Trusted: clients of the `<WAYLAND_DISPLAY>-trusted` socket (what `noobsbot-desktop --trusted <command>` connects
//! its command to), and the programs in `TRUSTED` by their path (root-owned, so an app can't pose as one). Our own
//! screenshots and recordings read the screen in-process and never ask. Sandboxed: connected through a security context (Flatpak).
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
    protocols::unsandboxed,
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

/// Records the client's program and its name (`/proc/<pid>/exe`, `comm`) as it connects, for every later verdict:
/// read later, a process could hand its socket to a child and exec a `TRUSTED` program, or its pid be reused.
// ponytail: still races a process that execs between its connect() and our accept; the header's ceiling covers that.
pub fn record_program(dh: &DisplayHandle, client: &Client) {
    let Ok(pid) = client.get_credentials(dh).map(|c| c.pid) else { return };
    let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) else { return };
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|c| c.trim().to_owned());
    let name = comm.filter(|c| !c.is_empty()).unwrap_or_else(|| exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    if let Some(data) = client.get_data::<ClientState>() {
        let _ = data.program.set((exe, name));
    }
}

/// The client's program and its name, as it connected.
fn program(client: &Client) -> Option<(PathBuf, String)> {
    client.get_data::<ClientState>()?.program.get().cloned()
}

/// Connected through the trusted socket.
pub fn trusted(client: &Client) -> bool {
    client.get_data::<ClientState>().is_some_and(|d| d.trusted)
}

/// `Some(true)`: allowed, `Some(false)`: denied, `None`: ask. `program`: the client's path, if known; `allowed` and
/// `denied`: the `[privacy]` lists of what it wants to do.
pub fn verdict(sandboxed: bool, trusted_client: bool, program: Option<&str>, allowed: &[String], denied: &[String]) -> Option<bool> {
    if sandboxed {
        return Some(false);
    }
    if trusted_client || program.is_some_and(|p| TRUSTED.contains(&p)) {
        return Some(true);
    }
    let program = program?;
    if denied.iter().any(|p| p == program) {
        Some(false)
    } else if allowed.iter().any(|p| p == program) {
        Some(true)
    } else {
        None
    }
}

/// Files `exe` under `[privacy] <key>` (allowed) or `<key>_denied`, out of the other, and saves both to config.toml.
fn remember(key: &str, exe: &str, allow: bool, allowed: &mut Vec<String>, denied: &mut Vec<String>) {
    let (add, remove) = if allow { (&mut *allowed, &mut *denied) } else { (&mut *denied, &mut *allowed) };
    remove.retain(|p| p != exe);
    if !add.iter().any(|p| p == exe) {
        add.push(exe.to_owned());
    }
    let path = config::path();
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let text = config::set_array(&text, "privacy", key, allowed);
    let text = config::set_array(&text, "privacy", &format!("{key}_denied"), denied);
    if let Err(err) = config::save_atomic(&path, &text) {
        tracing::warn!("{}: {err}", path.display());
    }
}

impl State {
    /// Whether `client` may record the screen now: `None` until the user answered.
    pub fn may_record(&self, client: &Client) -> Option<bool> {
        if let Some(answer) = self.capture.verdicts.get(&client.id()) {
            return Some(*answer);
        }
        let exe = program(client).map(|(exe, _)| exe.to_string_lossy().into_owned());
        let privacy = &self.config.privacy;
        verdict(!unsandboxed(client), trusted(client), exe.as_deref(), &privacy.screen_recording, &privacy.screen_recording_denied)
    }

    /// Whether `client`'s virtual keyboard or pointer may act now: never while locked (the login screen too), else
    /// once the user allowed it. Asks while nobody has answered; its events go nowhere meanwhile.
    pub fn may_control(&mut self, client: &Client) -> bool {
        let id = client.id();
        if !self.protocols.control.contains_key(&id) {
            let (exe, app) = program(client).map_or((String::new(), "An app".into()), |(e, n)| (e.to_string_lossy().into_owned(), n));
            let privacy = &self.config.privacy;
            let known = (!exe.is_empty()).then_some(exe.as_str());
            match verdict(!unsandboxed(client), trusted(client), known, &privacy.input_control, &privacy.input_control_denied) {
                Some(answer) => {
                    self.protocols.control.insert(id.clone(), answer);
                }
                None if self.alert.is_none() => self.open_ask(Kind::ControlInput { app, exe, client: id.clone() }),
                None => {}
            }
        }
        !self.locked() && self.protocols.control.get(&id) == Some(&true)
    }

    /// The user's answer about `client`'s virtual input: remembered for this connection and, by path, in `[privacy]`.
    pub fn control_answer(&mut self, client: ClientId, exe: String, allow: bool) {
        let dh = &self.display_handle;
        self.protocols.control.retain(|id, _| dh.backend_handle().get_client_data(id.clone()).is_ok());
        self.protocols.control.insert(client, allow);
        if !exe.is_empty() {
            let privacy = &mut self.config.privacy;
            remember("input_control", &exe, allow, &mut privacy.input_control, &mut privacy.input_control_denied);
        }
    }

    /// Asks about the first client waiting for frames that hasn't been answered, once no other alert is up.
    pub fn capture_ask(&mut self) {
        if self.alert.is_some() {
            return;
        }
        let Some(handle) = self.capture.waiting_clients().into_iter().find(|c| !self.capture.verdicts.contains_key(&c.id())) else { return };
        let client = handle.id();
        let (exe, app) = program(&handle).map_or((String::new(), "An app".into()), |(e, n)| (e.to_string_lossy().into_owned(), n));
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
            remember("screen_recording", &exe, allow, &mut privacy.screen_recording, &mut privacy.screen_recording_denied);
        }
        self.schedule_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn who_may_record() {
        let (allowed, denied) = (["/usr/bin/grim".to_owned()], ["/usr/bin/evil".to_owned()]);
        let verdict = |trusted, program| verdict(false, trusted, program, &allowed, &denied);
        assert_eq!(verdict(true, Some("/usr/bin/evil")), Some(true), "the trusted socket wins");
        assert_eq!(verdict(false, Some("/usr/lib/xdg-desktop-portal-wlr")), Some(true));
        assert_eq!(verdict(false, Some("/usr/libexec/xdg-desktop-portal-wlr")), Some(true), "Debian's path");
        assert_eq!(verdict(false, Some("/usr/bin/grim")), Some(true));
        assert_eq!(verdict(false, Some("/usr/bin/evil")), Some(false));
        assert_eq!(verdict(false, Some("/usr/bin/wf-recorder")), None, "an outside recorder asks");
        assert_eq!(verdict(false, None), None);
        assert_eq!(trusted_socket(OsStr::new("wayland-1")), "wayland-1-trusted");
    }

    #[test]
    fn who_may_control_input() {
        let privacy = config::Privacy {
            input_control: vec!["/usr/bin/wtype".into()],
            input_control_denied: vec!["/usr/bin/evil".into()],
            screen_recording: vec!["/usr/bin/grim".into()],
            ..Default::default()
        };
        let verdict = |sandboxed, trusted, program| verdict(sandboxed, trusted, program, &privacy.input_control, &privacy.input_control_denied);
        assert_eq!(verdict(false, true, Some("/usr/bin/evil")), Some(true), "trusted: the socket wins");
        assert_eq!(verdict(false, false, Some("/usr/bin/wtype")), Some(true), "allowed");
        assert_eq!(verdict(false, false, Some("/usr/bin/evil")), Some(false), "denied");
        assert_eq!(verdict(false, false, Some("/usr/bin/ydotool")), None, "unknown: asks");
        assert_eq!(verdict(false, false, None), None, "no path: asks");
        assert_eq!(verdict(true, false, Some("/usr/bin/wtype")), Some(false), "sandboxed: never, even if listed");
        assert_eq!(verdict(true, true, None), Some(false));
        assert_eq!(verdict(false, false, Some("/usr/bin/grim")), None, "recording's answer isn't input's");
    }

    #[test]
    fn answers_are_written_to_privacy() {
        let text = config::set_array(config::DEFAULT_TOML, "privacy", "screen_recording", &["/usr/bin/grim".into()]);
        let text = config::set_array(&text, "privacy", "input_control_denied", &["/usr/bin/ydotool".into()]);
        let parsed = config::parse(&text).unwrap();
        assert_eq!(parsed.privacy.screen_recording, ["/usr/bin/grim"]);
        assert!(parsed.privacy.screen_recording_denied.is_empty());
        assert_eq!(parsed.privacy.input_control_denied, ["/usr/bin/ydotool"]);
        assert!(parsed.privacy.input_control.is_empty());
    }

    #[test]
    fn crafted_paths_keep_the_file_valid() {
        let paths = vec!["/tmp/a\nb\tc\x7f\"d\\e\u{1}\u{8}\u{c}\ré".to_owned(), "/usr/bin/ydotool".to_owned()];
        let text = config::set_array(config::DEFAULT_TOML, "privacy", "input_control", &paths);
        assert_eq!(config::parse(&text).unwrap().privacy.input_control, paths);
    }
}
