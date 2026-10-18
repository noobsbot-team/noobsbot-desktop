//! Unity LauncherEntry (E3.1), the badge API Plank reads: apps broadcast `Update(app_uri, properties)` on the session
//! bus, each update adding to what that app said before; the dock shows the count, the progress and urgency. There is
//! no name to own, it is only a signal. An app that leaves the bus loses its badge.

use std::collections::HashMap;

use futures_lite::StreamExt;
use smithay::reexports::calloop::channel::Sender;
use zbus::{Connection, MatchRule, MessageStream, fdo, message, zvariant::OwnedValue};

use super::Update;

const INTERFACE: &str = "com.canonical.Unity.LauncherEntry";

/// What the dock shows on an app's icon.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Entry {
    pub count: Option<i64>,
    /// 0–1.
    pub progress: Option<f64>,
    pub urgent: bool,
}

/// Everything an app has said so far.
#[derive(Debug, Clone, Default, PartialEq)]
struct Said {
    count: i64,
    count_visible: bool,
    progress: f64,
    progress_visible: bool,
    urgent: bool,
}

impl Said {
    fn apply(&mut self, properties: &HashMap<String, OwnedValue>) {
        for (key, v) in properties {
            match key.as_str() {
                "count" => self.count = i64::try_from(v).or_else(|_| i32::try_from(v).map(i64::from)).unwrap_or(self.count),
                "count-visible" => self.count_visible = bool::try_from(v).unwrap_or(self.count_visible),
                "progress" => self.progress = f64::try_from(v).unwrap_or(self.progress),
                "progress-visible" => self.progress_visible = bool::try_from(v).unwrap_or(self.progress_visible),
                "urgent" => self.urgent = bool::try_from(v).unwrap_or(self.urgent),
                _ => {}
            }
        }
    }

    fn entry(&self) -> Entry {
        Entry { count: self.count_visible.then_some(self.count), progress: self.progress_visible.then_some(self.progress.clamp(0.0, 1.0)), urgent: self.urgent }
    }
}

/// `application://org.kde.dolphin.desktop` → `org.kde.dolphin`.
pub fn app_id(uri: &str) -> &str {
    let id = uri.strip_prefix("application://").unwrap_or(uri);
    id.strip_suffix(".desktop").unwrap_or(id)
}

enum Event {
    Update(message::Message),
    Gone(String),
}

pub async fn watch(bus: &Connection, updates: &Sender<Update>) {
    let streams = async {
        let rule = MatchRule::builder().msg_type(message::Type::Signal).interface(INTERFACE)?.member("Update")?.build();
        let signals = MessageStream::for_match_rule(rule, bus, Some(16)).await?;
        let gone = fdo::DBusProxy::new(bus).await?.receive_name_owner_changed_with_args(&[(2, "")]).await?;
        let gone = gone.filter_map(|s| s.args().ok().map(|a| Event::Gone(a.name().to_string())));
        zbus::Result::Ok(signals.filter_map(|m| m.ok().map(Event::Update)).or(gone))
    };
    let mut events = match streams.await {
        Ok(events) => Box::pin(events),
        Err(err) => return tracing::warn!("dock badges off: {err}"),
    };
    // App id → (its sender, what it said).
    let mut apps: HashMap<String, (String, Said)> = HashMap::new();
    while let Some(event) = events.next().await {
        let changed: Vec<(String, Entry)> = match event {
            Event::Update(message) => {
                let sender = message.header().sender().map(|s| s.to_string()).unwrap_or_default();
                let Ok((uri, properties)) = message.body().deserialize::<(String, HashMap<String, OwnedValue>)>() else { continue };
                let app = app_id(&uri).to_owned();
                let (owner, said) = apps.entry(app.clone()).or_default();
                *owner = sender;
                said.apply(&properties);
                vec![(app, said.entry())]
            }
            Event::Gone(name) => {
                let gone: Vec<String> = apps.iter().filter(|(_, (owner, _))| *owner == name).map(|(app, _)| app.clone()).collect();
                gone.into_iter().inspect(|app| drop(apps.remove(app))).map(|app| (app, Entry::default())).collect()
            }
        };
        for (app, entry) in changed {
            tracing::debug!(app, ?entry, "launcher entry");
            if updates.send(Update::Launcher { app, entry }).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    #[test]
    fn app_ids_come_from_uris() {
        assert_eq!(app_id("application://org.kde.dolphin.desktop"), "org.kde.dolphin");
        assert_eq!(app_id("application://firefox"), "firefox");
        assert_eq!(app_id("kitty.desktop"), "kitty");
    }

    #[test]
    fn updates_add_up_and_hide_until_visible() {
        let props = |pairs: Vec<(&str, Value)>| pairs.into_iter().map(|(k, v)| (k.to_owned(), OwnedValue::try_from(v).unwrap())).collect();
        let mut said = Said::default();
        said.apply(&props(vec![("count", Value::I64(3))]));
        assert_eq!(said.entry(), Entry::default(), "count not visible yet");
        said.apply(&props(vec![("count-visible", Value::Bool(true)), ("progress", Value::F64(1.5)), ("progress-visible", Value::Bool(true))]));
        assert_eq!(said.entry(), Entry { count: Some(3), progress: Some(1.0), urgent: false });
        said.apply(&props(vec![("urgent", Value::Bool(true)), ("count", Value::I32(7)), ("quicklist", Value::from("/x"))]));
        assert_eq!(said.entry(), Entry { count: Some(7), progress: Some(1.0), urgent: true });
    }
}
