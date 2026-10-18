//! Software Update check (E6.13): once at start and every 24 h, ask GitHub for the latest stable release and compare
//! it with the running version. No account, no identifier: the request carries only the version in its User-Agent.
//! Fails silently when offline. Off in nested mode and with `check_updates = false`. A newer release shows a
//! notification whose Details button opens its release page.

use std::{process::Command, sync::LazyLock, time::Duration};

use smithay::reexports::calloop::channel::Sender;

use regex_lite::Regex;
use semver::Version;

const URL: &str = "https://api.github.com/repos/anil0403/noobsbot-desktop/releases/latest";
const EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// A release newer than the running version.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: Version,
    /// The release page (notes, downloads).
    pub url: String,
}

/// Its own thread: the check blocks on the network for up to 20 s, and between checks it only sleeps (no wakeups).
pub fn spawn(found: Sender<Release>) {
    let thread = std::thread::Builder::new().name("update".into()).spawn(move || {
        let running = Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml has a SemVer version");
        loop {
            if let Some(release) = fetch().and_then(|json| newer(&json, &running)) {
                // ponytail: notify only; the download + minisign check, the polkit install helper, the logo-menu
                // badge and the Settings pane (M7) come next.
                tracing::info!("Noobsbot Desktop {} is available: {}", release.version, release.url);
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
    super::notifications::Notification {
        id: NOTICE_ID,
        app: "Software Update".into(),
        icon_name: icon.into(),
        icon: super::notifications::load_icon(icon).map(std::sync::Arc::new),
        summary: format!("Noobsbot Desktop {}.{} is available", release.version.major, release.version.minor),
        body: format!("Version {} is ready to install.", release.version),
        actions: vec![("default".into(), String::new()), ("details".into(), "Details".into())],
        urgency: 1,
        expire: -1,
        time: chrono::Local::now().timestamp(),
        ..Default::default()
    }
}

/// The release JSON, or `None` when offline or GitHub refuses.
// ponytail: curl (on every install) instead of an HTTP + TLS crate; switch to ureq if curl ever goes missing.
fn fetch() -> Option<String> {
    let agent = concat!("noobsbot-desktop/", env!("CARGO_PKG_VERSION"));
    let output = Command::new("curl")
        .args(["-fsSL", "--max-time", "20", "-A", agent, "-H", "Accept: application/vnd.github+json", URL])
        .output()
        .inspect_err(|err| tracing::debug!("update check: curl: {err}"))
        .ok()?;
    if !output.status.success() {
        tracing::debug!("update check: curl exited with {}", output.status);
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// The release in a GitHub `releases/latest` response, if it is newer than `running`. Drafts and pre-releases
/// never come from that endpoint, so only the tag matters.
pub fn newer(json: &str, running: &Version) -> Option<Release> {
    // Keys are unescaped quotes, so a `"tag_name"` inside the release notes (`\"tag_name\"`) can't match.
    static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""tag_name"\s*:\s*"v?([^"]+)""#).unwrap());
    static PAGE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""html_url"\s*:\s*"([^"]+/releases/tag/[^"]+)""#).unwrap());
    let version = Version::parse(TAG.captures(json)?.get(1)?.as_str()).ok()?;
    let url = PAGE.captures(json).map_or_else(|| "https://github.com/anil0403/noobsbot-desktop/releases".into(), |c| c[1].to_string());
    (version > *running).then_some(Release { version, url })
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"{"url":"https://api.github.com/repos/anil0403/noobsbot-desktop/releases/1",
        "html_url": "https://github.com/anil0403/noobsbot-desktop/releases/tag/v1.3.0",
        "author": {"html_url": "https://github.com/anil0403"},
        "tag_name": "v1.3.0", "draft": false, "prerelease": false,
        "body": "notes quoting \"tag_name\": \"v9.0.0\""}"#;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn newer_release() {
        let release = newer(JSON, &v("1.2.4")).unwrap();
        assert_eq!(release.version, v("1.3.0"));
        assert_eq!(release.url, "https://github.com/anil0403/noobsbot-desktop/releases/tag/v1.3.0");
    }

    #[test]
    fn same_or_older_is_none() {
        assert_eq!(newer(JSON, &v("1.3.0")), None);
        assert_eq!(newer(JSON, &v("1.10.0")), None);
    }

    #[test]
    fn beta_before_its_release_updates() {
        // SemVer: 1.3.0-beta.2 < 1.3.0, so a Beta user gets the final release.
        assert!(newer(JSON, &v("1.3.0-beta.2")).is_some());
        assert!(newer(JSON, &v("1.4.0-beta.1")).is_none());
    }

    #[test]
    fn garbage_is_none() {
        assert_eq!(newer("", &v("0.1.0")), None);
        assert_eq!(newer(r#"{"message":"Not Found"}"#, &v("0.1.0")), None);
        assert_eq!(newer(r#"{"tag_name":"nightly"}"#, &v("0.1.0")), None);
    }

    #[test]
    fn notice_names_major_minor() {
        let n = notice(&Release { version: v("1.3.2"), url: String::new() });
        assert_eq!((n.id, n.summary.as_str(), n.body.as_str()), (NOTICE_ID, "Noobsbot Desktop 1.3 is available", "Version 1.3.2 is ready to install."));
    }

    #[test]
    fn missing_page_falls_back_to_releases() {
        let release = newer(r#"{"tag_name":"2.0.0"}"#, &v("1.9.9")).unwrap();
        assert_eq!(release.url, "https://github.com/anil0403/noobsbot-desktop/releases");
    }
}
