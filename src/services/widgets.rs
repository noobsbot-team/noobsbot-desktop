//! Widgets from other processes (E8.8): `org.noobsbot.Desktop.Widgets` on the desktop's bus name. An app `Set`s a
//! widget by id with a small description (`a{sv}`: `title` s, `lines` as, `icon` s, `progress` d 0–1, `size` s
//! small/medium); the shell draws it in Notification Center or on the desktop (`[widgets]`). A click is the
//! `Activated(id)` signal, sent to the owner only. Its widgets go when the owner leaves the bus.

use std::collections::HashMap;

use futures_lite::StreamExt;
use smithay::reexports::calloop::channel::Sender;
use zbus::{Connection, fdo, message::Header, names::BusName, object_server::SignalEmitter, zvariant::OwnedValue};

use super::Update;
use crate::symbols::Symbol;

const PATH: &str = "/org/noobsbot/Desktop";
/// Lines past this are dropped; a medium card has room for four.
const MAX_LINES: usize = 4;
/// Per owner, so one app can't fill the screen.
const MAX_PER_OWNER: usize = 8;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Widget {
    pub id: String,
    /// The unique bus name that set it.
    pub owner: String,
    pub title: String,
    pub lines: Vec<String>,
    pub icon: Option<Symbol>,
    /// 0–1.
    pub progress: Option<f64>,
    /// Full width; small is half.
    pub medium: bool,
}

/// `content` as a widget; unknown keys are ignored, wrong types and unknown icons or sizes are errors.
pub fn parse(id: &str, owner: &str, content: &HashMap<String, OwnedValue>) -> Result<Widget, String> {
    if id.is_empty() || id.len() > 64 {
        return Err("id must be 1–64 bytes".into());
    }
    let text = |key: &str| -> Result<Option<String>, String> {
        content.get(key).map(|v| <&str>::try_from(v).map(str::to_owned).map_err(|_| format!("{key} must be a string"))).transpose()
    };
    let lines = match content.get("lines") {
        Some(v) => <Vec<String>>::try_from(v.try_clone().map_err(|e| e.to_string())?).map_err(|_| "lines must be an array of strings")?,
        None => Vec::new(),
    };
    let icon = match text("icon")? {
        Some(name) => Some(Symbol::named(&name).ok_or(format!("unknown icon {name}"))?),
        None => None,
    };
    let progress = content.get("progress").map(|v| f64::try_from(v).map_err(|_| "progress must be a double")).transpose()?;
    let medium = match text("size")?.as_deref() {
        None | Some("small") => false,
        Some("medium") => true,
        Some(other) => return Err(format!("size must be small or medium, not {other}")),
    };
    Ok(Widget {
        id: id.into(),
        owner: owner.into(),
        title: text("title")?.unwrap_or_default(),
        lines: lines.into_iter().take(MAX_LINES).collect(),
        icon,
        progress: progress.map(|p| p.clamp(0.0, 1.0)),
        medium,
    })
}

/// The widgets for Notification Center (`desktop` false) or the desktop, in `[widgets]` order.
pub fn shown<'a>(all: &'a [Widget], places: &crate::config::Widgets, desktop: bool) -> Vec<&'a Widget> {
    let find = |id: &String| all.iter().find(|w| w.id == *id);
    if desktop {
        return places.desktop.iter().filter_map(find).collect();
    }
    let rest = all.iter().filter(|w| ![&places.center, &places.desktop, &places.hidden].iter().any(|list| list.contains(&w.id)));
    places.center.iter().filter_map(find).chain(rest).collect()
}

/// Every widget, in the order they first came.
#[derive(Debug, Default)]
struct Store(Vec<Widget>);

impl Store {
    /// Adds or replaces; another owner's id is refused.
    fn set(&mut self, widget: Widget) -> Result<(), String> {
        match self.0.iter().position(|w| w.id == widget.id) {
            Some(i) if self.0[i].owner != widget.owner => Err(format!("{} belongs to {}", widget.id, self.0[i].owner)),
            Some(i) => {
                self.0[i] = widget;
                Ok(())
            }
            None if self.0.iter().filter(|w| w.owner == widget.owner).count() >= MAX_PER_OWNER => Err(format!("at most {MAX_PER_OWNER} widgets each")),
            None => {
                self.0.push(widget);
                Ok(())
            }
        }
    }

    /// Drops the owner's widgets for which `gone` holds; true when any went.
    fn drop_where(&mut self, owner: &str, gone: impl Fn(&Widget) -> bool) -> bool {
        let before = self.0.len();
        self.0.retain(|w| w.owner != owner || !gone(w));
        self.0.len() != before
    }
}

struct Server {
    store: Store,
    updates: Sender<Update>,
}

impl Server {
    fn changed(&self) {
        let _ = self.updates.send(Update::Widgets(self.store.0.clone()));
    }
}

fn sender(header: &Header<'_>) -> String {
    header.sender().map(ToString::to_string).unwrap_or_default()
}

#[zbus::interface(name = "org.noobsbot.Desktop.Widgets")]
impl Server {
    /// Shows or updates the caller's widget `id`.
    fn set(&mut self, #[zbus(header)] header: Header<'_>, id: &str, content: HashMap<String, OwnedValue>) -> fdo::Result<()> {
        let widget = parse(id, &sender(&header), &content).map_err(fdo::Error::InvalidArgs)?;
        self.store.set(widget).map_err(fdo::Error::AccessDenied)?;
        self.changed();
        Ok(())
    }

    /// Takes the caller's widget `id` away.
    fn remove(&mut self, #[zbus(header)] header: Header<'_>, id: &str) {
        if self.store.drop_where(&sender(&header), |w| w.id == id) {
            self.changed();
        }
    }

    /// Every widget's id, in the order they first came (Settings lists them).
    #[zbus(property)]
    fn ids(&self) -> Vec<String> {
        self.store.0.iter().map(|w| w.id.clone()).collect()
    }

    /// The widget was clicked; sent to its owner only.
    #[zbus(signal)]
    async fn activated(emitter: &SignalEmitter<'_>, id: &str) -> zbus::Result<()>;
}

/// Serves the interface (the name is requested by `shortcuts::serve`), then drops the widgets of apps that leave.
pub async fn serve(bus: &Connection, updates: &Sender<Update>) {
    if let Err(err) = watch(bus, updates).await {
        tracing::warn!("widgets off: {err}");
    }
}

async fn watch(bus: &Connection, updates: &Sender<Update>) -> zbus::Result<()> {
    bus.object_server().at(PATH, Server { store: Store::default(), updates: updates.clone() }).await?;
    let mut gone = fdo::DBusProxy::new(bus).await?.receive_name_owner_changed_with_args(&[(2, "")]).await?;
    let server = bus.object_server().interface::<_, Server>(PATH).await?;
    while let Some(signal) = gone.next().await {
        let Ok(args) = signal.args() else { continue };
        let mut server = server.get_mut().await;
        if server.store.drop_where(args.name().as_str(), |_| true) {
            server.changed();
        }
    }
    Ok(())
}

/// Tells `owner` its widget `id` was clicked.
pub async fn activated(bus: &Connection, owner: &str, id: &str) {
    let result = async {
        let emitter = SignalEmitter::new(bus, PATH)?.set_destination(BusName::try_from(owner)?);
        Server::activated(&emitter, id).await
    };
    if let Err(err) = result.await {
        tracing::debug!("widget {id}: {err}");
    }
}

#[cfg(test)]
mod tests {
    use zbus::zvariant::Value;

    use super::*;

    fn content(pairs: &[(&str, Value<'_>)]) -> HashMap<String, OwnedValue> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), OwnedValue::try_from(v.try_clone().unwrap()).unwrap())).collect()
    }

    #[test]
    fn content_parses() {
        let c = content(&[
            ("title", "Build".into()),
            ("lines", vec!["main", "3 min", "a", "b", "dropped"].into()),
            ("icon", "cloud-sun".into()),
            ("progress", 1.5.into()),
            ("size", "medium".into()),
            ("future", 7u32.into()),
        ]);
        let w = parse("ci", ":1.5", &c).unwrap();
        assert_eq!((w.title.as_str(), w.lines.len(), w.icon, w.progress, w.medium), ("Build", 4, Some(Symbol::CloudSun), Some(1.0), true));
        assert_eq!(parse("ci", ":1.5", &HashMap::new()).unwrap(), Widget { id: "ci".into(), owner: ":1.5".into(), ..Default::default() });
        for bad in [
            content(&[("title", 3u32.into())]),
            content(&[("icon", "no-such".into())]),
            content(&[("size", "large".into())]),
            content(&[("lines", "x".into())]),
        ] {
            assert!(parse("ci", ":1.5", &bad).is_err(), "{bad:?}");
        }
        assert!(parse("", ":1.5", &HashMap::new()).is_err());
    }

    #[test]
    fn placed_in_config_order() {
        let all: Vec<Widget> = ["a", "b", "c", "d"].map(|id| Widget { id: id.into(), ..Default::default() }).into();
        let mut places =
            crate::config::Widgets { center: vec!["c".into(), "gone".into(), "a".into()], desktop: vec!["d".into()], hidden: vec![], ..Default::default() };
        let ids = |places: &_, desktop| shown(&all, places, desktop).iter().map(|w| w.id.as_str()).collect::<Vec<_>>();
        assert_eq!((ids(&places, false), ids(&places, true)), (vec!["c", "a", "b"], vec!["d"]));
        places.hidden = vec!["b".into(), "c".into()];
        assert_eq!(ids(&places, false), ["c", "a"], "hidden only drops the unlisted");
    }

    #[test]
    fn owners_keep_their_ids_and_take_them_along() {
        let w = |id: &str, owner: &str| Widget { id: id.into(), owner: owner.into(), ..Default::default() };
        let mut store = Store::default();
        store.set(w("a", ":1.1")).unwrap();
        store.set(w("b", ":1.1")).unwrap();
        store.set(w("c", ":1.2")).unwrap();
        assert!(store.set(w("a", ":1.2")).is_err());
        assert!(!store.drop_where(":1.2", |w| w.id == "a"));
        assert!(store.drop_where(":1.1", |_| true));
        assert_eq!(store.0, vec![w("c", ":1.2")]);
        assert!(!store.drop_where(":1.1", |_| true));
        for i in 0..MAX_PER_OWNER {
            store.set(w(&i.to_string(), ":1.3")).unwrap();
        }
        assert!(store.set(w("one more", ":1.3")).is_err());
    }
}
