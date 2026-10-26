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

#[derive(Clone, Debug)]
pub struct Account {
    /// AccountsService's object path.
    pub path: String,
    pub login: String,
    /// The full name, else the login.
    pub name: String,
    pub picture: Option<Handle>,
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
    others.sort_by_key(|a| a.name.to_lowercase());
    let me = match me {
        Some(path) => account(&bus, &path).await,
        None => None,
    };
    Users { me, others }
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
    let name = Some(text(bus, path, "RealName").await).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| login.clone());
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
