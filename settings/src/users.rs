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
    Users { me, others }
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

/// "Change Picture…": its path field, as a text key; open while the key is there.
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
    // As the lock screen looks: AccountsService's copy, else `~/.face`.
    let files = [PathBuf::from(text(bus, path, "IconFile").await), PathBuf::from("/var/lib/AccountsService/icons").join(&login)];
    let face = PathBuf::from(text(bus, path, "HomeDirectory").await).join(".face");
    let picture = files.iter().chain([&face]).find_map(|f| picture(f));
    Some(Account { path: path.to_owned(), login, name, picture })
}

/// A square picture (shown as a circle), or nothing if `file` isn't a PNG or JPEG.
fn picture(file: &Path) -> Option<Handle> {
    Some(Handle::from_rgba(PICTURE, PICTURE, crate::gallery::cover(file, PICTURE, PICTURE)?))
}

#[cfg(test)]
mod tests {
    use super::*;

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
