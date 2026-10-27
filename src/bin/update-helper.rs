//! `noobsbot-desktop-update <download dir>` (E6.13): the privileged half of Software Update, run as root through
//! `pkexec` (polkit action `org.noobsbot.desktop.update`). The dir is the updater's download (`<tarball>`,
//! `SHA256SUMS`, `SHA256SUMS.minisig`) and is owned by the user, so nothing in it is trusted: each file is opened in
//! that dir, never through a symlink, and must be a regular file; the signature is checked again with the root-owned
//! release key, the tarball is copied into a root-only staging dir while hashing (so it can't be swapped after the
//! check), the release must be newer than the installed one (no downgrade or replay), and only then is it unpacked to
//! `/usr/lib/noobsbot-desktop/versions/<v>` and `/usr/bin/noobsbot-desktop` switched to it. The previous versions stay
//! for the session wrapper's rollback.
//!
//! `noobsbot-desktop-update --revert <v>` (Settings' "Revert to <v>") points the links back at the installed, older
//! `versions/<v>` instead; nothing is downloaded or unpacked. It first adds the version it leaves to
//! `/var/lib/noobsbot-desktop/reverted`, which `--verified` never installs.
//!
//! `noobsbot-desktop-update --verified` (`[update] install_at_login`, run by the session wrapper at login) goes through
//! polkit action `org.noobsbot.desktop.update.verified`, which pkexec picks by that first argument and whose rules file
//! lets only an admin's active local session through without the password.
//! It takes no other argument (no revert, no path): it installs the pkexec caller's own download, found here (its dirs
//! not symlinks, its files the caller's own), exactly as above, so that action can only install a newer
//! release whose signature this helper checked, and never a pre-release or a version reverted from.
//!
//! Testing: `NOOBSBOT_UPDATE_ROOT=<dir>` puts every system path under `<dir>`; it is ignored when running as root.

use std::{
    ffi::{CStr, OsStr, OsString},
    fs::{self, File},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt, symlink},
        },
    },
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use semver::Version;

#[path = "../services/verify.rs"]
mod verify;

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let result = mode(&args).and_then(|mode| match mode {
        Mode::Install(dir) => opendir(&dir).map_err(|e| format!("{}: {e}", dir.display())).and_then(|dir| install(&root(), &dir, None)),
        Mode::Revert(version) => revert(&root(), &version),
        Mode::Verified => verified(),
    });
    match result {
        Ok(version) => {
            println!("Noobsbot Desktop {version} starts at the next login.");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("noobsbot-desktop-update: {err}; nothing was changed.");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug, PartialEq)]
enum Mode {
    Install(PathBuf),
    Revert(String),
    Verified,
}

/// The arguments. `--verified` (the passwordless action's) only alone; an unknown `--flag` is no download dir.
fn mode(args: &[OsString]) -> Result<Mode, String> {
    match args {
        [flag] if flag == "--verified" => Ok(Mode::Verified),
        [flag, version] if flag == "--revert" => Ok(Mode::Revert(version.to_string_lossy().into())),
        [dir] if !dir.as_bytes().starts_with(b"--") => Ok(Mode::Install(dir.into())),
        _ => Err("usage: noobsbot-desktop-update <download dir> | --revert <version> | --verified".into()),
    }
}

/// `--verified`: the newest download of the pkexec caller (`newest`). Its home comes from the caller's uid (pkexec's
/// `PKEXEC_UID`), never from an argument, and its files must be the caller's own.
fn verified() -> Result<Version, String> {
    let uid: libc::uid_t = std::env::var("PKEXEC_UID").ok().and_then(|u| u.parse().ok()).ok_or("--verified runs through pkexec")?;
    // SAFETY: getpwuid returns null or an entry valid until the next call; this helper is single-threaded.
    let home = unsafe {
        let user = libc::getpwuid(uid);
        if user.is_null() {
            return Err(format!("no user {uid}"));
        }
        PathBuf::from(OsStr::from_bytes(CStr::from_ptr((*user).pw_dir).to_bytes()))
    };
    install(&root(), &newest(&home)?, Some(uid))
}

/// The newest `<home>/.cache/noobsbot-desktop/updates/<v>/`, where the updater keeps its download. Each dir is opened
/// in the one before and never through a symlink, so nothing the user links there leads root out of their home.
// ponytail: `~/.cache` only, and not a symlinked one; a download elsewhere waits for Restart to Update.
fn newest(home: &Path) -> Result<File, String> {
    let mut dir = opendir(home).map_err(|e| format!("home: {e}"))?;
    for name in [".cache", "noobsbot-desktop", "updates"] {
        dir = openat(&dir, name, libc::O_DIRECTORY)?;
    }
    // The listing of exactly that open dir.
    let (_, name) = fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd()))
        .map_err(|e| format!("updates: {e}"))?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|name| Some((Version::parse(&name).ok()?, name)))
        .max()
        .ok_or("no download")?;
    openat(&dir, &name, libc::O_DIRECTORY)
}

/// `/`, or `NOOBSBOT_UPDATE_ROOT` when testing as a user.
fn root() -> PathBuf {
    // SAFETY: geteuid has no preconditions.
    match std::env::var_os("NOOBSBOT_UPDATE_ROOT") {
        Some(fake) if unsafe { libc::geteuid() } != 0 => PathBuf::from(fake),
        _ => PathBuf::from("/"),
    }
}

/// Points the links at the installed `versions/<version>`, which must be older than the current one.
fn revert(root: &Path, version: &str) -> Result<Version, String> {
    let at = |path: &str| root.join(path.trim_start_matches('/'));
    // Parsed, so it can't be a path.
    let version = Version::parse(version).map_err(|e| format!("{version}: {e}"))?;
    let versions = at("/usr/lib/noobsbot-desktop/versions");
    let current = installed(&at("/usr/bin/noobsbot-desktop"), &versions)?;
    if version >= current {
        return Err(format!("{version} is not older than the installed {current}"));
    }
    let target = versions.join(version.to_string());
    if !target.join("noobsbot-desktop").is_file() {
        return Err(format!("{version} is not installed"));
    }
    // Recorded first: install at login never brings `current` back by itself, even after a crash below.
    record(&at(verify::REVERTED), &current)?;
    for name in ["noobsbot-desktop", "noobsbot-desktop-settings"] {
        if target.join(name).exists() {
            relink(&target.join(name), &at(&format!("/usr/bin/{name}")))?;
        }
    }
    Ok(version)
}

/// Installs the download in `dir`; `caller` is `--verified`'s pkexec caller, who must own its files.
fn install(root: &Path, dir: &File, caller: Option<libc::uid_t>) -> Result<Version, String> {
    let at = |path: &str| root.join(path.trim_start_matches('/'));

    let key = fs::read_to_string(at(verify::PUBKEY)).map_err(|e| format!("{}: {e}", verify::PUBKEY))?;
    let sums = small(dir, "SHA256SUMS", caller)?;
    let sig = String::from_utf8(small(dir, "SHA256SUMS.minisig", caller)?).map_err(|_| "the signature is not text")?;
    let signed = verify::signed(&key, &sums, &sig, std::env::consts::ARCH)?;

    let lib = at("/usr/lib/noobsbot-desktop");
    let versions = lib.join("versions");
    let bin = at("/usr/bin/noobsbot-desktop");
    let current = installed(&bin, &versions)?;
    if signed.version <= current {
        return Err(format!("{} is not newer than the installed {current}", signed.version));
    }
    if caller.is_some() {
        let reverted = match fs::read_to_string(at(verify::REVERTED)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            read => read.map_err(|e| format!("{}: {e}", verify::REVERTED))?,
        };
        if !verify::unattended(&signed.version, &reverted) {
            return Err(format!("{} is a pre-release or was reverted: only Restart to Update installs it", signed.version));
        }
    }

    // Root-only staging, emptied first: a leftover from an interrupted run is never reused.
    let stage = versions.join(format!(".{}.new", signed.version));
    let _ = fs::remove_dir_all(&stage);
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&stage).map_err(|e| format!("{}: {e}", stage.display()))?;
    let result = unpack(dir, caller, &signed, &stage, &versions);
    let _ = fs::remove_dir_all(&stage);
    let target = result?;

    // Each link is replaced in one rename, so a crash leaves the old or the new one, never none.
    for name in ["noobsbot-desktop", "noobsbot-desktop-settings"] {
        if target.join(name).exists() {
            relink(&target.join(name), &at(&format!("/usr/bin/{name}")))?;
        }
    }
    // The helper itself sits at the fixed path the polkit action names; the new version brings its own.
    let helper = target.join("noobsbot-desktop-update");
    if helper.exists() {
        let new = lib.join(".noobsbot-desktop-update.new");
        fs::copy(&helper, &new).and_then(|_| fs::rename(&new, lib.join("noobsbot-desktop-update"))).map_err(|e| format!("helper: {e}"))?;
    }
    Ok(signed.version)
}

/// The version `/usr/bin/noobsbot-desktop` points at: install.sh and this helper link it to `versions/<v>/`.
/// Anything else (a pacman package, a hand-built binary) is not ours to replace.
fn installed(bin: &Path, versions: &Path) -> Result<Version, String> {
    let link = fs::read_link(bin).map_err(|e| format!("{}: {e} (not installed by install.sh?)", bin.display()))?;
    let version = link.strip_prefix(versions).ok().filter(|rest| rest.ends_with("noobsbot-desktop")).and_then(|rest| rest.parent());
    let version = version.and_then(Path::to_str).and_then(|v| Version::parse(v).ok());
    version.ok_or_else(|| format!("{} points at {}, not a release install.sh made", bin.display(), link.display()))
}

/// Copies the tarball into `stage` while hashing it, checks the hash, unpacks it and moves it to `versions/<v>`.
fn unpack(dir: &File, caller: Option<libc::uid_t>, signed: &verify::Signed, stage: &Path, versions: &Path) -> Result<PathBuf, String> {
    let copy = stage.join("release.tar.zst");
    let mut out = File::create_new(&copy).map_err(|e| format!("{}: {e}", copy.display()))?;
    let tarball = open(dir, &signed.tarball, caller)?;
    // A release is tens of MB: a huge file (or a device) put in its place fills no disk before its hash fails.
    let sha256 = verify::sha256(tarball.take(1 << 30), |chunk| out.write_all(chunk)).map_err(|e| format!("{}: {e}", signed.tarball))?;
    if sha256 != signed.sha256 {
        return Err(format!("{} doesn't match its signed SHA-256", signed.tarball));
    }
    drop(out);
    let tree = stage.join("tree");
    fs::create_dir(&tree).and_then(|_| fs::set_permissions(&tree, fs::Permissions::from_mode(0o755))).map_err(|e| e.to_string())?;
    let status = Command::new("/usr/bin/tar")
        .args(["--zstd", "--no-same-owner", "--strip-components=1", "-xf"])
        .arg(&copy)
        .arg("-C")
        .arg(&tree)
        .status()
        .map_err(|e| format!("tar: {e}"))?;
    if !status.success() {
        return Err(format!("tar failed ({status})"));
    }
    if !fs::metadata(tree.join("noobsbot-desktop")).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0) {
        return Err("the release has no noobsbot-desktop binary".into());
    }
    // A leftover of this version (an earlier install that was rolled back) is replaced; it isn't the running one.
    let target = versions.join(signed.version.to_string());
    if target.exists() {
        fs::remove_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    fs::rename(&tree, &target).map_err(|e| format!("{}: {e}", target.display()))?;
    Ok(target)
}

/// Adds `version` to the reverted list at `path`, root-owned and readable by the session whatever the umask.
fn record(path: &Path, version: &Version) -> Result<(), String> {
    let dir = path.parent().ok_or("no dir")?;
    fs::create_dir_all(dir)
        .and_then(|_| fs::set_permissions(dir, fs::Permissions::from_mode(0o755)))
        .and_then(|_| fs::OpenOptions::new().append(true).create(true).open(path))
        .and_then(|mut file| {
            file.set_permissions(fs::Permissions::from_mode(0o644))?;
            writeln!(file, "{version}")
        })
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Points `link` at `to` atomically.
fn relink(to: &Path, link: &Path) -> Result<(), String> {
    let new = link.with_extension("update-new");
    let _ = fs::remove_file(&new);
    symlink(to, &new).and_then(|_| fs::rename(&new, link)).map_err(|e| format!("{}: {e}", link.display()))
}

/// A small file of the download (the sums or the signature), at most 64 KiB.
fn small(dir: &File, name: &str, caller: Option<libc::uid_t>) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    open(dir, name, caller)?.take(64 * 1024).read_to_end(&mut bytes).map_err(|e| format!("{name}: {e}"))?;
    Ok(bytes)
}

/// A file of the user's download: never through a symlink, without waiting on a FIFO, and only a regular file (no
/// device), owned by `caller` when given (no hard link to someone else's file). Errors name only `name`.
fn open(dir: &File, name: &str, caller: Option<libc::uid_t>) -> Result<File, String> {
    let file = openat(dir, name, libc::O_NONBLOCK)?;
    let meta = file.metadata().map_err(|e| format!("{name}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{name} is not a regular file"));
    }
    if caller.is_some_and(|uid| meta.uid() != uid) {
        return Err(format!("{name} is not the caller's"));
    }
    Ok(file)
}

/// `name` in `dir`, read-only, never through a symlink; `flags` adds e.g. `O_DIRECTORY`.
fn openat(dir: &File, name: &str, flags: libc::c_int) -> Result<File, String> {
    let c = std::ffi::CString::new(name).map_err(|_| format!("{name}: bad name"))?;
    // SAFETY: a valid dir fd and C string.
    let fd = unsafe { libc::openat(dir.as_raw_fd(), c.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | flags) };
    if fd < 0 {
        return Err(format!("{name}: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: a new fd, owned by nothing else.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn opendir(path: &Path) -> std::io::Result<File> {
    fs::OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY).open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode_of(args: &[&str]) -> Result<Mode, String> {
        mode(&args.iter().map(OsString::from).collect::<Vec<_>>())
    }

    #[test]
    fn verified_takes_nothing_else() {
        assert_eq!(mode_of(&["--verified"]), Ok(Mode::Verified));
        for args in [&["--verified", "/tmp/x"][..], &["--verified", "--revert", "1.0.0"], &["--verified", "--verified"], &["--verified="], &["--other"], &[]] {
            assert!(mode_of(args).is_err(), "{args:?}");
        }
        assert_eq!(mode_of(&["--revert", "1.2.0"]), Ok(Mode::Revert("1.2.0".into())));
        assert_eq!(mode_of(&["/home/u/.cache/noobsbot-desktop/updates/1.3.0"]), Ok(Mode::Install("/home/u/.cache/noobsbot-desktop/updates/1.3.0".into())));
    }

    /// An empty scratch dir; the test removes it.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("noobsbot-update-helper-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn opens_only_the_callers_regular_files() {
        let tmp = scratch("open");
        fs::write(tmp.join("file"), "x").unwrap();
        symlink(tmp.join("file"), tmp.join("link")).unwrap();
        let fifo = std::ffi::CString::new(tmp.join("fifo").as_os_str().as_bytes()).unwrap();
        // SAFETY: a valid C string.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let dir = opendir(&tmp).unwrap();
        // SAFETY: getuid has no preconditions.
        let me = unsafe { libc::getuid() };
        assert!(open(&dir, "file", Some(me)).is_ok());
        assert!(open(&dir, "file", None).is_ok());
        // Someone else's file (a hard link to it, say): what a test can do without root.
        assert_eq!(open(&dir, "file", Some(me + 1)).unwrap_err(), "file is not the caller's");
        assert!(open(&dir, "link", None).unwrap_err().starts_with("link: "), "a symlink is never followed");
        assert_eq!(open(&dir, "fifo", None).unwrap_err(), "fifo is not a regular file");
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn newest_download_never_through_a_symlink() {
        let home = scratch("home");
        let updates = home.join(".cache/noobsbot-desktop/updates");
        for v in ["1.2.0", "1.10.0", "junk"] {
            fs::create_dir_all(updates.join(v)).unwrap();
        }
        let ino = |dir: File| dir.metadata().unwrap().ino();
        assert_eq!(ino(newest(&home).unwrap()), fs::metadata(updates.join("1.10.0")).unwrap().ino());
        // A newer version that is a symlink is refused, not followed.
        symlink("/", updates.join("2.0.0")).unwrap();
        assert!(newest(&home).unwrap_err().starts_with("2.0.0: "));
        fs::remove_file(updates.join("2.0.0")).unwrap();
        // So is a symlinked ~/.cache.
        fs::rename(home.join(".cache"), home.join("cache")).unwrap();
        symlink(home.join("cache"), home.join(".cache")).unwrap();
        assert!(newest(&home).unwrap_err().starts_with(".cache: "));
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn verified_never_reinstalls_a_reverted_version() {
        // The testdata is a signed 1.3.0 for x86_64.
        if std::env::consts::ARCH != "x86_64" {
            return;
        }
        let root = scratch("root");
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/services/testdata");
        let versions = root.join("usr/lib/noobsbot-desktop/versions");
        for v in ["1.2.0", "1.3.0"] {
            fs::create_dir_all(versions.join(v)).unwrap();
            fs::write(versions.join(v).join("noobsbot-desktop"), "").unwrap();
        }
        fs::create_dir_all(root.join("usr/bin")).unwrap();
        symlink(versions.join("1.3.0/noobsbot-desktop"), root.join("usr/bin/noobsbot-desktop")).unwrap();
        fs::create_dir_all(root.join("usr/share/noobsbot-desktop")).unwrap();
        fs::copy(data.join("minisign.pub"), root.join("usr/share/noobsbot-desktop/minisign.pub")).unwrap();
        let download = root.join("download");
        fs::create_dir_all(&download).unwrap();
        for f in ["SHA256SUMS", "SHA256SUMS.minisig"] {
            fs::copy(data.join(f), download.join(f)).unwrap();
        }
        fs::write(download.join("noobsbot-desktop-1.3.0-x86_64.tar.zst"), "pretend tarball\n").unwrap();
        let dir = opendir(&download).unwrap();
        // SAFETY: getuid has no preconditions.
        let me = Some(unsafe { libc::getuid() });

        // "Revert to 1.2.0" lists 1.3.0, world-readable.
        assert_eq!(revert(&root, "1.2.0"), Ok(Version::new(1, 2, 0)));
        let reverted = root.join("var/lib/noobsbot-desktop/reverted");
        assert_eq!(fs::read_to_string(&reverted).unwrap(), "1.3.0\n");
        assert_eq!(fs::metadata(&reverted).unwrap().permissions().mode() & 0o777, 0o644);
        // The 1.3.0 download is newer again: --verified refuses it; the password path gets as far as unpacking it
        // (no real tarball), and so does --verified once nothing is listed.
        assert!(install(&root, &dir, me).unwrap_err().contains("was reverted"));
        assert!(install(&root, &dir, None).unwrap_err().starts_with("tar"));
        fs::remove_file(&reverted).unwrap();
        assert!(install(&root, &dir, me).unwrap_err().starts_with("tar"));
        fs::remove_dir_all(&root).unwrap();
    }
}
