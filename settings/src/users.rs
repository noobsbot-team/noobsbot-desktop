//! Users (E8.2): the accounts from AccountsService (`org.freedesktop.Accounts`) on the system bus, read when the
//! page opens.

use std::path::{Path, PathBuf};

use iced::widget::image::Handle;
use zbus::{Connection, zvariant::OwnedObjectPath};

const ACCOUNTS: &str = "org.freedesktop.Accounts";
const ACCOUNTS_PATH: &str = "/org/freedesktop/Accounts";
const USER: &str = "org.freedesktop.Accounts.User";
/// Pictures are decoded at this many px a side (shown at 64 and 32).
const PICTURE: u32 = 128;
/// A new picture is saved at this many px a side: AccountsService refuses files over 1 MB.
const FACE: u32 = 256;

#[derive(Clone, Debug)]
pub struct Account {
    /// AccountsService's object path.
    pub path: String,
    pub login: String,
    pub uid: u64,
    /// The full name; may be empty.
    pub name: String,
    pub picture: Option<Handle>,
}

impl Account {
    /// The full name, else the login.
    pub fn shown(&self) -> &str {
        if self.name.trim().is_empty() { &self.login } else { &self.name }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Users {
    /// `None`: AccountsService isn't running.
    pub me: Option<Account>,
    /// The other people's accounts (no system accounts), by name.
    pub others: Vec<Account>,
    /// The camera "Take Photo…" uses; `None` without one or without `ffmpeg`.
    pub camera: Option<PathBuf>,
    /// How many fingers fprintd has for the user; `None` without fprintd or a reader.
    pub fingers: Option<usize>,
}

pub async fn read() -> Users {
    let Ok(bus) = crate::system::system(2).await else { return Users::default() };
    let login = std::env::var("USER").unwrap_or_default();
    let me = bus.call_method(Some(ACCOUNTS), ACCOUNTS_PATH, Some(ACCOUNTS), "FindUserByName", &(login.as_str(),)).await;
    let me = me.ok().and_then(|m| m.body().deserialize::<OwnedObjectPath>().ok());
    let cached = bus.call_method(Some(ACCOUNTS), ACCOUNTS_PATH, Some(ACCOUNTS), "ListCachedUsers", &()).await;
    let mut others = Vec::new();
    for path in cached.ok().and_then(|m| m.body().deserialize::<Vec<OwnedObjectPath>>().ok()).unwrap_or_default() {
        if Some(&path) != me.as_ref()
            && let Some(account) = account(&bus, &path).await
        {
            others.push(account);
        }
    }
    others.sort_by_key(|a| a.shown().to_lowercase());
    let me = match me {
        Some(path) => account(&bus, &path).await,
        None => None,
    };
    let fingers = fingers(&login).await.map(|f| f.len());
    Users { me, others, camera: camera(), fingers }
}

/// The full name field, as a text key.
pub const NAME: &str = "users.name";

/// Calls `method` (`SetRealName`, `SetIconFile`) on the account at `path`, then the accounts again and what went
/// wrong. Interactive: polkit may ask for the password through the desktop's agent.
pub async fn set(path: String, method: &'static str, value: String) -> (Users, Option<String>) {
    let result = async { crate::system::system(120).await?.call_method(Some(ACCOUNTS), path.as_str(), Some(USER), method, &(value.as_str(),)).await };
    let what = if method == "SetRealName" { "name" } else { "picture" };
    let error = result.await.err().map(|err| format!("Couldn't change the {what}: {err}"));
    (read().await, error)
}

/// "Change Picture…": its pictures, as a text key; open while the key is there.
pub const PICTURES: &str = "users.pictures";
/// Its path field, shown when there is no file chooser.
pub const PICTURE_FILE: &str = "users.picture";

/// `file` becomes the account's picture: cut square into `~/.face`, which AccountsService then copies.
pub async fn set_picture(path: String, file: PathBuf) -> (Users, Option<String>) {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    match face(&file, &home) {
        Some(face) => set(path, "SetIconFile", face.to_string_lossy().into_owned()).await,
        None => (read().await, Some(format!("Couldn't use {} as a picture: it must be a PNG or JPEG", file.display()))),
    }
}

/// `file` as a `FACE` px square PNG at `home/.face`.
fn face(file: &Path, home: &Path) -> Option<PathBuf> {
    let rgba = crate::gallery::cover(file, FACE, FACE)?;
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, FACE, FACE);
    encoder.set_color(png::ColorType::Rgba);
    encoder.write_header().and_then(|mut w| w.write_image_data(&rgba)).ok()?;
    let path = home.join(".face");
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}

/// "Take Photo…"'s sheet: counting down, taking, the photo taken, or why it failed.
#[derive(Clone, Debug)]
pub enum Shot {
    Count(u8),
    Taking,
    Taken(Handle),
    Failed(String),
}

/// The camera: `/dev/videoN` of the first capture node (sysfs `index` 0; 1 is a camera's metadata node) that isn't
/// an infrared one; `None` too when `ffmpeg` isn't on the PATH.
fn camera() -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).find(|dir| dir.join("ffmpeg").is_file())?;
    let nodes = std::fs::read_dir("/sys/class/video4linux").ok()?.flatten().filter_map(|e| {
        let dir = e.path();
        let n = e.file_name().to_str()?.strip_prefix("video")?.parse().ok()?;
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap_or_default();
        Some((n, read("index").trim().to_owned(), read("name")))
    });
    pick(nodes.collect()).map(|n| PathBuf::from(format!("/dev/video{n}")))
}

/// The lowest N of `(N, index, name)` with index 0 and no "IR" word in its name.
fn pick(mut nodes: Vec<(u32, String, String)>) -> Option<u32> {
    nodes.sort();
    nodes.into_iter().find(|(_, index, name)| index == "0" && !name.split_whitespace().any(|w| w == "IR")).map(|(n, ..)| n)
}

/// Where a photo is kept until "Use Photo".
pub fn photo_file() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from).join("noobsbot-settings-photo.jpg")
}

/// One frame from `camera` into `photo_file()`, shown square; why not otherwise (no camera, busy).
/// ponytail: one still through `ffmpeg`, no live preview; a v4l2 stream of our own if one is wanted.
pub async fn take_photo(camera: PathBuf) -> Result<Handle, String> {
    let file = photo_file();
    let _ = std::fs::remove_file(&file);
    let out = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-f", "v4l2", "-video_size", "640x480", "-i"])
        .arg(&camera)
        // A second in: a camera's first frames are dark while its exposure settles.
        .args(["-ss", "1", "-frames:v", "1", "-y"])
        .arg(&file)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|err| format!("Couldn't run ffmpeg: {err}"))?;
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr);
        let why = if said.contains("busy") { "the camera is in use".to_owned() } else { said.lines().last().unwrap_or("no frame").trim().to_owned() };
        return Err(format!("Couldn't take a photo: {why}"));
    }
    picture(&file).ok_or_else(|| "Couldn't take a photo: no frame".to_owned())
}

/// Change Password…'s fields (current, new, verify), as text keys; open while they are there.
pub const PASSWORD: [&str; 3] = ["users.password.old", "users.password.new", "users.password.verify"];

/// What is wrong with the Change Password fields, if anything.
pub fn check(old: &str, new: &str, verify: &str) -> Result<(), &'static str> {
    if old.is_empty() {
        Err("Enter your current password")
    } else if new.is_empty() {
        Err("Enter a new password")
    } else if new != verify {
        Err("The new passwords don't match")
    } else if new == old {
        Err("The new password is the same as the current one")
    } else if [old, new].iter().any(|p| p.contains(['\n', '\r'])) {
        Err("A password can't have a line break")
    } else {
        Ok(())
    }
}

/// Changes the user's own password with `passwd`, the current one and the new one twice fed through a pipe (not
/// AccountsService's `SetPassword`, which takes a hash and an administrator); what went wrong, if anything. The
/// passwords are wiped once written.
/// ponytail: iced's fields and messages hold copies we can't wipe; they go when the form closes.
pub async fn change_password(old: String, new: String) -> Option<String> {
    // Sized up front: no reallocation leaves a copy behind.
    let mut input = Vec::with_capacity(old.len() + 2 * new.len() + 3);
    for p in [&old, &new, &new] {
        input.extend_from_slice(p.as_bytes());
        input.push(b'\n');
    }
    wipe(old);
    wipe(new);
    let result = passwd(&input);
    input.fill(0);
    std::hint::black_box(&input);
    result.err()
}

/// Zeroes a password before it is dropped.
pub fn wipe(password: String) {
    let mut bytes = password.into_bytes();
    bytes.fill(0);
    std::hint::black_box(&bytes);
}

fn passwd(input: &[u8]) -> Result<(), String> {
    use std::{io::Write, process::Stdio};
    let mut child = std::process::Command::new("passwd")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("Couldn't run passwd: {err}"))?;
    // It may stop reading early (a wrong password); the exit status tells.
    let _ = child.stdin.take().expect("piped").write_all(input);
    let out = child.wait_with_output().map_err(|err| format!("Couldn't run passwd: {err}"))?;
    if out.status.success() { Ok(()) } else { Err(passwd_error(&String::from_utf8_lossy(&out.stderr))) }
}

/// passwd's complaint in words: its prompts (`Current password: `) dropped, the usual ones put plainly.
fn passwd_error(stderr: &str) -> String {
    let said: Vec<&str> = stderr.lines().filter_map(|l| l.rsplit("password: ").next()).map(str::trim).filter(|l| !l.is_empty()).collect();
    if let Some(bad) = said.iter().find_map(|l| l.strip_prefix("BAD PASSWORD: ")) {
        return format!("The new password isn't allowed: {bad}");
    }
    if said.iter().any(|l| l.ends_with("Authentication token manipulation error") || l.ends_with("Authentication failure")) {
        return "The current password isn't right".into();
    }
    match said.iter().find_map(|l| l.strip_prefix("passwd: ")).filter(|r| *r != "password unchanged") {
        Some(reason) => format!("Couldn't change the password: {reason}"),
        None => "Couldn't change the password".into(),
    }
}

/// Add User…'s fields (full name, account name, password, verify, `admin` when set), as text keys; open while the
/// first is there.
pub const NEW: [&str; 5] = ["users.new.name", "users.new.login", "users.new.password", "users.new.verify", "users.new.admin"];

/// Delete User…'s alert: the account's path, as a text key; open while it is there.
pub const DELETE: &str = "users.delete";

/// An account name from a full name: its ASCII letters lower-cased, digits, `-` and `_` (none of these first), at
/// most 32 characters. ponytail: accented letters are dropped, not transliterated.
pub fn suggest(full: &str) -> String {
    let mut login = String::new();
    for c in full.chars().map(|c| c.to_ascii_lowercase()) {
        if c.is_ascii_lowercase() || c == '_' || (!login.is_empty() && (c.is_ascii_digit() || c == '-')) {
            login.push(c);
        }
    }
    login.truncate(32);
    login
}

/// What is wrong with Add User's fields, if anything; `passwd` is `/etc/passwd`, whose names are taken.
pub fn check_new(login: &str, password: &str, verify: &str, passwd: &str) -> Result<(), &'static str> {
    let mut chars = login.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        && login.len() <= 32;
    if login.is_empty() {
        Err("Enter an account name")
    } else if !valid {
        Err("An account name has lower-case letters, digits, - and _, starts with a letter, and is at most 32 long")
    } else if passwd.lines().any(|l| l.split(':').next() == Some(login)) {
        Err("That account name is taken")
    } else if password.is_empty() {
        Err("Enter a password")
    } else if password != verify {
        Err("The passwords don't match")
    } else if password.contains(['\n', '\r']) {
        Err("A password can't have a line break")
    } else {
        Ok(())
    }
}

/// AccountsService's `CreateUser` arguments: name, full name, account type (0 standard, 1 administrator).
fn create_args<'a>(login: &'a str, full: &'a str, admin: bool) -> (&'a str, &'a str, i32) {
    (login, full, i32::from(admin))
}

/// AccountsService's `DeleteUser` arguments: the uid, and whether the home folder goes too.
fn delete_args(uid: u64, remove_files: bool) -> (i64, bool) {
    (uid as i64, remove_files)
}

/// The words of a D-Bus error (a polkit denial says "Not authorized").
fn reason(err: zbus::Error) -> String {
    match err {
        zbus::Error::MethodError(_, Some(words), _) => words,
        err => err.to_string(),
    }
}

/// Adds an account (`CreateUser`, then `SetPassword` with a SHA-512 crypt hash from `openssl passwd`), then the
/// accounts again and what went wrong. Interactive: polkit asks for an administrator through the desktop's agent.
pub async fn create(full: String, login: String, admin: bool, password: String) -> (Users, Option<String>) {
    let hash = crypt(&password);
    wipe(password);
    let error = async {
        let hash = hash?;
        let bus = crate::system::system(120).await.map_err(reason)?;
        let added = bus.call_method(Some(ACCOUNTS), ACCOUNTS_PATH, Some(ACCOUNTS), "CreateUser", &create_args(&login, &full, admin)).await;
        let added = added.map_err(|err| format!("Couldn't add {login}: {}", reason(err)))?;
        let path: OwnedObjectPath = added.body().deserialize().map_err(|err| format!("Couldn't add {login}: {err}"))?;
        let set = bus.call_method(Some(ACCOUNTS), path.as_str(), Some(USER), "SetPassword", &(hash.as_str(), "")).await;
        set.map_err(|err| format!("Added {login}, but couldn't set the password: {}", reason(err))).map(drop)
    };
    let error = error.await.err();
    (read().await, error)
}

/// Removes an account (`DeleteUser`), its home folder too with `remove_files`; then the accounts again and what went
/// wrong. Interactive like `create`.
pub async fn delete(login: String, uid: u64, remove_files: bool) -> (Users, Option<String>) {
    let error = async {
        let bus = crate::system::system(120).await?;
        bus.call_method(Some(ACCOUNTS), ACCOUNTS_PATH, Some(ACCOUNTS), "DeleteUser", &delete_args(uid, remove_files)).await
    };
    let error = error.await.err().map(|err| format!("Couldn't delete {login}: {}", reason(err)));
    (read().await, error)
}

/// `password`'s SHA-512 crypt hash, which `SetPassword` takes; the password goes through a pipe.
fn crypt(password: &str) -> Result<String, String> {
    use std::{io::Write, process::Stdio};
    let failed = |err: &dyn std::fmt::Display| format!("Couldn't run openssl for the password: {err}");
    let mut child = std::process::Command::new("openssl")
        .args(["passwd", "-6", "-stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| failed(&err))?;
    let mut input = Vec::with_capacity(password.len() + 1);
    input.extend_from_slice(password.as_bytes());
    input.push(b'\n');
    let written = child.stdin.take().expect("piped").write_all(&input);
    input.fill(0);
    std::hint::black_box(&input);
    written.map_err(|err| failed(&err))?;
    let out = child.wait_with_output().map_err(|err| failed(&err))?;
    let hash = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if out.status.success() && hash.starts_with("$6$") { Ok(hash) } else { Err(failed(&"no hash")) }
}

const FPRINT: &str = "net.reactivated.Fprint";
const FPRINT_DEVICE: &str = "net.reactivated.Fprint.Device";
/// fprintd's names for the fingers, in the order Add Fingerprint… takes them.
const FINGERS: [&str; 10] = [
    "right-index-finger",
    "left-index-finger",
    "right-thumb",
    "left-thumb",
    "right-middle-finger",
    "left-middle-finger",
    "right-ring-finger",
    "left-ring-finger",
    "right-little-finger",
    "left-little-finger",
];

/// fprintd's default reader, on a connection whose calls wait `timeout` s (fprintd may be started by the first; polkit
/// may ask for the password); `None` without fprintd or a reader.
async fn reader(timeout: u64) -> Option<zbus::Proxy<'static>> {
    let bus = crate::system::system(timeout).await.ok()?;
    let manager = "net.reactivated.Fprint.Manager";
    let reply = bus.call_method(Some(FPRINT), "/net/reactivated/Fprint/Manager", Some(manager), "GetDefaultDevice", &()).await.ok()?;
    let path: OwnedObjectPath = reply.body().deserialize().ok()?;
    zbus::Proxy::new(&bus, FPRINT, path, FPRINT_DEVICE).await.ok()
}

/// `login`'s enrolled fingers, by fprintd's names; `None` without a reader.
async fn fingers(login: &str) -> Option<Vec<String>> {
    let device = reader(25).await?;
    // None enrolled is an error (NoEnrolledPrints).
    Some(device.call("ListEnrolledFingers", &(login,)).await.unwrap_or_default())
}

/// Users ▸ Fingerprints ▸ Delete All; then the accounts again and what went wrong. Interactive: polkit asks for the
/// password through the desktop's agent.
pub async fn delete_fingers() -> (Users, Option<String>) {
    let login = std::env::var("USER").unwrap_or_default();
    let error = async {
        let device = reader(120).await.ok_or("there's no fingerprint reader")?;
        device.call_method("Claim", &(login.as_str(),)).await.map_err(reason)?;
        let deleted = device.call_method("DeleteEnrolledFingers2", &()).await.map_err(reason);
        let _ = device.call_method("Release", &()).await;
        deleted.map(drop)
    };
    let error = error.await.err().map(|err: String| format!("Couldn't delete the fingerprints: {err}"));
    (read().await, error)
}

/// Add Fingerprint…'s sheet.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Enrolling {
    /// Touches that counted.
    pub touches: u32,
    /// How many it takes (fprintd's `num-enroll-stages`); 0 while unknown.
    pub stages: u32,
    /// A touch that didn't count, or why it failed.
    pub note: Option<String>,
    /// fprintd is done: the finger was added, unless there is a note.
    pub done: bool,
}

/// One `EnrollStatus` on the sheet.
pub fn enroll_step(e: &mut Enrolling, result: &str, done: bool) {
    e.done = done;
    e.note = match result {
        "enroll-stage-passed" => {
            e.touches += 1;
            None
        }
        "enroll-completed" => {
            e.touches = e.touches.max(e.stages);
            None
        }
        "enroll-retry-scan" | "enroll-swipe-too-short" => Some("That touch didn't take: touch again"),
        "enroll-finger-not-centered" => Some("Centre your finger on the sensor"),
        "enroll-remove-and-retry" => Some("Lift your finger, then touch again"),
        "enroll-duplicate" => Some("That finger is already enrolled"),
        "enroll-data-full" => Some("The sensor has no room for another finger"),
        "enroll-disconnected" => Some("The sensor was disconnected"),
        // "enroll-failed", "enroll-unknown-error".
        _ => Some("Couldn't add the fingerprint"),
    }
    .map(String::from);
}

/// Add Fingerprint…: enrolls the first finger fprintd doesn't have yet, a sheet state after each touch. Dropping the
/// stream (the sheet closed) drops its connection, and fprintd stops and releases the reader by itself.
pub fn enroll() -> impl iced::futures::Stream<Item = Enrolling> {
    iced::stream::channel(4, async |mut tx: iced::futures::channel::mpsc::Sender<Enrolling>| {
        use iced::futures::{SinkExt, StreamExt};
        let mut state = Enrolling::default();
        let enrolled = async {
            let login = std::env::var("USER").unwrap_or_default();
            let device = reader(120).await.ok_or("there's no fingerprint reader")?;
            let have: Vec<String> = device.call("ListEnrolledFingers", &(login.as_str(),)).await.unwrap_or_default();
            let finger = FINGERS.into_iter().find(|f| !have.iter().any(|h| h == f)).ok_or("all ten fingers are enrolled")?;
            device.call_method("Claim", &(login.as_str(),)).await.map_err(reason)?;
            let stages = crate::system::property(device.connection(), FPRINT, device.path().as_str(), FPRINT_DEVICE, "num-enroll-stages").await;
            state.stages = stages.ok().and_then(|v| i32::try_from(v).ok()).map_or(0, |n| n.max(0) as u32);
            let mut statuses = device.receive_signal("EnrollStatus").await.map_err(reason)?;
            device.call_method("EnrollStart", &(finger,)).await.map_err(reason)?;
            let _ = tx.send(state.clone()).await;
            while let Some(message) = statuses.next().await {
                let (result, done): (String, bool) = message.body().deserialize().map_err(|err| err.to_string())?;
                enroll_step(&mut state, &result, done);
                let _ = tx.send(state.clone()).await;
                if done {
                    break;
                }
            }
            let _ = device.call_method("EnrollStop", &()).await;
            let _ = device.call_method("Release", &()).await;
            Ok::<(), String>(())
        };
        if let Err(err) = enrolled.await {
            state.note = Some(format!("Couldn't add the fingerprint: {err}"));
            state.done = true;
            let _ = tx.send(state).await;
        }
        std::future::pending::<()>().await;
    })
}

async fn text(bus: &Connection, path: &str, name: &str) -> String {
    crate::system::property(bus, ACCOUNTS, path, USER, name).await.ok().and_then(|v| String::try_from(v).ok()).unwrap_or_default()
}

async fn account(bus: &Connection, path: &OwnedObjectPath) -> Option<Account> {
    let path = path.as_str();
    let login = text(bus, path, "UserName").await;
    if login.is_empty() {
        return None;
    }
    let name = text(bus, path, "RealName").await;
    let uid = crate::system::property(bus, ACCOUNTS, path, USER, "Uid").await.ok().and_then(|v| u64::try_from(v).ok()).unwrap_or_default();
    // As the lock screen looks: AccountsService's copy, else `~/.face`.
    let files = [PathBuf::from(text(bus, path, "IconFile").await), PathBuf::from("/var/lib/AccountsService/icons").join(&login)];
    let face = PathBuf::from(text(bus, path, "HomeDirectory").await).join(".face");
    let picture = files.iter().chain([&face]).find_map(|f| picture(f));
    Some(Account { path: path.to_owned(), login, uid, name, picture })
}

/// A square picture (shown as a circle), or nothing if `file` isn't a PNG or JPEG.
fn picture(file: &Path) -> Option<Handle> {
    Some(Handle::from_rgba(PICTURE, PICTURE, crate::gallery::cover(file, PICTURE, PICTURE)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enroll_counts_touches() {
        let mut e = Enrolling { stages: 3, ..Default::default() };
        enroll_step(&mut e, "enroll-stage-passed", false);
        enroll_step(&mut e, "enroll-finger-not-centered", false);
        assert_eq!((e.touches, e.note.as_deref(), e.done), (1, Some("Centre your finger on the sensor"), false));
        enroll_step(&mut e, "enroll-stage-passed", false);
        assert_eq!(e.note, None, "a good touch clears the note");
        enroll_step(&mut e, "enroll-completed", true);
        assert_eq!((e.touches, e.note, e.done), (3, None, true), "added");
        let mut e = Enrolling::default();
        enroll_step(&mut e, "enroll-duplicate", true);
        assert!(e.done && e.note.as_deref() == Some("That finger is already enrolled"), "failed");
        enroll_step(&mut e, "enroll-failed", true);
        assert_eq!(e.note.as_deref(), Some("Couldn't add the fingerprint"));
    }

    #[test]
    fn camera_picked() {
        let node = |n, index: &str, name: &str| (n, index.to_owned(), name.to_owned());
        let hp = vec![
            node(3, "1", "HP 5MP Camera: HP IR Camera\n"),
            node(1, "1", "HP 5MP Camera: HP 5MP Camera\n"),
            node(2, "0", "HP 5MP Camera: HP IR Camera\n"),
            node(0, "0", "HP 5MP Camera: HP 5MP Camera\n"),
        ];
        assert_eq!(pick(hp), Some(0));
        assert_eq!(pick(vec![node(0, "0", "Integrated IR Camera"), node(4, "0", "USB Camera")]), Some(4));
        assert_eq!(pick(vec![node(0, "1", "meta")]), None);
        assert_eq!(pick(Vec::new()), None);
    }

    #[test]
    fn password_form_checks() {
        assert_eq!(check("", "new", "new"), Err("Enter your current password"));
        assert_eq!(check("old", "", ""), Err("Enter a new password"));
        assert_eq!(check("old", "new", "nwe"), Err("The new passwords don't match"));
        assert_eq!(check("same", "same", "same"), Err("The new password is the same as the current one"));
        assert_eq!(check("old", "a\nb", "a\nb"), Err("A password can't have a line break"));
        assert_eq!(check("old", "new", "new"), Ok(()));
    }

    #[test]
    fn passwd_errors_in_words() {
        let wrong = "Changing password for me.\nCurrent password: passwd: Authentication token manipulation error\npasswd: password unchanged\n";
        assert_eq!(passwd_error(wrong), "The current password isn't right");
        let short = "Changing password for me.\nCurrent password: New password: BAD PASSWORD: The password is shorter than 8 characters\nNew password: \npasswd: Have exhausted maximum number of retries for service\npasswd: password unchanged\n";
        assert_eq!(passwd_error(short), "The new password isn't allowed: The password is shorter than 8 characters");
        assert_eq!(
            passwd_error("passwd: Have exhausted maximum number of retries for service\npasswd: password unchanged\n"),
            "Couldn't change the password: Have exhausted maximum number of retries for service"
        );
        assert_eq!(passwd_error(""), "Couldn't change the password");
    }

    #[test]
    fn account_name_suggested() {
        assert_eq!(suggest("Anil Shrestha"), "anilshrestha");
        assert_eq!(suggest("  2nd-José_O'Neil 3 "), "nd-jos_oneil3");
        assert_eq!(suggest("日本"), "");
        assert_eq!(suggest(&"a".repeat(40)).len(), 32);
    }

    #[test]
    fn new_user_checks() {
        let passwd = "root:x:0:0::/root:/bin/bash\nanil:x:1000:1000::/home/anil:/bin/zsh\n";
        assert_eq!(check_new("", "pw", "pw", passwd), Err("Enter an account name"));
        for bad in ["1abc", "-abc", "Abc", "a b", "josé", &"a".repeat(33)] {
            assert!(check_new(bad, "pw", "pw", passwd).is_err_and(|e| e.starts_with("An account name has")), "{bad}");
        }
        assert_eq!(check_new("anil", "pw", "pw", passwd), Err("That account name is taken"));
        assert_eq!(check_new("ani", "", "", passwd), Err("Enter a password"));
        assert_eq!(check_new("ani", "pw", "wp", passwd), Err("The passwords don't match"));
        assert_eq!(check_new("ani", "p\nw", "p\nw", passwd), Err("A password can't have a line break"));
        assert_eq!(check_new("_ani-2", "pw", "pw", passwd), Ok(()));
        assert_eq!(check_new(&"a".repeat(32), "pw", "pw", passwd), Ok(()));
    }

    #[test]
    fn dbus_arguments() {
        assert_eq!(create_args("ani", "Ani S", false), ("ani", "Ani S", 0));
        assert_eq!(create_args("ani", "Ani S", true), ("ani", "Ani S", 1));
        assert_eq!(delete_args(1001, true), (1001, true));
        assert_eq!(delete_args(1001, false), (1001, false));
        // The signatures AccountsService declares: CreateUser(ssi), DeleteUser(xb).
        use zbus::zvariant::Type;
        assert_eq!(<(&str, &str, i32)>::SIGNATURE.to_string(), "(ssi)");
        assert_eq!(<(i64, bool)>::SIGNATURE.to_string(), "(xb)");
    }

    #[test]
    fn crypt_hashes() {
        // Needs `openssl`, as the live call does.
        let hash = crypt("hunter22").unwrap();
        assert!(hash.starts_with("$6$") && !hash.contains("hunter22"), "{hash}");
    }

    #[test]
    fn face_is_a_square_png() {
        let dir = std::env::temp_dir().join(format!("noobsbot-settings-face-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wide = dir.join("wide.png");
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, 4, 2);
        encoder.set_color(png::ColorType::Rgb);
        encoder.write_header().unwrap().write_image_data(&[200; 4 * 2 * 3]).unwrap();
        std::fs::write(&wide, bytes).unwrap();
        let face = face(&wide, &dir).unwrap();
        assert_eq!(face, dir.join(".face"));
        let decoder = png::Decoder::new(std::io::Cursor::new(std::fs::read(&face).unwrap()));
        let info = decoder.read_info().unwrap();
        assert_eq!((info.info().width, info.info().height), (FACE, FACE));
        assert_eq!(super::face(&dir.join("missing.jpg"), &dir), None);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
