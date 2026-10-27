//! Now Playing (E2.3) over MPRIS: every media app (Helium, Spotify, mpv) publishes `org.mpris.MediaPlayer2.<app>` on
//! the session bus. Follows players appearing, leaving and changing track or state.

use std::collections::HashMap;

use futures_lite::StreamExt;
use smithay::reexports::calloop::channel::Sender;
use zbus::{Connection, MatchRule, MessageStream, fdo, message, proxy::CacheProperties, zvariant::OwnedValue};

use super::Update;

const NAMESPACE: &str = "org.mpris.MediaPlayer2";
const PATH: &str = "/org/mpris/MediaPlayer2";

#[derive(Clone, Debug, PartialEq)]
pub struct Media {
    /// The player's bus name, where the controls go.
    pub player: String,
    pub title: String,
    /// Artists joined with `, `; may be empty (a web video).
    pub artist: String,
    pub playing: bool,
    /// `mpris:artUrl`'s path, percent-decoded, when it is a local file (the player's own cache); empty otherwise, since
    /// nothing is downloaded.
    pub art: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    PlayPause,
    Next,
    Previous,
    Stop,
    /// Ten seconds on (`true`) or back.
    Seek(bool),
}

#[zbus::proxy(interface = "org.mpris.MediaPlayer2.Player", default_path = "/org/mpris/MediaPlayer2")]
trait Player {
    fn play_pause(&self) -> zbus::Result<()>;
    fn next(&self) -> zbus::Result<()>;
    fn previous(&self) -> zbus::Result<()>;
    fn stop(&self) -> zbus::Result<()>;
    /// Microseconds from the current position.
    fn seek(&self, offset: i64) -> zbus::Result<()>;
    #[zbus(property)]
    fn playback_status(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn metadata(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
}

pub async fn watch(bus: &Connection, updates: &Sender<Update>) {
    let streams = async {
        let owners =
            MatchRule::builder().msg_type(message::Type::Signal).sender("org.freedesktop.DBus")?.member("NameOwnerChanged")?.arg0ns(NAMESPACE)?.build();
        let changes = MatchRule::builder().msg_type(message::Type::Signal).member("PropertiesChanged")?.path(PATH)?.build();
        let owners = MessageStream::for_match_rule(owners, bus, Some(16)).await?;
        let changes = MessageStream::for_match_rule(changes, bus, Some(16)).await?;
        zbus::Result::Ok(owners.map(drop).or(changes.map(drop)))
    };
    match streams.await {
        Ok(events) => super::relay(bus, events, "media", updates, Update::Media, read).await,
        Err(err) => tracing::warn!("Now Playing off: {err}"),
    }
}

/// The playing player, else a paused one; `None` when nothing is playing or paused.
// ponytail: with several paused players the first by name wins, not the last one used; remember the last player to
// switch to Playing if that shows up in use.
async fn read(bus: &Connection) -> Option<Media> {
    let mut names = fdo::DBusProxy::new(bus).await.ok()?.list_names().await.ok()?;
    names.sort();
    let mut paused = None;
    // playerctld mirrors another player; showing it would show that one twice.
    for name in names.iter().filter(|n| n.strip_prefix(NAMESPACE).is_some_and(|app| app.starts_with('.') && app != ".playerctld")) {
        let Some(media) = player(bus, name).await else { continue };
        if media.playing {
            return Some(media);
        }
        paused.get_or_insert(media);
    }
    paused
}

async fn player(bus: &Connection, name: &str) -> Option<Media> {
    let proxy = PlayerProxy::builder(bus).destination(name).ok()?.cache_properties(CacheProperties::No).build().await.ok()?;
    let status = proxy.playback_status().await.ok()?;
    from_raw(name, &status, &proxy.metadata().await.unwrap_or_default())
}

fn from_raw(player: &str, status: &str, metadata: &HashMap<String, OwnedValue>) -> Option<Media> {
    let playing = match status {
        "Playing" => true,
        "Paused" => false,
        _ => return None,
    };
    let title = metadata.get("xesam:title").and_then(|v| v.downcast_ref::<&str>().ok()).unwrap_or_default().to_owned();
    let artists = metadata.get("xesam:artist").and_then(|v| Vec::<String>::try_from(v.try_clone().ok()?).ok());
    let art = metadata.get("mpris:artUrl").and_then(|v| v.downcast_ref::<&str>().ok()).and_then(crate::apps::file_path).unwrap_or_default();
    Some(Media { player: player.to_owned(), title, artist: artists.unwrap_or_default().join(", "), playing, art })
}

pub async fn control(bus: &Connection, player: &str, action: Action) {
    let result = match PlayerProxy::builder(bus).destination(player).map(|b| b.cache_properties(CacheProperties::No)) {
        Ok(builder) => match builder.build().await {
            Ok(proxy) => match action {
                Action::PlayPause => proxy.play_pause().await,
                Action::Next => proxy.next().await,
                Action::Previous => proxy.previous().await,
                Action::Stop => proxy.stop().await,
                Action::Seek(on) => proxy.seek(if on { 10_000_000 } else { -10_000_000 }).await,
            },
            Err(err) => Err(err),
        },
        Err(err) => Err(err),
    };
    if let Err(err) = result {
        tracing::warn!("{player} {action:?}: {err}");
    }
}

#[cfg(test)]
mod tests {
    use zbus::zvariant::{Str, Value};

    use super::*;

    #[test]
    fn metadata() {
        let mut metadata = HashMap::new();
        metadata.insert("xesam:title".to_owned(), OwnedValue::from(Str::from("Opal")));
        metadata.insert("xesam:artist".to_owned(), Value::from(vec!["A", "B"]).try_into().unwrap());
        metadata.insert("mpris:artUrl".to_owned(), OwnedValue::from(Str::from("file:///tmp/art.png")));
        let media = from_raw("org.mpris.MediaPlayer2.mpv", "Playing", &metadata).unwrap();
        assert_eq!((media.title.as_str(), media.artist.as_str(), media.playing, media.art.as_str()), ("Opal", "A, B", true, "/tmp/art.png"));
        metadata.insert("mpris:artUrl".to_owned(), OwnedValue::from(Str::from("file:///tmp/My%20Art.png")));
        assert_eq!(from_raw("org.mpris.MediaPlayer2.mpv", "Playing", &metadata).unwrap().art, "/tmp/My Art.png", "escapes decoded");
        metadata.insert("mpris:artUrl".to_owned(), OwnedValue::from(Str::from("https://i.scdn.co/image/x")));
        assert_eq!(from_raw("org.mpris.MediaPlayer2.mpv", "Playing", &metadata).unwrap().art, "", "a remote picture is not fetched");
        let media = from_raw("org.mpris.MediaPlayer2.helium", "Paused", &HashMap::new()).unwrap();
        assert_eq!((media.title.as_str(), media.artist.as_str(), media.playing), ("", "", false));
        assert_eq!(from_raw("org.mpris.MediaPlayer2.mpv", "Stopped", &metadata), None);
    }
}
