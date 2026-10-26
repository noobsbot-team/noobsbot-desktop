//! The Trash (freedesktop Trash spec): `~/.local/share/Trash/files` holds the items, `info/<name>.trashinfo`
//! where each came from and when; files on another disk go to that disk's `$topdir/.Trash-$uid`. The dock's Trash
//! icon (E8.8) shows them empty or full, empties them and takes drops.

use std::{
    fs, io,
    os::unix::fs::{DirBuilderExt, MetadataExt},
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

/// The device holding `path`, or its nearest existing ancestor.
fn device(path: &Path) -> io::Result<u64> {
    let existing = path.ancestors().find(|p| p.exists()).unwrap_or(Path::new("/"));
    Ok(fs::metadata(existing)?.dev())
}

/// The top directory of the mount holding `path` (absolute): its highest ancestor on the same device.
fn topdir(path: &Path) -> io::Result<PathBuf> {
    let dev = device(path)?;
    let mut top = path.to_path_buf();
    for p in path.ancestors().skip(1) {
        if device(p)? != dev {
            break;
        }
        top = p.to_path_buf();
    }
    Ok(top)
}

/// The trash `path` (absolute) goes to: `home` when on its disk, else `$topdir/.Trash-$uid`, made with mode 0700.
fn trash_for(home: &Path, path: &Path) -> io::Result<PathBuf> {
    let parent = path.parent().unwrap_or(path);
    if device(parent)? == device(home)? {
        return Ok(home.to_path_buf());
    }
    let dir = topdir(parent)?.join(format!(".Trash-{}", uid()));
    match fs::DirBuilder::new().mode(0o700).create(&dir) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    // The spec: not a link, and ours alone.
    let meta = fs::symlink_metadata(&dir)?;
    if !meta.is_dir() || meta.uid() != uid() {
        return Err(io::Error::other(format!("{} is not this user's trash", dir.display())));
    }
    Ok(dir)
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

/// Moves `path` into the home trash `home`, or its disk's trash when elsewhere: claims a free name by creating its
/// `.trashinfo` (exclusively, so two trashings never take one name), then renames the item into `files/`. A disk
/// without a usable trash (read-only, foreign) is an error; the file stays.
pub fn trash(home: &Path, path: &Path, date: &str) -> io::Result<()> {
    let path = std::path::absolute(path)?;
    let dir = &trash_for(home, &path)?;
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
                fs::rename(&path, files.join(&name)).inspect_err(|_| drop(fs::remove_file(&info_path)))?;
                // The watcher sees only the trashes there were at start.
                FULL.store(true, Ordering::Relaxed);
                return Ok(());
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

/// Empties every trash (`dirs`).
pub fn empty_all() {
    dirs().iter().for_each(|d| empty(d));
    FULL.store(any_full(), Ordering::Relaxed);
}

/// Something is in any trash (`dirs`).
pub fn any_full() -> bool {
    dirs().iter().any(|d| is_full(d))
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

/// Watches `files/`, keeping `icon` current and calling `changed` whenever it flips; stops when `changed` returns false.
pub fn watch(mut changed: impl FnMut() -> bool + Send + 'static) {
    let spawned = std::thread::Builder::new().name("trash-watch".into()).spawn(move || {
        let _ = fs::create_dir_all(dir().join("files"));
        FULL.store(any_full(), Ordering::Relaxed);
        if !changed() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let mut watcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(err) => return tracing::warn!("trash: no live icon: {err}"),
        };
        for files in dirs().iter().map(|d| d.join("files")) {
            if let Err(err) = watcher.watch(&files, RecursiveMode::NonRecursive) {
                tracing::warn!("trash: {}: {err}", files.display());
            }
        }
        while let Ok(event) = rx.recv() {
            if !event.is_ok_and(|e| e.kind.is_create() || e.kind.is_remove() || e.kind.is_modify()) {
                continue;
            }
            let full = any_full();
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

    #[test]
    fn trashinfo_dates() {
        let info = "[Trash Info]\nPath=/home/u/a%20b.txt\nDeletionDate=2026-09-01T13:45:07\n";
        assert_eq!(deletion_date(info).map(|d| d.to_string()).as_deref(), Some("2026-09-01 13:45:07"));
        assert_eq!(deletion_date("[Trash Info]\nPath=/x\n"), None);
        assert_eq!(deletion_date("DeletionDate=yesterday"), None);
    }

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

    #[test]
    fn other_disks_use_their_own_trash() {
        let root = temp_dir("disks");
        let home = root.join("Trash");
        // Same disk: home.
        assert_eq!(trash_for(&home, &root.join("a")).unwrap(), home);
        let top = topdir(&root.join("a")).unwrap();
        assert!(root.starts_with(&top));
        assert!(top.parent().is_none_or(|p| device(p).unwrap() != device(&top).unwrap()), "the mount's top");
        assert_eq!(unescape_mount("/run/media/u/My\\040Disk"), "/run/media/u/My Disk");
        // Another disk (the build dir is not on the temp dir's in some setups): `$topdir/.Trash-$uid`, 0700.
        let elsewhere = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        if device(&elsewhere).unwrap() != device(&root).unwrap() {
            let own = top.join(format!(".Trash-{}", uid()));
            let made = !own.exists();
            let dir = trash_for(&elsewhere.join("Trash"), &root.join("a")).unwrap();
            assert_eq!(dir, own);
            assert_eq!(fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
            fs::write(root.join("b.txt"), "x").unwrap();
            trash(&elsewhere.join("Trash"), &root.join("b.txt"), "2026-10-26T09:30:00").unwrap();
            assert!(!root.join("b.txt").exists());
            let _ = fs::remove_file(dir.join("files/b.txt"));
            let _ = fs::remove_file(dir.join("info/b.txt.trashinfo"));
            if made {
                let _ = fs::remove_dir_all(&dir);
            }
        }
        let _ = fs::remove_dir_all(&root);
    }
}
