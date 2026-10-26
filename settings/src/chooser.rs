//! The file chooser: the XDG desktop portal's `FileChooser.OpenFile`, called on the session bus. A path field stands
//! in when the portal is missing or fails.

use std::{collections::HashMap, os::unix::ffi::OsStringExt, path::PathBuf};

use futures_lite::StreamExt;
use zbus::zvariant::{OwnedValue, Value};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

/// A `filters` entry: its name and `(0, glob)` or `(1, MIME type)` patterns.
pub type Filter = (&'static str, &'static [(u32, &'static str)]);
pub const IMAGES: Filter = ("Images", &[(1, "image/png"), (1, "image/jpeg")]);
pub const WIREGUARD: Filter = ("WireGuard configuration", &[(0, "*.conf")]);

/// What the chooser answered: `Ok(Some(path))` chosen, `Ok(None)` cancelled, `Err(())` no portal (or it failed), so
/// the path field shows instead.
pub type Chosen = Result<Option<PathBuf>, ()>;

/// Opens the chooser titled `title` (a folder one when `directory`) and waits for the choice.
pub async fn open(title: &'static str, directory: bool, filter: Option<Filter>) -> Chosen {
    let answer = async {
        let bus = zbus::Connection::session().await?;
        // The Request object's path is known before the call (`handle_token`), so its Response can't be missed.
        let sender = bus.unique_name().map(|n| n.trim_start_matches(':').replace('.', "_")).unwrap_or_default();
        let token = format!("noobsbot{}", std::process::id());
        let request = format!("{PORTAL_PATH}/request/{sender}/{token}");
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface("org.freedesktop.portal.Request")?
            .member("Response")?
            .path(request)?
            .build();
        let mut responses = zbus::MessageStream::for_match_rule(rule, &bus, None).await?;
        bus.call_method(Some(PORTAL), PORTAL_PATH, Some("org.freedesktop.portal.FileChooser"), "OpenFile", &("", title, options(&token, directory, filter)))
            .await?;
        let response = responses.next().await.ok_or(zbus::Error::Failure("no Response".into()))??;
        let (code, results): (u32, HashMap<String, OwnedValue>) = response.body().deserialize()?;
        zbus::Result::Ok((code, results.get("uris").and_then(|v| Vec::<String>::try_from(v.try_clone().ok()?).ok())))
    };
    match answer.await {
        Ok((0, uris)) => uris.and_then(|u| u.first().and_then(|u| path(u))).map(Some).ok_or(()),
        // 1: cancelled; 2: closed some other way.
        Ok((1 | 2, _)) => Ok(None),
        _ => Err(()),
    }
}

/// `OpenFile`'s options: the token naming the Request, a folder chooser or not, and the one filter.
fn options(token: &str, directory: bool, filter: Option<Filter>) -> HashMap<&'static str, Value<'_>> {
    let mut options = HashMap::from([("handle_token", Value::from(token)), ("modal", true.into()), ("directory", directory.into())]);
    if let Some((name, patterns)) = filter {
        options.insert("filters", vec![(name, patterns.to_vec())].into());
    }
    options
}

/// A `file://` URI's path, percent-decoded; nothing for other schemes, other hosts or bad escapes.
fn path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let mut bytes = Vec::with_capacity(rest.len());
    let mut it = rest.bytes();
    while let Some(b) = it.next() {
        bytes.push(match b {
            b'%' => {
                let hex = [it.next()?, it.next()?];
                u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?
            }
            b => b,
        });
    }
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_file_uris() {
        assert_eq!(path("file:///home/me/My%20Pictures/a%20b.png"), Some("/home/me/My Pictures/a b.png".into()));
        assert_eq!(path("file:///home/me/%C3%A9t%C3%A9/%E5%86%99%E7%9C%9F.jpg"), Some("/home/me/été/写真.jpg".into()));
        assert_eq!(path("file://localhost/etc/wg0.conf"), Some("/etc/wg0.conf".into()));
        // Unescaped unicode passes through.
        assert_eq!(path("file:///tmp/ü.conf"), Some("/tmp/ü.conf".into()));
        for bad in ["https://example.com/a.png", "smb://host/share/a.png", "file://otherhost/a", "/plain/path", "file:///a%2", "file:///a%zz"] {
            assert_eq!(path(bad), None, "{bad}");
        }
    }

    #[test]
    fn builds_options() {
        let o = options("t1", true, None);
        assert_eq!(o["handle_token"], Value::from("t1"));
        assert_eq!(o["directory"], Value::from(true));
        assert!(!o.contains_key("filters"));
        let o = options("t2", false, Some(WIREGUARD));
        assert_eq!(o["directory"], Value::from(false));
        assert_eq!(o["filters"].value_signature().to_string(), "a(sa(us))");
        assert_eq!(o["filters"], Value::from(vec![("WireGuard configuration", vec![(0u32, "*.conf")])]));
    }
}
