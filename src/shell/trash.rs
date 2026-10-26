//! The home Trash (freedesktop Trash spec): `~/.local/share/Trash/files` holds the items, `info/<name>.trashinfo`
//! where each came from and when. The dock's Trash icon (E8.8) shows it empty or full, empties it and takes drops.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

use notify::{RecursiveMode, Watcher};

/// `$XDG_DATA_HOME/Trash`.
pub fn dir() -> PathBuf {
    crate::config::xdg_dir("XDG_DATA_HOME", ".local/share").join("Trash")
}

/// Something is in the trash at `dir`.
pub fn is_full(dir: &Path) -> bool {
    fs::read_dir(dir.join("files")).is_ok_and(|mut d| d.next().is_some())
}

/// As of the watcher's last look.
static FULL: AtomicBool = AtomicBool::new(false);

/// The dock icon: the theme's full or empty trash.
pub fn icon() -> &'static str {
    if FULL.load(Ordering::Relaxed) { "user-trash-full" } else { "user-trash" }
}

/// `path` percent-encoded as the spec asks (RFC 2396 escaping; `/` kept).
fn encode(path: &str) -> String {
    let mut out = String::new();
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A `.trashinfo` for `path` (absolute) deleted at `date` (local time, `YYYY-MM-DDThh:mm:ss`).
pub fn info_text(path: &Path, date: &str) -> String {
    format!("[Trash Info]\nPath={}\nDeletionDate={date}\n", encode(&path.to_string_lossy()))
}

/// The names to try for `name` in the trash: itself, then `stem.2.ext`, `stem.3.ext`, …
fn candidate(name: &str, n: u32) -> String {
    if n < 2 {
        return name.to_owned();
    }
    match name.rsplit_once('.').filter(|(stem, _)| !stem.is_empty()) {
        Some((stem, ext)) => format!("{stem}.{n}.{ext}"),
        None => format!("{name}.{n}"),
    }
}

/// Moves `path` into the trash at `dir`: claims a free name by creating its `.trashinfo` (exclusively, so two
/// trashings never take one name), then renames the item into `files/`.
// ponytail: same filesystem as home only (rename); a file on another disk fails with EXDEV and stays put. Add
// `$topdir/.Trash-$uid` trashes if that matters.
pub fn trash(dir: &Path, path: &Path, date: &str) -> io::Result<()> {
    let path = std::path::absolute(path)?;
    let name = path.file_name().ok_or_else(|| io::Error::other("no file name"))?.to_string_lossy().into_owned();
    let (files, info) = (dir.join("files"), dir.join("info"));
    fs::create_dir_all(&files)?;
    fs::create_dir_all(&info)?;
    for n in 1.. {
        let name = candidate(&name, n);
        let info_path = info.join(format!("{name}.trashinfo"));
        if files.join(&name).exists() {
            continue;
        }
        match fs::OpenOptions::new().write(true).create_new(true).open(&info_path) {
            Ok(mut f) => {
                io::Write::write_all(&mut f, info_text(&path, date).as_bytes())?;
                return fs::rename(&path, files.join(&name)).inspect_err(|_| drop(fs::remove_file(&info_path)));
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// Erases everything in the trash at `dir`: `files/`, `info/` and the `directorysizes` cache.
pub fn empty(dir: &Path) {
    for sub in ["files", "info"] {
        let Ok(entries) = fs::read_dir(dir.join(sub)) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let gone = if entry.file_type().is_ok_and(|t| t.is_dir()) { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
            let _ = gone.inspect_err(|err| tracing::warn!("trash: {}: {err}", path.display()));
        }
    }
    let _ = fs::remove_file(dir.join("directorysizes"));
}

/// Watches `files/`, keeping `icon` current and calling `changed` whenever it flips; stops when `changed` returns false.
pub fn watch(mut changed: impl FnMut() -> bool + Send + 'static) {
    let spawned = std::thread::Builder::new().name("trash-watch".into()).spawn(move || {
        let files = dir().join("files");
        let _ = fs::create_dir_all(&files);
        FULL.store(is_full(&dir()), Ordering::Relaxed);
        if !changed() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let mut watcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(err) => return tracing::warn!("trash: no live icon: {err}"),
        };
        if let Err(err) = watcher.watch(&files, RecursiveMode::NonRecursive) {
            return tracing::warn!("trash: {}: {err}", files.display());
        }
        while let Ok(event) = rx.recv() {
            if !event.is_ok_and(|e| e.kind.is_create() || e.kind.is_remove() || e.kind.is_modify()) {
                continue;
            }
            let full = is_full(&dir());
            if FULL.swap(full, Ordering::Relaxed) != full && !changed() {
                return;
            }
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("trash: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("noobsbot-desktop-test-trash-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn trashinfo_is_the_specs() {
        let text = info_text(Path::new("/home/a/My File#1.txt"), "2026-10-26T09:30:00");
        assert_eq!(text, "[Trash Info]\nPath=/home/a/My%20File%231.txt\nDeletionDate=2026-10-26T09:30:00\n");
        assert_eq!(encode("/ü"), "/%C3%BC");
    }

    #[test]
    fn names_count_up_before_the_extension() {
        assert_eq!(candidate("a.txt", 1), "a.txt");
        assert_eq!(candidate("a.txt", 2), "a.2.txt");
        assert_eq!(candidate("notes", 3), "notes.3");
        assert_eq!(candidate(".bashrc", 2), ".bashrc.2");
    }

    #[test]
    fn trashing_fills_and_emptying_clears() {
        let root = temp_dir("roundtrip");
        let trash_dir = root.join("Trash");
        assert!(!is_full(&trash_dir));
        for _ in 0..2 {
            fs::write(root.join("a.txt"), "x").unwrap();
            trash(&trash_dir, &root.join("a.txt"), "2026-10-26T09:30:00").unwrap();
        }
        assert!(!root.join("a.txt").exists());
        assert!(trash_dir.join("files/a.txt").exists() && trash_dir.join("files/a.2.txt").exists());
        let info = fs::read_to_string(trash_dir.join("info/a.2.txt.trashinfo")).unwrap();
        assert!(info.contains(&format!("Path={}/a.txt", encode(&root.to_string_lossy()))));
        assert!(is_full(&trash_dir));
        fs::create_dir_all(trash_dir.join("files/dir/sub")).unwrap();
        empty(&trash_dir);
        assert!(!is_full(&trash_dir));
        assert_eq!(fs::read_dir(trash_dir.join("info")).unwrap().count(), 0);
        let _ = fs::remove_dir_all(&root);
    }
}
