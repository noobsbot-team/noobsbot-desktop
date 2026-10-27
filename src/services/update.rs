//! Software Update (E6.13): once at start and every 24 h, ask GitHub for the newest release on the chosen channel
//! and compare it with the running version. No account, no identifier: the request carries only the version in its
//! User-Agent. Fails silently when offline. Off in nested mode and with `check_updates = false` (applied on reload).
//!
//! A newer release on an install.sh install (versions under `/usr/lib/noobsbot-desktop/versions/`, release key
//! installed) is downloaded in the background to `~/.cache/noobsbot-desktop/updates/<v>/` and verified (minisign
//! signature of `SHA256SUMS`, then the tarball's SHA-256); a bad download is deleted. The notification then offers
//! "Restart to Update", which runs the privileged helper through pkexec and logs out, so the next login starts the
//! new version. Anything else (an AUR package, a hand-built binary, no key yet) only notifies.
//!
//! noobsbot-terminal's releases are checked too, for its own row in Settings and the update count; its own installer
//! or package updates it, so it is only shown (with its release page), never downloaded.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc,
    time::Duration,
};

use smithay::reexports::calloop::channel::Sender;

use semver::Version;

use super::verify;

pub const REPO: &str = "https://github.com/noobsbot-team/noobsbot-desktop";
const TERMINAL: &str = "https://github.com/noobsbot-team/noobsbot-terminal";
const EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// The privileged helper, at the path the polkit action `org.noobsbot.desktop.update` names.
pub const HELPER: &str = "/usr/lib/noobsbot-desktop/noobsbot-desktop-update";
const VERSIONS: &str = "/usr/lib/noobsbot-desktop/versions/";
/// This machine's CPU, as release tarballs name it (`x86_64`, `aarch64`).
const ARCH: &str = std::env::consts::ARCH;

/// Which releases count (`[update] channel`).
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Stable,
    /// Pre-releases (`1.4.0-beta.2`) too.
    Beta,
}

/// A release newer than the running version.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: Version,
    /// The git tag (`v1.3.0`).
    pub tag: String,
    /// The release page (notes, downloads).
    pub url: String,
    pub download: Download,
    /// The download for this machine's CPU, in bytes.
    pub size: Option<u64>,
    /// `published_at` (RFC 3339).
    pub published: Option<String>,
}

/// How far the background download got.
#[derive(Clone, Debug, PartialEq)]
pub enum Download {
    /// Not downloaded: not an install.sh install, no release key, or offline.
    None,
    /// Downloaded and verified: the dir to hand the helper.
    Ready(PathBuf),
    /// Failed verification and deleted.
    Refused,
}

/// What the update thread is asked.
#[derive(Debug, Clone, Copy)]
pub enum Ask {
    /// `[update] channel` changed (a check at once), or `None`: `check_updates` went off (idle).
    Every(Option<Channel>),
    /// Settings' Check Now: one check, whatever `check_updates` says.
    Now(Channel),
}

/// A finished check: the newer release, if any, noobsbot-terminal's row, and whether Check Now asked for it.
pub struct Checked {
    pub release: Option<Release>,
    pub terminal: Option<Terminal>,
    pub asked: bool,
}

/// Its own thread: the check and download block on the network, and between checks it only sleeps (no wakeups).
/// It idles until the first `Ask`. Offline checks report nothing.
pub fn spawn(found: Sender<Checked>) -> mpsc::Sender<Ask> {
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::Builder::new().name("update".into()).spawn(move || {
        let mut every = None;
        loop {
            let next = match every {
                Some(_) => rx.recv_timeout(EVERY),
                None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
            };
            let (channel, asked) = match next {
                Ok(Ask::Every(channel)) => {
                    every = channel;
                    (channel, false)
                }
                Ok(Ask::Now(channel)) => (Some(channel), true),
                Err(mpsc::RecvTimeoutError::Timeout) => (every, false),
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            };
            let Some(channel) = channel else { continue };
            let Some(mut release) = latest(REPO, &running(), channel) else { continue };
            if let Some(release) = &mut release {
                release.download = download(release);
                tracing::info!("Noobsbot Desktop {} is available ({:?}): {}", release.version, release.download, release.url);
            }
            if found.send(Checked { release, terminal: terminal(channel), asked }).is_err() {
                return;
            }
        }
    });
    if let Err(err) = thread {
        tracing::warn!("update check off: {err}");
    }
    tx
}

/// This build's version.
fn running() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml has a SemVer version")
}

/// The newest release of `repo` (its web URL) on `channel` if it is newer than `running`: `Some(None)` when up to
/// date, `None` offline.
fn latest(repo: &str, running: &Version, channel: Channel) -> Option<Option<Release>> {
    let api = repo.replace("https://github.com/", "https://api.github.com/repos/");
    let url = match channel {
        Channel::Stable => format!("{api}/releases/latest"),
        Channel::Beta => format!("{api}/releases?per_page=20"),
    };
    fetch(&url).map(|json| newer(&json, repo, running, channel))
}

/// noobsbot-terminal's Software Update row: the installed version and a newer release on the same channel.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Terminal {
    pub installed: String,
    pub available: Option<String>,
    pub url: Option<String>,
    pub size: Option<u64>,
    pub published: Option<String>,
}

/// noobsbot-terminal's row: `None` when it isn't installed (or GitHub can't be reached).
fn terminal(channel: Channel) -> Option<Terminal> {
    // `noobsbot 0.5.4`.
    let out = Command::new("noobsbot").arg("--version").output().ok()?;
    let installed = Version::parse(String::from_utf8_lossy(&out.stdout).split_whitespace().last()?.trim_start_matches('v')).ok()?;
    let newer = latest(TERMINAL, &installed, channel)?;
    Some(Terminal {
        installed: installed.to_string(),
        available: newer.as_ref().map(|r| r.version.to_string()),
        url: newer.as_ref().map(|r| r.url.clone()),
        size: newer.as_ref().and_then(|r| r.size),
        published: newer.and_then(|r| r.published),
    })
}

/// `noobsbot-desktop --update`: the same check, download and helper as "Restart to Update", from a terminal.
/// pkexec asks for the admin password; the new version starts at the next login.
pub fn cli(channel: Channel) -> i32 {
    println!("Checking for updates…");
    let Some(release) = latest(REPO, &running(), channel).flatten() else {
        println!("Noobsbot Desktop {} is up to date (or GitHub can't be reached).", env!("CARGO_PKG_VERSION"));
        return 0;
    };
    println!("Downloading Noobsbot Desktop {}…", release.version);
    match download(&release) {
        Download::Ready(dir) => match Command::new("pkexec").arg(HELPER).arg(&dir).status() {
            Ok(status) if status.success() => {
                println!("Log out and back in to start {}.", release.version);
                0
            }
            Ok(_) => 1,
            Err(err) => {
                eprintln!("pkexec: {err}");
                1
            }
        },
        Download::Refused => {
            eprintln!("The download of {} failed its signature check and was deleted.", release.version);
            1
        }
        Download::None => {
            eprintln!(
                "{} is available ({}), but this install can't update itself (not installed by install.sh, or offline); use ./install.sh --update or your package manager.",
                release.version, release.url
            );
            1
        }
    }
}

/// The update notification's id, apart from low battery's and below the disk notices' range (shell/disks.rs), far above
/// the server's counter.
pub const NOTICE_ID: u32 = 0xFFFB_0000;

/// "Noobsbot Desktop 1.3 is available".
pub fn notice(release: &Release) -> super::notifications::Notification {
    let icon = "system-software-update";
    let v = &release.version;
    let mut actions = vec![("default".into(), String::new()), ("details".into(), "Details".into())];
    let body = match release.download {
        Download::None => format!("Version {v} is ready to install."),
        Download::Ready(_) => {
            actions.push(("install".into(), "Restart to Update".into()));
            format!("Version {v} is downloaded and verified.")
        }
        Download::Refused => format!("The download of {v} failed its signature check and was deleted."),
    };
    super::notifications::Notification {
        id: NOTICE_ID,
        app: "Software Update".into(),
        icon_name: icon.into(),
        icon: super::notifications::load_icon(icon).map(std::sync::Arc::new),
        summary: format!("Noobsbot Desktop {}.{} is available", v.major, v.minor),
        body,
        actions,
        urgency: 1,
        expire: -1,
        time: chrono::Local::now().timestamp(),
        ..Default::default()
    }
}

/// Runs `curl` with `args`: its output, or `None` when offline or GitHub refuses.
// ponytail: curl (on every install) instead of an HTTP + TLS crate; switch to ureq if curl ever goes missing.
fn curl(args: &[&str]) -> Option<Vec<u8>> {
    let agent = concat!("noobsbot-desktop/", env!("CARGO_PKG_VERSION"));
    let output = Command::new("curl")
        .args(["-fsSL", "--proto", "=https", "-A", agent])
        .args(args)
        .output()
        .inspect_err(|err| tracing::debug!("update: curl: {err}"))
        .ok()?;
    if !output.status.success() {
        tracing::debug!("update: curl {args:?} exited with {}", output.status);
        return None;
    }
    Some(output.stdout)
}

/// The release JSON, or `None` when offline.
fn fetch(url: &str) -> Option<String> {
    String::from_utf8(curl(&["--max-time", "20", "-H", "Accept: application/vnd.github+json", url])?).ok()
}

/// A release in GitHub's JSON, as far as the checker reads it.
#[derive(serde::Deserialize)]
struct Json {
    tag_name: String,
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(serde::Deserialize)]
struct Asset {
    name: String,
    size: u64,
}

/// The newest release of `repo` (its web URL) in a GitHub `releases/latest` (one) or `releases` (a list) response,
/// if it is newer than `running`. Stable ignores pre-releases; GitHub never lists drafts to anonymous requests.
pub fn newer(json: &str, repo: &str, running: &Version, channel: Channel) -> Option<Release> {
    let releases: Vec<Json> = serde_json::from_str(json).or_else(|_| serde_json::from_str(json).map(|one| vec![one])).ok()?;
    let (version, json) = releases
        .into_iter()
        .filter_map(|r| Some((Version::parse(r.tag_name.strip_prefix('v').unwrap_or(&r.tag_name)).ok()?, r)))
        .filter(|(v, _)| channel == Channel::Beta || v.pre.is_empty())
        .max_by(|a, b| a.0.cmp(&b.0))?;
    // The tarball for this CPU, not its checksum (smaller) nor another CPU's.
    let size = json.assets.iter().filter(|a| a.name.contains(ARCH)).map(|a| a.size).max();
    let url = format!("{repo}/releases/tag/{}", json.tag_name);
    (version > *running).then_some(Release { version, tag: json.tag_name, url, download: Download::None, size, published: json.published_at })
}

/// Downloads and verifies `release` (or re-verifies an earlier download).
fn download(release: &Release) -> Download {
    let key = fs::read_to_string(verify::PUBKEY).unwrap_or_default();
    let ours = fs::canonicalize("/usr/bin/noobsbot-desktop").is_ok_and(|bin| bin.starts_with(VERSIONS));
    let cache = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(".cache")));
    let Some(cache) = cache.filter(|_| !key.trim().is_empty() && ours && Path::new(HELPER).exists()) else { return Download::None };
    let updates = cache.join("noobsbot-desktop/updates");
    let dir = updates.join(release.version.to_string());
    // Earlier versions' downloads are done with (installed or superseded).
    for old in fs::read_dir(&updates).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| *p != dir) {
        let _ = fs::remove_dir_all(old);
    }
    if check(&dir, &key, &release.version, ARCH).is_ok() {
        return Download::Ready(dir);
    }
    let base = format!("{REPO}/releases/download/{}", release.tag);
    let get = |name: &str| {
        let path = dir.join(name);
        curl(&["--max-time", "900", "-o", path.to_str()?, &format!("{base}/{name}")]).map(|_| ())
    };
    let tarball = format!("noobsbot-desktop-{}-{ARCH}.tar.zst", release.version);
    if fs::create_dir_all(&dir).is_err() || [tarball.as_str(), "SHA256SUMS", "SHA256SUMS.minisig"].into_iter().any(|f| get(f).is_none()) {
        return Download::None;
    }
    match check(&dir, &key, &release.version, ARCH) {
        Ok(()) => Download::Ready(dir),
        Err(err) => {
            tracing::warn!("update {}: {err}; deleted the download", release.version);
            let _ = fs::remove_dir_all(&dir);
            Download::Refused
        }
    }
}

/// The download in `dir` is signed by `key`, is `version`, and its `arch` tarball matches the signed SHA-256.
fn check(dir: &Path, key: &str, version: &Version, arch: &str) -> Result<(), String> {
    let read = |name: &str| fs::read(dir.join(name)).map_err(|e| format!("{name}: {e}"));
    let signed = verify::signed(key, &read("SHA256SUMS")?, &String::from_utf8_lossy(&read("SHA256SUMS.minisig")?), arch)?;
    if signed.version != *version {
        // A correctly signed but different (older) release served under this tag: a replay.
        return Err(format!("signed for {}, not {version}", signed.version));
    }
    let file = fs::File::open(dir.join(&signed.tarball)).map_err(|e| format!("{}: {e}", signed.tarball))?;
    let sha256 = verify::sha256(file, |_| Ok(())).map_err(|e| e.to_string())?;
    (sha256 == signed.sha256).then_some(()).ok_or_else(|| format!("{} doesn't match its signed SHA-256", signed.tarball))
}

/// `$XDG_STATE_HOME/noobsbot-desktop/update.json`, which Settings ▸ General ▸ Software Update shows.
fn status_path() -> PathBuf {
    crate::config::xdg_dir("XDG_STATE_HOME", ".local/state").join("noobsbot-desktop/update.json")
}

/// What Settings shows; `checked` is a Unix time, 0 never.
#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Status {
    pub checked: i64,
    /// A newer version than the running one.
    pub available: Option<String>,
    /// Its release page (notes).
    pub url: Option<String>,
    /// Its download's size in bytes and release date (RFC 3339).
    pub size: Option<u64>,
    pub published: Option<String>,
    /// Downloaded and verified: Update Now installs it.
    pub ready: bool,
    /// The version "Revert to" goes back to (`revert_to`).
    pub revert: Option<String>,
    /// noobsbot-terminal's row, when it is installed.
    pub terminal: Option<Terminal>,
}

/// How long after an update Settings offers to revert it.
const REVERT_FOR: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// The version to revert to: the newest installed one older than `current`, while the update to `current` is younger
/// than 30 days (`age`). Rollback (the session wrapper) goes to the last version that booted instead; after a minute
/// that is the current one, so it can't name the previous.
pub fn revert_to(current: &Version, installed: impl IntoIterator<Item = Version>, age: Duration) -> Option<Version> {
    (age < REVERT_FOR).then(|| installed.into_iter().filter(|v| v < current).max()).flatten()
}

/// `revert_to` on this machine: `/usr/bin/noobsbot-desktop` → `versions/<current>/`, its link's age, the other dirs.
fn revert_here() -> Option<Version> {
    let bin = Path::new("/usr/bin/noobsbot-desktop");
    let link = fs::read_link(bin).ok()?;
    let current = Version::parse(link.strip_prefix(VERSIONS).ok()?.parent()?.to_str()?).ok()?;
    let age = fs::symlink_metadata(bin).ok()?.modified().ok()?.elapsed().unwrap_or_default();
    let installed = fs::read_dir(VERSIONS).ok()?.flatten().filter(|e| e.path().join("noobsbot-desktop").is_file());
    revert_to(&current, installed.filter_map(|e| Version::parse(e.file_name().to_str()?).ok()), age)
}

impl crate::state::State {
    /// Matches the update thread to `check_updates` and `[update] channel`: starts it the first time it is on, tells
    /// it the new channel (a check at once) or to idle. Turning it off also withdraws a pending notice. Off nested
    /// (a dev session: no network).
    pub fn update_checks(&mut self) {
        let on = self.config.check_updates.then_some(self.config.update.channel);
        if on.is_some() || self.update_channel.is_some() {
            self.ask_update(Ask::Every(on));
        }
        if on.is_none() {
            self.update_terminal = None;
        }
        if on.is_none() && self.update.take().is_some() {
            self.close_notification(NOTICE_ID, super::notifications::Reason::Dismissed);
        }
        self.update_changed(None);
    }

    /// Settings' Check Now: one check on the chosen channel, also with `check_updates` off.
    pub fn check_update_now(&mut self) {
        self.ask_update(Ask::Now(self.config.update.channel));
    }

    /// Sends `ask` to the update thread, starting it first.
    fn ask_update(&mut self, ask: Ask) {
        if self.nested {
            return;
        }
        if self.update_channel.is_none() {
            let (found_tx, found_rx) = smithay::reexports::calloop::channel::channel();
            let inserted = self.loop_handle.insert_source(found_rx, |event, _, state| {
                let smithay::reexports::calloop::channel::Event::Msg(Checked { release, terminal, asked }) = event else { return };
                if asked || state.config.check_updates {
                    state.update_terminal = terminal;
                }
                // A check already running when the setting went off still reports.
                if let Some(release) = release
                    && (asked || state.config.check_updates)
                {
                    state.notify(notice(&release));
                    state.update = Some(release);
                }
                state.update_changed(Some(chrono::Local::now().timestamp()));
                state.schedule_redraw();
            });
            match inserted {
                Ok(_) => self.update_channel = Some(spawn(found_tx)),
                Err(err) => return tracing::warn!("update check off: {err}"),
            }
        }
        if let Some(tx) = &self.update_channel {
            let _ = tx.send(ask);
        }
    }

    /// Pending updates: the desktop's and noobsbot-terminal's (the dock's and the logo menu's count).
    pub fn update_count(&self) -> usize {
        usize::from(self.update.is_some()) + usize::from(self.update_terminal.as_ref().is_some_and(|t| t.available.is_some()))
    }

    /// A red count on System Settings in the dock while an update is pending, and the status file for Settings
    /// (`checked`: a check just finished; `None` keeps the last time).
    pub fn update_changed(&mut self, checked: Option<i64>) {
        let count = Some(self.update_count() as i64).filter(|&n| n > 0);
        self.dock_fx.entry("noobsbot-desktop-settings".into(), super::launcher::Entry { count, ..Default::default() });
        if self.nested {
            return;
        }
        let path = status_path();
        let checked =
            checked.unwrap_or_else(|| fs::read_to_string(&path).ok().and_then(|json| serde_json::from_str::<Status>(&json).ok()).map_or(0, |s| s.checked));
        let status = Status {
            checked,
            available: self.update.as_ref().map(|r| r.version.to_string()),
            url: self.update.as_ref().map(|r| r.url.clone()),
            size: self.update.as_ref().and_then(|r| r.size),
            published: self.update.as_ref().and_then(|r| r.published.clone()),
            ready: self.update_ready(),
            revert: revert_here().map(|v| v.to_string()),
            terminal: self.update_terminal.clone(),
        };
        let write = || {
            fs::create_dir_all(path.parent()?).ok()?;
            let tmp = path.with_extension("json.new");
            fs::write(&tmp, serde_json::to_vec(&status).ok()?).and_then(|_| fs::rename(&tmp, &path)).ok()
        };
        if write().is_none() {
            tracing::debug!("update: couldn't write {}", path.display());
        }
    }

    /// A verified download is waiting for "Restart to Update" (logo menu, notification).
    pub fn update_ready(&self) -> bool {
        matches!(self.update, Some(Release { download: Download::Ready(_), .. }))
    }

    /// "Restart to Update": the helper installs the verified download (pkexec asks for the admin password through the
    /// session's polkit agent), then the log-out alert, so the next login starts the new version.
    pub fn install_update(&mut self) {
        // Also keeps `--show update` (a pretend download) from ever reaching pkexec on a dev machine.
        let Some(release) = self.update.as_mut().filter(|_| Path::new(HELPER).exists()) else { return };
        // Taken while the helper runs, so a second click doesn't ask twice.
        let Download::Ready(dir) = std::mem::replace(&mut release.download, Download::None) else { return };
        let back = dir.clone();
        self.run_helper(vec![dir.into_os_string()], move |state, ok| {
            if ok {
                state.update = None;
                state.close_notification(NOTICE_ID, super::notifications::Reason::Dismissed);
            } else if let Some(release) = state.update.as_mut() {
                // Cancelled or refused (the helper logs why): offered again.
                release.download = Download::Ready(back.clone());
            }
        });
    }

    /// Settings' "Revert to <v>": the helper points the links back at the previous version, then the log-out alert.
    pub fn revert_update(&mut self) {
        let Some(version) = revert_here() else { return };
        self.run_helper(vec!["--revert".into(), version.to_string().into()], |_, _| {});
    }

    /// Runs the helper with `args` through pkexec on a thread; then `done(ok)`, and on success the log-out alert so the
    /// next login starts the version it linked.
    fn run_helper(&mut self, args: Vec<std::ffi::OsString>, mut done: impl FnMut(&mut Self, bool) + 'static) {
        let (tx, finished) = smithay::reexports::calloop::channel::channel();
        let inserted = self.loop_handle.insert_source(finished, move |event, _, state| {
            let smithay::reexports::calloop::channel::Event::Msg(ok) = event else { return };
            done(state, ok);
            if ok {
                state.open_alert(crate::shell::alert::Power::LogOut);
            }
            state.update_changed(None);
            state.schedule_redraw();
        });
        if let Err(err) = inserted {
            return tracing::warn!("update: {err}");
        }
        std::thread::spawn(move || {
            let status = Command::new("pkexec").arg(HELPER).args(&args).status();
            tracing::info!("update: {HELPER}: {status:?}");
            let _ = tx.send(status.is_ok_and(|s| s.success()));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"{"url":"https://api.github.com/repos/noobsbot-team/noobsbot-desktop/releases/1",
        "html_url": "https://github.com/noobsbot-team/noobsbot-desktop/releases/tag/v1.3.0",
        "author": {"html_url": "https://github.com/anil0403"},
        "tag_name": "v1.3.0", "draft": false, "prerelease": false, "published_at": "2026-10-27T09:41:00Z",
        "assets": [{"name": "noobsbot-desktop-1.3.0-x86_64.tar.zst", "size": 12345678},
            {"name": "noobsbot-desktop-1.3.0-aarch64.tar.zst", "size": 99999999}, {"name": "SHA256SUMS", "size": 200}],
        "body": "notes quoting \"tag_name\": \"v9.0.0\""}"#;

    const LIST: &str = r#"[{"tag_name": "v1.4.0-beta.2", "prerelease": true}, {"tag_name": "v1.3.0"},
        {"tag_name": "v1.4.0-beta.1", "prerelease": true}]"#;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn newer_release() {
        let release = newer(JSON, REPO, &v("1.2.4"), Channel::Stable).unwrap();
        assert_eq!((release.version, release.tag.as_str()), (v("1.3.0"), "v1.3.0"));
        assert_eq!(release.url, "https://github.com/noobsbot-team/noobsbot-desktop/releases/tag/v1.3.0");
        assert_eq!(release.download, Download::None);
        assert_eq!(release.published.as_deref(), Some("2026-10-27T09:41:00Z"));
        let size = if ARCH == "aarch64" { 99999999 } else { 12345678 };
        assert_eq!(release.size, (ARCH == "x86_64" || ARCH == "aarch64").then_some(size));
    }

    #[test]
    fn same_or_older_is_none() {
        assert_eq!(newer(JSON, REPO, &v("1.3.0"), Channel::Stable), None);
        assert_eq!(newer(JSON, REPO, &v("1.10.0"), Channel::Stable), None);
    }

    #[test]
    fn beta_before_its_release_updates() {
        // SemVer: 1.3.0-beta.2 < 1.3.0, so a Beta user gets the final release.
        assert!(newer(JSON, REPO, &v("1.3.0-beta.2"), Channel::Stable).is_some());
        assert!(newer(JSON, REPO, &v("1.4.0-beta.1"), Channel::Stable).is_none());
    }

    #[test]
    fn channels() {
        assert_eq!(newer(LIST, REPO, &v("1.2.0"), Channel::Beta).unwrap().version, v("1.4.0-beta.2"));
        assert_eq!(newer(LIST, REPO, &v("1.2.0"), Channel::Stable).unwrap().version, v("1.3.0"));
        assert!(newer(LIST, REPO, &v("1.3.0"), Channel::Stable).is_none());
    }

    #[test]
    fn garbage_is_none() {
        assert_eq!(newer("", REPO, &v("0.1.0"), Channel::Beta), None);
        assert_eq!(newer(r#"{"message":"Not Found"}"#, REPO, &v("0.1.0"), Channel::Stable), None);
        assert_eq!(newer(r#"{"tag_name":"nightly"}"#, REPO, &v("0.1.0"), Channel::Stable), None);
    }

    #[test]
    fn revert_for_30_days_to_the_previous() {
        let day = Duration::from_secs(24 * 60 * 60);
        let installed = || [v("1.2.4"), v("1.3.0"), v("1.1.0"), v("1.4.0-beta.1")];
        assert_eq!(revert_to(&v("1.3.0"), installed(), day), Some(v("1.2.4")));
        assert_eq!(revert_to(&v("1.3.0"), installed(), 29 * day), Some(v("1.2.4")));
        assert_eq!(revert_to(&v("1.3.0"), installed(), 30 * day), None, "too late");
        assert_eq!(revert_to(&v("1.1.0"), installed(), day), None, "nothing older");
        assert_eq!(revert_to(&v("1.4.0"), installed(), day), Some(v("1.4.0-beta.1")));
    }

    #[test]
    fn status_reads_back_and_tolerates_missing_keys() {
        let terminal = Terminal { installed: "0.5.4".into(), available: Some("0.6.0".into()), ..Default::default() };
        let status =
            Status { checked: 5, available: Some("1.3.0".into()), ready: true, revert: Some("1.2.4".into()), terminal: Some(terminal), ..Default::default() };
        assert_eq!(serde_json::from_str::<Status>(&serde_json::to_string(&status).unwrap()).unwrap(), status);
        assert_eq!(serde_json::from_str::<Status>(r#"{"checked": 7}"#).unwrap(), Status { checked: 7, ..Default::default() });
    }

    #[test]
    fn notice_names_major_minor() {
        let release = newer(r#"{"tag_name":"1.3.2"}"#, REPO, &v("1.0.0"), Channel::Stable).unwrap();
        assert_eq!(release.url, "https://github.com/noobsbot-team/noobsbot-desktop/releases/tag/1.3.2");
        let n = notice(&release);
        assert_eq!((n.id, n.summary.as_str(), n.body.as_str()), (NOTICE_ID, "Noobsbot Desktop 1.3 is available", "Version 1.3.2 is ready to install."));
        assert_eq!(n.actions.len(), 2);
        let ready = notice(&Release { download: Download::Ready(PathBuf::new()), ..release });
        assert_eq!(ready.actions.last().unwrap(), &("install".to_string(), "Restart to Update".to_string()));
    }

    #[test]
    fn check_verifies_the_download() {
        let dir = std::env::temp_dir().join(format!("noobsbot-update-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/services/testdata");
        for f in ["SHA256SUMS", "SHA256SUMS.minisig"] {
            fs::copy(data.join(f), dir.join(f)).unwrap();
        }
        let key = fs::read_to_string(data.join("minisign.pub")).unwrap();
        let tarball = dir.join("noobsbot-desktop-1.3.0-x86_64.tar.zst");
        fs::write(&tarball, "pretend tarball\n").unwrap();
        assert_eq!(check(&dir, &key, &v("1.3.0"), "x86_64"), Ok(()));
        assert!(check(&dir, &key, &v("1.3.1"), "x86_64").unwrap_err().starts_with("signed for 1.3.0"));
        fs::write(&tarball, "tampered tarball\n").unwrap();
        assert!(check(&dir, &key, &v("1.3.0"), "x86_64").unwrap_err().contains("doesn't match"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
