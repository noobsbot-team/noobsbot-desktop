//! `noobsbot-desktop-update <download dir>` (E6.13): the privileged half of Software Update, run as root through
//! `pkexec` (polkit action `org.noobsbot.desktop.update`). The dir is the updater's download (`<tarball>`,
//! `SHA256SUMS`, `SHA256SUMS.minisig`) and is owned by the user, so nothing in it is trusted: the signature is checked
//! again with the root-owned release key, the tarball is copied into a root-only staging dir while hashing (so it
//! can't be swapped after the check), the release must be newer than the installed one (no downgrade or replay), and
//! only then is it unpacked to `/usr/lib/noobsbot-desktop/versions/<v>` and `/usr/bin/noobsbot-desktop` switched to it.
//! The previous versions stay for the session wrapper's rollback.
//!
//! Testing: `NOOBSBOT_UPDATE_ROOT=<dir>` puts every system path under `<dir>`; it is ignored when running as root.

use std::{
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use semver::Version;

#[path = "../services/verify.rs"]
mod verify;

fn main() -> ExitCode {
    match install() {
        Ok(version) => {
            println!("Noobsbot Desktop {version} is installed; it starts at the next login.");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("noobsbot-desktop-update: {err}; nothing was changed.");
            ExitCode::FAILURE
        }
    }
}

fn install() -> Result<Version, String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let [dir] = &args[..] else { return Err("usage: noobsbot-desktop-update <download dir>".into()) };
    let dir = Path::new(dir);
    // SAFETY: geteuid has no preconditions.
    let root = match std::env::var_os("NOOBSBOT_UPDATE_ROOT") {
        Some(fake) if unsafe { libc::geteuid() } != 0 => PathBuf::from(fake),
        _ => PathBuf::from("/"),
    };
    let at = |path: &str| root.join(path.trim_start_matches('/'));

    let key = fs::read_to_string(at(verify::PUBKEY)).map_err(|e| format!("{}: {e}", verify::PUBKEY))?;
    let sums = small(&dir.join("SHA256SUMS"))?;
    let sig = String::from_utf8(small(&dir.join("SHA256SUMS.minisig"))?).map_err(|_| "the signature is not text")?;
    let signed = verify::signed(&key, &sums, &sig)?;

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
    let tarball = File::open(dir.join(&signed.tarball)).map_err(|e| format!("{}: {e}", signed.tarball))?;
    let sha256 = verify::sha256(tarball, |chunk| out.write_all(chunk)).map_err(|e| format!("{}: {e}", signed.tarball))?;
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
    File::open(path).and_then(|f| f.take(64 * 1024).read_to_end(&mut bytes)).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(bytes)
}
