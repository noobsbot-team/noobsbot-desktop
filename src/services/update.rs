//! Software Update (E6.13): once at start and every 24 h, ask GitHub for the newest release on the chosen channel
//! and compare it with the running version. No account, no identifier: the request carries only the version in its
//! User-Agent. Fails silently when offline. Off in nested mode and with `check_updates = false`.
//!
//! A newer release on an install.sh install (versions under `/usr/lib/noobsbot-desktop/versions/`, release key
//! installed) is downloaded in the background to `~/.cache/noobsbot-desktop/updates/<v>/` and verified (minisign
//! signature of `SHA256SUMS`, then the tarball's SHA-256); a bad download is deleted. The notification then offers
//! "Restart to Update", which runs the privileged helper through pkexec and logs out, so the next login starts the
//! new version. Anything else (an AUR package, a hand-built binary, no key yet) only notifies.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::LazyLock,
    time::Duration,
};

use smithay::reexports::calloop::channel::Sender;

use regex_lite::Regex;
use semver::Version;

use super::verify;

const REPO: &str = "https://github.com/noobsbot-team/noobsbot-desktop";
const API: &str = "https://api.github.com/repos/noobsbot-team/noobsbot-desktop";
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

/// Its own thread: the check and download block on the network, and between checks it only sleeps (no wakeups).
pub fn spawn(found: Sender<Release>, channel: Channel) {
    let thread = std::thread::Builder::new().name("update".into()).spawn(move || {
        let running = Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml has a SemVer version");
        let url = match channel {
            Channel::Stable => format!("{API}/releases/latest"),
            Channel::Beta => format!("{API}/releases?per_page=20"),
        };
        loop {
            if let Some(mut release) = fetch(&url).and_then(|json| newer(&json, &running, channel)) {
                release.download = download(&release);
                tracing::info!("Noobsbot Desktop {} is available ({:?}): {}", release.version, release.download, release.url);
                if found.send(release).is_err() {
                    return;
                }
            }
            std::thread::sleep(EVERY);
        }
    });
    if let Err(err) = thread {
        tracing::warn!("update check off: {err}");
    }
}

/// The update notification's id, below the disk notices' range (shell/disks.rs) and far above the server's counter.
pub const NOTICE_ID: u32 = 0xFFFE_0000;

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

/// The newest release in a GitHub `releases/latest` (one) or `releases` (a list) response, if it is newer than
/// `running`. Stable ignores pre-releases; GitHub never lists drafts to anonymous requests.
pub fn newer(json: &str, running: &Version, channel: Channel) -> Option<Release> {
    // Keys are unescaped quotes, so a `"tag_name"` inside the release notes (`\"tag_name\"`) can't match.
    static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""tag_name"\s*:\s*"(v?([^"]+))""#).unwrap());
    let (version, tag) = TAG
        .captures_iter(json)
        .filter_map(|c| Some((Version::parse(c.get(2)?.as_str()).ok()?, c[1].to_string())))
        .filter(|(v, _)| channel == Channel::Beta || v.pre.is_empty())
        .max_by(|a, b| a.0.cmp(&b.0))?;
    let url = format!("{REPO}/releases/tag/{tag}");
    (version > *running).then_some(Release { version, tag, url, download: Download::None })
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

impl crate::state::State {
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
        let (done, finished) = smithay::reexports::calloop::channel::channel();
        let back = dir.clone();
        let inserted = self.loop_handle.insert_source(finished, move |event, _, state| {
            let smithay::reexports::calloop::channel::Event::Msg(ok) = event else { return };
            if ok {
                state.update = None;
                state.close_notification(NOTICE_ID, super::notifications::Reason::Dismissed);
                state.open_alert(crate::shell::alert::Power::LogOut);
            } else if let Some(release) = state.update.as_mut() {
                // Cancelled or refused (the helper logs why): offered again.
                release.download = Download::Ready(back.clone());
            }
            state.schedule_redraw();
        });
        if let Err(err) = inserted {
            tracing::warn!("update: {err}");
            return;
        }
        std::thread::spawn(move || {
            let status = Command::new("pkexec").arg(HELPER).arg(&dir).status();
            tracing::info!("update: {HELPER}: {status:?}");
            let _ = done.send(status.is_ok_and(|s| s.success()));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"{"url":"https://api.github.com/repos/noobsbot-team/noobsbot-desktop/releases/1",
        "html_url": "https://github.com/noobsbot-team/noobsbot-desktop/releases/tag/v1.3.0",
        "author": {"html_url": "https://github.com/anil0403"},
        "tag_name": "v1.3.0", "draft": false, "prerelease": false,
        "body": "notes quoting \"tag_name\": \"v9.0.0\""}"#;

    const LIST: &str = r#"[{"tag_name": "v1.4.0-beta.2", "prerelease": true}, {"tag_name": "v1.3.0"},
        {"tag_name": "v1.4.0-beta.1", "prerelease": true}]"#;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn newer_release() {
        let release = newer(JSON, &v("1.2.4"), Channel::Stable).unwrap();
        assert_eq!((release.version, release.tag.as_str()), (v("1.3.0"), "v1.3.0"));
        assert_eq!(release.url, "https://github.com/noobsbot-team/noobsbot-desktop/releases/tag/v1.3.0");
        assert_eq!(release.download, Download::None);
    }

    #[test]
    fn same_or_older_is_none() {
        assert_eq!(newer(JSON, &v("1.3.0"), Channel::Stable), None);
        assert_eq!(newer(JSON, &v("1.10.0"), Channel::Stable), None);
    }

    #[test]
    fn beta_before_its_release_updates() {
        // SemVer: 1.3.0-beta.2 < 1.3.0, so a Beta user gets the final release.
        assert!(newer(JSON, &v("1.3.0-beta.2"), Channel::Stable).is_some());
        assert!(newer(JSON, &v("1.4.0-beta.1"), Channel::Stable).is_none());
    }

    #[test]
    fn channels() {
        assert_eq!(newer(LIST, &v("1.2.0"), Channel::Beta).unwrap().version, v("1.4.0-beta.2"));
        assert_eq!(newer(LIST, &v("1.2.0"), Channel::Stable).unwrap().version, v("1.3.0"));
        assert!(newer(LIST, &v("1.3.0"), Channel::Stable).is_none());
    }

    #[test]
    fn garbage_is_none() {
        assert_eq!(newer("", &v("0.1.0"), Channel::Beta), None);
        assert_eq!(newer(r#"{"message":"Not Found"}"#, &v("0.1.0"), Channel::Stable), None);
        assert_eq!(newer(r#"{"tag_name":"nightly"}"#, &v("0.1.0"), Channel::Stable), None);
    }

    #[test]
    fn notice_names_major_minor() {
        let release = newer(r#"{"tag_name":"1.3.2"}"#, &v("1.0.0"), Channel::Stable).unwrap();
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
