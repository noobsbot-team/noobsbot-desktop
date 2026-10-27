//! Housekeeping (E8.8): a low-disk notification checked hourly, and once a day, off the frame thread, old Trash
//! items, unused thumbnails and cached icons, and old recent-files entries cleared.

use std::{
    fs,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use smithay::reexports::calloop::{
    channel,
    timer::{TimeoutAction, Timer},
};

use crate::{
    config::{self, Housekeeping},
    services::notifications::{Notification, load_icon},
    shell::trash,
    state::State,
};

/// The low-disk notification's id (replaced, not stacked); below the calendar alerts' ids.
pub const LOW_DISK: u32 = 0xFFFC_0000;
const HOUR: Duration = Duration::from_secs(3600);
const DAY: Duration = Duration::from_secs(86_400);
/// Thumbnails not opened for this long are removed.
const THUMBNAIL_AGE: Duration = Duration::from_secs(90 * 86_400);
/// Cached icons (`apps::icon_cache`) not drawn for this long are removed.
const ICON_AGE: Duration = Duration::from_secs(30 * 86_400);

/// Under 5% or 2 GB free.
fn low(free: u64, total: u64) -> bool {
    free < 2_000_000_000 || free.saturating_mul(20) < total
}

/// (free, total) bytes of the file system holding `path`.
fn space(path: &Path) -> Option<(u64, u64)> {
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: a valid C string and a zeroed out struct for statvfs to fill.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(path.as_ptr(), &mut s) } == 0).then(|| (s.f_bavail * s.f_frsize, s.f_blocks * s.f_frsize))
}

/// Starts the hourly check, its first a minute after login (once outputs and the menu bar are up). The disks are
/// looked at on a thread, so a hung network home never stalls a frame; the answer comes back through a channel.
pub fn start(state: &State) {
    let (tx, rx) = channel::channel();
    let mut warned = false;
    let answers = state.loop_handle.insert_source(rx, move |event, _, state| {
        let channel::Event::Msg(full) = event else { return };
        match full {
            Some((name, free)) if !warned => {
                warned = true;
                state.notify(notice(name, free));
            }
            None => warned = false,
            _ => {}
        }
    });
    if let Err(err) = answers {
        return tracing::warn!("housekeeping: {err}");
    }
    let timer = state.loop_handle.insert_source(Timer::from_duration(Duration::from_secs(60)), move |_, _, state| {
        let (tx, settings) = (tx.clone(), state.config.housekeeping.clone());
        let spawned = std::thread::Builder::new().name("housekeeping".into()).spawn(move || {
            let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
            let full = [("the system disk", Path::new("/")), ("Home", home.as_path())].into_iter().find_map(|(name, p)| {
                let (free, total) = space(p)?;
                low(free, total).then_some((name, free))
            });
            let _ = tx.send(full);
            if due(&stamp()) {
                clean(&settings);
            }
        });
        if let Err(err) = spawned {
            tracing::warn!("housekeeping: {err}");
        }
        TimeoutAction::ToDuration(HOUR)
    });
    if let Err(err) = timer {
        tracing::warn!("housekeeping timer: {err}");
    }
}

/// "Your disk is almost full", with Open Storage.
fn notice(name: &str, free: u64) -> Notification {
    let icon = "drive-harddisk";
    Notification {
        id: LOW_DISK,
        app: "Storage".into(),
        icon_name: icon.into(),
        icon: load_icon(icon).map(std::sync::Arc::new),
        summary: "Your disk is almost full".into(),
        body: format!("Only {:.1} GB left on {name}. Empty the Trash or remove files you no longer need.", free as f64 / 1e9),
        actions: vec![("default".into(), String::new()), ("storage".into(), "Open Storage".into())],
        urgency: 1,
        expire: -1,
        time: chrono::Local::now().timestamp(),
        ..Default::default()
    }
}

/// Marks the last cleanup, so it runs once a day across restarts too.
fn stamp() -> PathBuf {
    config::xdg_dir("XDG_CACHE_HOME", ".cache").join("noobsbot-desktop/housekeeping")
}

/// A day since the last cleanup; touches the stamp when it is.
fn due(stamp: &Path) -> bool {
    let last = fs::metadata(stamp).and_then(|m| m.modified()).ok();
    if last.is_some_and(|t| t.elapsed().is_ok_and(|age| age < DAY)) {
        return false;
    }
    let _ = fs::create_dir_all(stamp.parent().unwrap_or(Path::new("/")));
    fs::write(stamp, chrono::Local::now().to_rfc3339()).is_ok()
}

/// The daily cleanups; runs on the housekeeping thread.
fn clean(settings: &Housekeeping) {
    if settings.empty_trash_days > 0 {
        let cutoff = chrono::Local::now().naive_local() - chrono::Duration::days(settings.empty_trash_days.into());
        trash::dirs().iter().for_each(|d| trash::empty_before(d, cutoff));
    }
    let cache = config::xdg_dir("XDG_CACHE_HOME", ".cache");
    remove_unused(&cache.join("thumbnails"), THUMBNAIL_AGE);
    remove_unused(&cache.join("noobsbot-desktop/icons"), ICON_AGE);
    if settings.recent_days > 0 {
        let path = crate::shell::dock::recent_path();
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(settings.recent_days.into())).format("%Y-%m-%dT%H:%M:%SZ").to_string();
        // ponytail: read, prune, rename; an app writing the file in between loses that one entry.
        if let Some(pruned) = fs::read_to_string(&path).ok().and_then(|xbel| prune_recent(&xbel, &cutoff)) {
            let _ = config::save_atomic(&path, &pruned).inspect_err(|err| tracing::warn!("{}: {err}", path.display()));
        }
    }
}

/// Files in each folder of `dir` (thumbnail sizes, icon cache versions) not opened (nor made) within `age`.
fn remove_unused(dir: &Path, age: Duration) {
    let Ok(sizes) = fs::read_dir(dir) else { return };
    for file in sizes.flatten().filter_map(|d| fs::read_dir(d.path()).ok()).flatten().flatten() {
        let Ok(m) = file.metadata() else { continue };
        let used = m.accessed().into_iter().chain(m.modified()).max().unwrap_or(SystemTime::now());
        if m.is_file() && used.elapsed().is_ok_and(|unused| unused > age) {
            let _ = fs::remove_file(file.path());
        }
    }
}

/// `xbel` without the bookmarks last added, modified or visited before `cutoff` (ISO 8601 UTC, which sorts as text),
/// everything else verbatim; `None` when nothing goes or the file is not as expected.
fn prune_recent(xbel: &str, cutoff: &str) -> Option<String> {
    let attr = |head: &str, key: &str| {
        let start = head.find(&format!(" {key}=\""))? + key.len() + 3;
        Some(head[start..start + head[start..].find('"')?].to_owned())
    };
    let mut out = String::with_capacity(xbel.len());
    let mut rest = xbel;
    let mut dropped = false;
    while let Some(start) = rest.find("<bookmark ") {
        let head_end = start + rest[start..].find('>')? + 1;
        let end = if rest[..head_end].ends_with("/>") { head_end } else { head_end + rest[head_end..].find("</bookmark>")? + "</bookmark>".len() };
        let head = &rest[start..head_end];
        let newest = ["added", "modified", "visited"].into_iter().filter_map(|k| attr(head, k)).max();
        if newest.is_some_and(|t| t.as_str() < cutoff) {
            dropped = true;
            // Its whole line goes when it has one to itself.
            let line = rest[..start].trim_end_matches([' ', '\t']);
            if line.is_empty() || line.ends_with('\n') {
                out.push_str(line);
                rest = rest[end..].strip_prefix('\n').unwrap_or(&rest[end..]);
            } else {
                out.push_str(&rest[..start]);
                rest = &rest[end..];
            }
        } else {
            out.push_str(&rest[..end]);
            rest = &rest[end..];
        }
    }
    out.push_str(rest);
    dropped.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_disk_threshold() {
        const GB: u64 = 1_000_000_000;
        assert!(low(GB, 10 * GB), "under 2 GB");
        assert!(!low(3 * GB, 10 * GB));
        assert!(low(20 * GB, 500 * GB), "under 5%");
        assert!(!low(30 * GB, 500 * GB));
        assert!(low(0, 0));
    }

    #[test]
    fn xbel_pruning() {
        let xbel = r#"<?xml version="1.0" encoding="UTF-8"?>
<xbel version="1.0" xmlns:bookmark="http://www.freedesktop.org/standards/desktop-bookmarks">
  <bookmark href="file:///old" added="2026-01-01T00:00:00Z" modified="2026-01-02T00:00:00Z" visited="2026-01-03T00:00:00Z">
    <info><metadata owner="http://freedesktop.org"><bookmark:applications>
      <bookmark:application name="Kate" exec="&apos;kate %u&apos;" modified="2026-12-01T00:00:00Z" count="1"/>
    </bookmark:applications></metadata></info>
  </bookmark>
  <bookmark href="file:///new" added="2026-01-01T00:00:00Z" visited="2026-10-20T00:00:00Z">
    <info><mime:mime-type type="text/plain"/></info>
  </bookmark>
  <bookmark href="file:///old2" modified="2025-12-31T23:59:59Z"/>
</xbel>
"#;
        let kept = prune_recent(xbel, "2026-09-26T00:00:00Z").unwrap();
        assert_eq!(
            kept,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<xbel version="1.0" xmlns:bookmark="http://www.freedesktop.org/standards/desktop-bookmarks">
  <bookmark href="file:///new" added="2026-01-01T00:00:00Z" visited="2026-10-20T00:00:00Z">
    <info><mime:mime-type type="text/plain"/></info>
  </bookmark>
</xbel>
"#
        );
        assert_eq!(prune_recent(&kept, "2026-09-26T00:00:00Z"), None, "nothing older");
        assert_eq!(prune_recent("<xbel><bookmark href=\"x\" added=\"2000-01-01T00:00:00Z\">", "2026"), None, "cut short");
    }
}
