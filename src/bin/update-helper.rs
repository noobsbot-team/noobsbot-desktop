//! `noobsbot-desktop-update <download dir>` (E6.13): the privileged half of Software Update, run as root through
//! `pkexec` (polkit action `org.noobsbot.desktop.update`). The dir is the updater's download (`<tarball>`,
//! `SHA256SUMS`, `SHA256SUMS.minisig`) and is owned by the user, so nothing in it is trusted: the signature is checked
//! again with the root-owned release key, the tarball is copied into a root-only staging dir while hashing (so it
//! can't be swapped after the check), the release must be newer than the installed one (no downgrade or replay), and
//! only then is it unpacked to `/usr/lib/noobsbot-desktop/versions/<v>` and `/usr/bin/noobsbot-desktop` switched to it.
//! The previous versions stay for the session wrapper's rollback.
//!
//! `noobsbot-desktop-update --revert <v>` (Settings' "Revert to <v>") points the links back at the installed, older
//! `versions/<v>` instead; nothing is downloaded or unpacked.
//!
//! `noobsbot-desktop-update --verified` (`[update] install_at_login`, run by the session wrapper at login) goes through
//! the passwordless polkit action `org.noobsbot.desktop.update.verified`, which pkexec picks by that first argument.
//! It takes no other argument (no revert, no path): it installs the pkexec caller's own download, found here, exactly
//! as above, so that action can only install a newer release whose signature this helper checked.
//!
//! Testing: `NOOBSBOT_UPDATE_ROOT=<dir>` puts every system path under `<dir>`; it is ignored when running as root.

use std::{
    ffi::{CStr, OsStr, OsString},
    fs::{self, File},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink},
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
        Mode::Install(dir) => install(&dir),
        Mode::Revert(version) => revert(&version),
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

/// `--verified`: the newest download in the pkexec caller's `~/.cache/noobsbot-desktop/updates/<v>/`, where the updater
/// keeps it. The path comes from the caller's uid (pkexec's `PKEXEC_UID`), never from an argument.
// ponytail: `~/.cache` only; a download under another XDG_CACHE_HOME isn't found, and waits for Restart to Update.
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
    let updates = home.join(".cache/noobsbot-desktop/updates");
    let (_, dir) = fs::read_dir(&updates)
        .map_err(|e| format!("{}: {e}", updates.display()))?
        .flatten()
        .filter_map(|e| Some((Version::parse(e.file_name().to_str()?).ok()?, e.path())))
        .max_by(|a, b| a.0.cmp(&b.0))
        .ok_or("no download")?;
    install(&dir)
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
fn revert(version: &str) -> Result<Version, String> {
    let root = root();
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
    for name in ["noobsbot-desktop", "noobsbot-desktop-settings"] {
        if target.join(name).exists() {
            relink(&target.join(name), &at(&format!("/usr/bin/{name}")))?;
        }
    }
    Ok(version)
}

fn install(dir: &Path) -> Result<Version, String> {
    let root = root();
    let at = |path: &str| root.join(path.trim_start_matches('/'));

    let key = fs::read_to_string(at(verify::PUBKEY)).map_err(|e| format!("{}: {e}", verify::PUBKEY))?;
    let sums = small(&dir.join("SHA256SUMS"))?;
    let sig = String::from_utf8(small(&dir.join("SHA256SUMS.minisig"))?).map_err(|_| "the signature is not text")?;
    let signed = verify::signed(&key, &sums, &sig, std::env::consts::ARCH)?;

    let lib = at("/usr/lib/noobsbot-desktop");
    let versions = lib.join("versions");
    let bin = at("/usr/bin/noobsbot-desktop");
    let current = installed(&bin, &versions)?;
    if signed.version <= current {
        return Err(format!("{} is not newer than the installed {current}", signed.version));
    }

    // Root-only staging, emptied first: a leftover from an interrupted run is never reused.
    let stage = versions.join(format!(".{}.new", signed.version));
    let _ = fs::remove_dir_all(&stage);
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&stage).map_err(|e| format!("{}: {e}", stage.display()))?;
    let result = unpack(dir, &signed, &stage, &versions);
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
fn unpack(dir: &Path, signed: &verify::Signed, stage: &Path, versions: &Path) -> Result<PathBuf, String> {
    let copy = stage.join("release.tar.zst");
    let mut out = File::create_new(&copy).map_err(|e| format!("{}: {e}", copy.display()))?;
    let tarball = open(&dir.join(&signed.tarball)).map_err(|e| format!("{}: {e}", signed.tarball))?;
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

/// Points `link` at `to` atomically.
fn relink(to: &Path, link: &Path) -> Result<(), String> {
    let new = link.with_extension("update-new");
    let _ = fs::remove_file(&new);
    symlink(to, &new).and_then(|_| fs::rename(&new, link)).map_err(|e| format!("{}: {e}", link.display()))
}

/// A small file (the sums or the signature), at most 64 KiB.
fn small(path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    open(path).and_then(|f| f.take(64 * 1024).read_to_end(&mut bytes)).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(bytes)
}

/// A file of the user's download, opened without waiting on a FIFO put there (its reads then fail at once).
fn open(path: &Path) -> std::io::Result<File> {
    fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path)
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
}
