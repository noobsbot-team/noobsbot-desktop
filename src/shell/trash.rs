//! The Trash (freedesktop Trash spec): `~/.local/share/Trash/files` holds the items, `info/<name>.trashinfo`
//! where each came from and when; files on another disk go to that disk's `$topdir/.Trash-$uid`. Housekeeping
//! (E8.8) erases old items.

use std::{
    fs,
    path::{Path, PathBuf},
};

/// `$XDG_DATA_HOME/Trash`.
pub fn dir() -> PathBuf {
    crate::config::xdg_dir("XDG_DATA_HOME", ".local/share").join("Trash")
}

/// The home trash and every mounted disk's `.Trash-$uid` that exists.
pub fn dirs() -> Vec<PathBuf> {
    let mounts = fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    let uid = uid();
    let mut dirs = vec![dir()];
    dirs.extend(mounts.lines().filter_map(|l| l.split(' ').nth(1)).map(|m| Path::new(&unescape_mount(m)).join(format!(".Trash-{uid}"))).filter(|d| d.is_dir()));
    dirs
}

fn uid() -> u32 {
    // SAFETY: getuid has no failure mode.
    unsafe { libc::getuid() }
}

/// A `/proc/self/mounts` field with its octal escapes (`\040` for a space) undone.
fn unescape_mount(field: &str) -> String {
    field.replace("\\040", " ").replace("\\011", "\t").replace("\\012", "\n").replace("\\134", "\\")
}

/// The `DeletionDate` of a `.trashinfo` file (local time, as the spec has it).
fn deletion_date(info: &str) -> Option<chrono::NaiveDateTime> {
    let value = info.lines().find_map(|l| l.trim().strip_prefix("DeletionDate="))?;
    chrono::NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%dT%H:%M:%S").ok()
}

/// Erases the items moved to the trash at `trash` before `cutoff` (local time), and their info files.
pub fn empty_before(trash: &Path, cutoff: chrono::NaiveDateTime) {
    let Ok(infos) = fs::read_dir(trash.join("info")) else { return };
    for info in infos.flatten() {
        let path = info.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".trashinfo")) else { continue };
        if !fs::read_to_string(&path).ok().and_then(|t| deletion_date(&t)).is_some_and(|d| d < cutoff) {
            continue;
        }
        let item = trash.join("files").join(name);
        // A link is removed itself, never followed.
        let gone = match fs::symlink_metadata(&item) {
            Ok(m) if m.is_dir() => fs::remove_dir_all(&item),
            Ok(_) => fs::remove_file(&item),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        };
        match gone {
            Ok(()) => drop(fs::remove_file(&path)),
            Err(err) => tracing::warn!("trash: {}: {err}", item.display()),
        }
    }
    // A cache of folder sizes; rebuilt by whoever reads it.
    let _ = fs::remove_file(trash.join("directorysizes"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trashinfo_dates() {
        let info = "[Trash Info]\nPath=/home/u/a%20b.txt\nDeletionDate=2026-09-01T13:45:07\n";
        assert_eq!(deletion_date(info).map(|d| d.to_string()).as_deref(), Some("2026-09-01 13:45:07"));
        assert_eq!(deletion_date("[Trash Info]\nPath=/x\n"), None);
        assert_eq!(deletion_date("DeletionDate=yesterday"), None);
    }

    #[test]
    fn mount_escapes_are_undone() {
        assert_eq!(unescape_mount("/run/media/u/My\\040Disk"), "/run/media/u/My Disk");
    }
}
