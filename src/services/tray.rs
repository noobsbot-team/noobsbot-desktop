//! Tray icons (E2.2) over StatusNotifierItem: apps register an item with `org.kde.StatusNotifierWatcher`, which we
//! host on the session bus (without one, apps never show their icons). If another desktop already owns it, we only
//! read it. Items follow the watcher's signals and their own NewIcon/NewStatus/NewTitle; menus are dbusmenu.

use std::{collections::HashMap, path::Path, sync::Arc};

use futures_lite::{StreamExt, future};
use resvg::{tiny_skia, usvg};
use smithay::reexports::calloop::channel::Sender;
use zbus::{
    Connection, MatchRule, MessageStream, fdo,
    fdo::{RequestNameFlags, RequestNameReply},
    message::{self, Header},
    names::InterfaceName,
    object_server::SignalEmitter,
    proxy::CacheProperties,
    zvariant::{ObjectPath, OwnedValue, Structure, Value},
};

use super::Update;
use crate::text::Rendered;

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM: &str = "org.kde.StatusNotifierItem";
const MENU: &str = "com.canonical.dbusmenu";
/// Searched in order after the item's own IconThemePath, under every XDG data dir's `icons/`.
// ponytail: fixed list (D9's WhiteSur first); follows an icon-theme setting once Appearance has one.
const THEMES: &[&str] = &["WhiteSur", "hicolor", "breeze", "Adwaita"];

#[derive(Clone, Debug, PartialEq)]
pub struct TrayItem {
    /// Bus name and object path, where clicks go.
    pub service: String,
    pub path: String,
    pub title: String,
    /// The dbusmenu object path.
    pub menu: Option<String>,
    /// A click opens the menu instead of calling Activate.
    pub item_is_menu: bool,
    /// Every size found; `render` picks one.
    pub icon: Arc<Vec<Image>>,
}

#[derive(PartialEq, Eq)]
pub enum Image {
    /// IconPixmap: ARGB32, network byte order.
    Argb {
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    },
    Png {
        size: u32,
        bytes: Vec<u8>,
    },
    Svg(Vec<u8>),
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Image::Argb { width, height, .. } => write!(f, "Argb({width}×{height})"),
            Image::Png { size, .. } => write!(f, "Png({size})"),
            Image::Svg(_) => write!(f, "Svg"),
        }
    }
}

impl Image {
    fn size(&self) -> u32 {
        match self {
            Image::Argb { width, height, .. } => *width.max(height),
            Image::Png { size, .. } => *size,
            Image::Svg(_) => u32::MAX,
        }
    }
}

/// A dbusmenu row; `None` in a row list is a separator.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuRow {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
}

/// The watcher we host: item ids are `<bus name><object path>`, as KDE's.
#[derive(Default)]
struct Watcher {
    items: Vec<String>,
}

impl Watcher {
    /// `service` is a bus name, or an object path on `sender` (libappindicator). The id, if it is new.
    fn register(&mut self, sender: &str, service: &str) -> Option<String> {
        let id = match service {
            s if s.starts_with('/') => format!("{sender}{s}"),
            s if s.contains('/') => s.to_owned(),
            s => format!("{s}/StatusNotifierItem"),
        };
        (!self.items.contains(&id)).then(|| {
            self.items.push(id.clone());
            id
        })
    }

    /// Drops the items on bus name `name`, which has gone; the ids dropped.
    fn vanished(&mut self, name: &str) -> Vec<String> {
        let (gone, kept) = std::mem::take(&mut self.items).into_iter().partition(|id| split(id).0 == name);
        self.items = kept;
        gone
    }
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    async fn register_status_notifier_item(
        &mut self,
        service: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        let sender = header.sender().map(|s| s.as_str()).unwrap_or_default();
        if let Some(id) = self.register(sender, service) {
            Self::status_notifier_item_registered(&emitter, &id).await?;
            self.registered_status_notifier_items_changed(&emitter).await?;
        }
        Ok(())
    }

    /// We are the only host that matters.
    fn register_status_notifier_host(&self, _service: &str) {}

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.clone()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    async fn status_notifier_item_registered(emitter: &SignalEmitter<'_>, service: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_item_unregistered(emitter: &SignalEmitter<'_>, service: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

#[zbus::proxy(interface = "org.kde.StatusNotifierWatcher", default_service = "org.kde.StatusNotifierWatcher", default_path = "/StatusNotifierWatcher")]
trait StatusNotifierWatcher {
    fn register_status_notifier_host(&self, service: &str) -> zbus::Result<()>;
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> zbus::Result<Vec<String>>;
}

/// `<bus name><object path>` → both; a bare name means the default path.
fn split(id: &str) -> (&str, &str) {
    id.find('/').map_or((id, "/StatusNotifierItem"), |i| id.split_at(i))
}

/// Hosts the watcher (or reads the one already running) and sends the items whenever they change.
// ponytail: when another desktop's watcher goes away later we keep reading nothing rather than taking the name over;
// claim it on its NameOwnerChanged if that matters outside nested runs.
pub async fn watch(bus: &Connection, updates: &Sender<Update>) {
    let hosting = match serve(bus).await {
        Ok(hosting) => hosting,
        Err(err) => return tracing::warn!("tray off: {err}"),
    };
    let prune = async {
        if hosting && let Err(err) = prune(bus).await {
            tracing::warn!("tray watcher: {err}");
        }
    };
    let streams = async {
        let watcher = MatchRule::builder().msg_type(message::Type::Signal).sender(WATCHER)?.path(WATCHER_PATH)?.build();
        let items = MatchRule::builder().msg_type(message::Type::Signal).interface(ITEM)?.build();
        let owner = fdo::DBusProxy::new(bus).await?.receive_name_owner_changed_with_args(&[(0, WATCHER)]).await?;
        let watcher = MessageStream::for_match_rule(watcher, bus, Some(16)).await?;
        // NewToolTip and the like change nothing shown.
        let items = MessageStream::for_match_rule(items, bus, Some(16))
            .await?
            .filter(|m| m.as_ref().is_ok_and(|m| m.header().member().is_some_and(|n| matches!(n.as_str(), "NewIcon" | "NewStatus" | "NewTitle"))));
        zbus::Result::Ok(owner.map(drop).or(watcher.map(drop)).or(items.map(drop)))
    };
    let follow = async {
        match streams.await {
            Ok(events) => super::relay(bus, events, "tray", updates, |items| Update::Tray(items.unwrap_or_default()), read).await,
            Err(err) => tracing::warn!("tray off: {err}"),
        }
    };
    future::zip(prune, follow).await;
}

/// `true`: we own the watcher. Otherwise someone else does, and we tell it there is a host.
async fn serve(bus: &Connection) -> zbus::Result<bool> {
    bus.object_server().at(WATCHER_PATH, Watcher::default()).await?;
    // zbus turns the "Exists" reply into NameTaken.
    let reply = match bus.request_name_with_flags(WATCHER, RequestNameFlags::DoNotQueue.into()).await {
        Err(zbus::Error::NameTaken) => RequestNameReply::Exists,
        other => other?,
    };
    if reply == RequestNameReply::PrimaryOwner {
        let watcher = bus.object_server().interface::<_, Watcher>(WATCHER_PATH).await?;
        Watcher::status_notifier_host_registered(watcher.signal_emitter()).await?;
        return Ok(true);
    }
    tracing::info!("{WATCHER} is already running, reading it");
    bus.object_server().remove::<Watcher, _>(WATCHER_PATH).await?;
    let name = bus.unique_name().map(|n| n.to_string()).unwrap_or_default();
    StatusNotifierWatcherProxy::new(bus).await?.register_status_notifier_host(&name).await?;
    Ok(false)
}

/// Drops the items of apps that leave the bus.
async fn prune(bus: &Connection) -> zbus::Result<()> {
    let mut gone = fdo::DBusProxy::new(bus).await?.receive_name_owner_changed_with_args(&[(2, "")]).await?;
    let watcher = bus.object_server().interface::<_, Watcher>(WATCHER_PATH).await?;
    while let Some(signal) = gone.next().await {
        let Ok(args) = signal.args() else { continue };
        let removed = watcher.get_mut().await.vanished(args.name().as_str());
        for id in &removed {
            Watcher::status_notifier_item_unregistered(watcher.signal_emitter(), id).await?;
        }
        if !removed.is_empty() {
            watcher.get().await.registered_status_notifier_items_changed(watcher.signal_emitter()).await?;
        }
    }
    Ok(())
}

/// Every shown item, in registration order.
// ponytail: re-reads every item (and looks its icon up again) on any item's signal; cache per item if a tray full
// of animated icons shows up in a profile.
async fn read(bus: &Connection) -> Option<Vec<TrayItem>> {
    let watcher = StatusNotifierWatcherProxy::builder(bus).cache_properties(CacheProperties::No).build().await.ok()?;
    let mut items = Vec::new();
    for id in watcher.registered_status_notifier_items().await.ok()? {
        let (service, path) = split(&id);
        let props = async {
            let proxy =
                fdo::PropertiesProxy::builder(bus).destination(service).ok()?.path(path).ok()?.cache_properties(CacheProperties::No).build().await.ok()?;
            proxy.get_all(InterfaceName::from_static_str_unchecked(ITEM)).await.ok()
        };
        if let Some(item) = props.await.and_then(|p| from_props(service, path, &p)) {
            items.push(item);
        }
    }
    Some(items)
}

/// `None` for a Passive item (hidden, as on KDE) or one without any icon.
fn from_props(service: &str, path: &str, props: &HashMap<String, OwnedValue>) -> Option<TrayItem> {
    let text = |key: &str| props.get(key).and_then(|v| v.downcast_ref::<&str>().ok()).unwrap_or_default().to_owned();
    if text("Status") == "Passive" {
        return None;
    }
    let name = text("IconName");
    let mut icon = if name.is_empty() { Vec::new() } else { find_icon(&name, &text("IconThemePath")) };
    if icon.is_empty() {
        let pixmaps = props.get("IconPixmap").and_then(|v| Vec::<(i32, i32, Vec<u8>)>::try_from(v.try_clone().ok()?).ok());
        icon = pixmaps.unwrap_or_default().into_iter().filter_map(|(w, h, pixels)| argb(w, h, pixels)).collect();
    }
    if icon.is_empty() {
        return None;
    }
    let menu = props.get("Menu").and_then(|v| v.downcast_ref::<ObjectPath>().ok()).map(|p| p.to_string());
    Some(TrayItem {
        service: service.to_owned(),
        path: path.to_owned(),
        title: text("Title"),
        // libappindicator says "/NO_DBUSMENU" for none.
        menu: menu.filter(|p| p != "/" && p != "/NO_DBUSMENU"),
        item_is_menu: props.get("ItemIsMenu").and_then(|v| v.downcast_ref::<bool>().ok()).unwrap_or(false),
        icon: Arc::new(icon),
    })
}

/// A pixmap whose size matches its data.
fn argb(width: i32, height: i32, pixels: Vec<u8>) -> Option<Image> {
    let (width, height) = (u32::try_from(width).ok()?, u32::try_from(height).ok()?);
    (width > 0 && height > 0 && pixels.len() == (width * height * 4) as usize).then_some(Image::Argb { width, height, pixels })
}

/// `name` in the first place that has it: IconThemePath, then each theme (every size it has), then pixmaps/. An
/// absolute path is read as it is.
fn find_icon(name: &str, theme_path: &str) -> Vec<Image> {
    if name.starts_with('/') {
        return load(Path::new(name)).into_iter().collect();
    }
    let home = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{}/.local/share", std::env::var("HOME").unwrap_or_default()));
    let system = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    let data: Vec<&str> = std::iter::once(home.as_str()).chain(system.split(':')).collect();
    // (root, depth): a theme is <size>/<context>/ (or <context>/<size>/); an IconThemePath may hold a theme.
    let mut roots = vec![(theme_path.to_owned(), 3)];
    roots.extend(THEMES.iter().flat_map(|theme| data.iter().map(move |dir| (format!("{dir}/icons/{theme}"), 2))));
    roots.push(("/usr/share/pixmaps".into(), 0));
    roots
        .into_iter()
        .filter(|(root, _)| !root.is_empty())
        .map(|(root, depth)| search(Path::new(&root), name, depth))
        .find(|found| !found.is_empty())
        .unwrap_or_default()
}

fn search(dir: &Path, name: &str, depth: u32) -> Vec<Image> {
    let mut found: Vec<Image> = ["png", "svg"].iter().filter_map(|ext| load(&dir.join(format!("{name}.{ext}")))).collect();
    if depth > 0
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        for path in entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
            found.extend(search(&path, name, depth - 1));
        }
    }
    found
}

/// A PNG (its size from the header) or an SVG.
fn load(path: &Path) -> Option<Image> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.starts_with(b"\x89PNG") {
        let size = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
        return Some(Image::Png { size, bytes });
    }
    (path.extension()? == "svg").then_some(Image::Svg(bytes))
}

/// The best of `images` for `px`: the nearest raster size (the larger on a tie), but an SVG over a raster that is too
/// small.
fn choose(images: &[Image], px: u32) -> Option<&Image> {
    let raster = images.iter().filter(|i| !matches!(i, Image::Svg(_))).min_by_key(|i| (i.size().abs_diff(px), std::cmp::Reverse(i.size())));
    let svg = images.iter().find(|i| matches!(i, Image::Svg(_)));
    match raster {
        Some(r) if r.size() >= px || svg.is_none() => Some(r),
        _ => svg,
    }
}

/// Premultiplied RGBA from ARGB32 in network byte order.
fn premultiply(argb: &[u8]) -> Vec<u8> {
    argb.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[a, r, g, b]| {
            let [r, g, b] = [r, g, b].map(|c| (u16::from(c) * u16::from(a) / 255) as u8);
            [r, g, b, a]
        })
        .collect()
}

/// A `px`×`px` icon, centred and scaled to fit; empty if nothing decodes.
pub fn render(images: &[Image], px: u32) -> Rendered {
    let empty = Rendered { pixels: Vec::new(), width: 0, height: 0, baseline: 0, truncated: false };
    let source = match choose(images, px) {
        Some(Image::Argb { width, height, pixels }) => {
            tiny_skia::IntSize::from_wh(*width, *height).and_then(|s| tiny_skia::Pixmap::from_vec(premultiply(pixels), s))
        }
        Some(Image::Png { bytes, .. }) => tiny_skia::Pixmap::decode_png(bytes).ok(),
        Some(Image::Svg(bytes)) => usvg::Tree::from_data(bytes, &usvg::Options::default()).ok().and_then(|tree| {
            let mut pixmap = tiny_skia::Pixmap::new(px, px)?;
            let fit = px as f32 / tree.size().width().max(tree.size().height());
            resvg::render(&tree, tiny_skia::Transform::from_scale(fit, fit), &mut pixmap.as_mut());
            Some(pixmap)
        }),
        None => None,
    };
    let (Some(source), Some(mut out)) = (source, tiny_skia::Pixmap::new(px, px)) else { return empty };
    let fit = px as f32 / source.width().max(source.height()) as f32;
    let (dx, dy) = ((px as f32 - source.width() as f32 * fit) / 2.0, (px as f32 - source.height() as f32 * fit) / 2.0);
    let paint = tiny_skia::PixmapPaint { quality: tiny_skia::FilterQuality::Bicubic, ..Default::default() };
    out.draw_pixmap(0, 0, source.as_ref(), &paint, tiny_skia::Transform::from_row(fit, 0.0, 0.0, fit, dx, dy), None);
    Rendered { pixels: out.take(), width: px, height: px, baseline: px, truncated: false }
}

pub async fn activate(bus: &Connection, service: &str, path: &str, x: i32, y: i32) {
    if let Err(err) = bus.call_method(Some(service), path, Some(ITEM), "Activate", &(x, y)).await {
        tracing::warn!("tray {service} Activate: {err}");
    }
}

/// Fetches `menu`'s rows and sends them as `Update::TrayMenu`.
pub async fn menu(bus: &Connection, service: String, menu: String, updates: &Sender<Update>) {
    // Lets the app fill the menu in; many don't implement it.
    let _ = bus.call_method(Some(service.as_str()), menu.as_str(), Some(MENU), "AboutToShow", &(0i32,)).await;
    let reply = match bus.call_method(Some(service.as_str()), menu.as_str(), Some(MENU), "GetLayout", &(0i32, -1i32, Vec::<&str>::new())).await {
        Ok(reply) => reply,
        Err(err) => return tracing::warn!("tray {service} menu: {err}"),
    };
    let body = reply.body();
    let mut rows = Vec::new();
    if let Some(root) = body.deserialize::<Structure>().ok().as_ref().and_then(|l| l.fields().get(1)) {
        children(root, 0, &mut rows);
    }
    while rows.last().is_some_and(Option::is_none) {
        rows.pop();
    }
    let _ = updates.send(Update::TrayMenu { service, menu, rows });
}

pub async fn clicked(bus: &Connection, service: &str, menu: &str, id: i32) {
    if let Err(err) = bus.call_method(Some(service), menu, Some(MENU), "Event", &(id, "clicked", Value::I32(0), 0u32)).await {
        tracing::warn!("tray {service} menu click: {err}");
    }
}

/// The rows under a dbusmenu node `(ia{sv}av)`. Submenus are flattened: the parent shows disabled, its rows follow,
/// indented.
// ponytail: flattened, and filled from the one GetLayout; real submenus (and AboutToShow per submenu, which some Qt
// apps need) come with nested menus in the popover.
fn children(node: &Value, depth: usize, rows: &mut Vec<Option<MenuRow>>) {
    let Value::Structure(node) = unwrap(node) else { return };
    let Some(Value::Array(kids)) = node.fields().get(2).map(unwrap) else { return };
    for value in kids.inner() {
        let Value::Structure(child) = unwrap(value) else { continue };
        let (Some(Value::I32(id)), Some(Value::Dict(props))) = (child.fields().first().map(unwrap), child.fields().get(1).map(unwrap)) else { continue };
        let prop = |key: &str| props.iter().find(|(k, _)| matches!(k, Value::Str(s) if s.as_str() == key)).map(|(_, v)| unwrap(v));
        let text = |key: &str| match prop(key) {
            Some(Value::Str(s)) => s.as_str().to_owned(),
            _ => String::new(),
        };
        if matches!(prop("visible"), Some(Value::Bool(false))) {
            continue;
        }
        if text("type") == "separator" {
            if rows.last().is_some_and(Option::is_some) {
                rows.push(None);
            }
            continue;
        }
        // "_File" underlines F; "__" is a literal underscore.
        let label = text("label").replace("__", "\0").replace('_', "").replace('\0', "_");
        let check = if matches!(prop("toggle-state"), Some(Value::I32(1))) { "✓ " } else { "" };
        let submenu = text("children-display") == "submenu";
        let enabled = !submenu && !matches!(prop("enabled"), Some(Value::Bool(false)));
        rows.push(Some(MenuRow { id: *id, label: format!("{}{check}{label}", "    ".repeat(depth)), enabled }));
        if submenu {
            children(value, depth + 1, rows);
        }
    }
}

/// The value inside any variants.
fn unwrap<'a, 'v>(value: &'a Value<'v>) -> &'a Value<'v> {
    match value {
        Value::Value(inner) => unwrap(inner),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use zbus::zvariant::{Array, Dict, StructureBuilder, Type};

    use super::*;

    #[test]
    fn pixmaps_premultiply_and_pick_sizes() {
        // ARGB: half-transparent white, opaque red.
        assert_eq!(premultiply(&[128, 255, 255, 255, 255, 255, 0, 0]), vec![128, 128, 128, 128, 255, 0, 0, 255]);
        assert_eq!(argb(2, 2, vec![0; 15]), None);
        assert_eq!(argb(-1, 2, vec![]), None);
        let px = |s: u32| argb(s as i32, s as i32, vec![255; (s * s * 4) as usize]).unwrap();
        let images = vec![px(16), px(22), px(64)];
        assert_eq!(choose(&images, 16).map(Image::size), Some(16));
        assert_eq!(choose(&images, 32).map(Image::size), Some(22));
        assert_eq!(choose(&images, 48).map(Image::size), Some(64));
        // Too small: an SVG wins; big enough: the raster does.
        let with_svg = vec![px(16), Image::Svg(Vec::new())];
        assert!(matches!(choose(&with_svg, 32), Some(Image::Svg(_))));
        assert_eq!(choose(&with_svg, 16).map(Image::size), Some(16));

        // A 2×1 opaque red pixmap fits 32 px across, centred vertically.
        let r = render(&[argb(2, 1, vec![255, 255, 0, 0, 255, 255, 0, 0]).unwrap()], 32);
        assert_eq!((r.width, r.height, r.pixels.len()), (32, 32, 32 * 32 * 4));
        let at = |x: usize, y: usize| &r.pixels[(y * 32 + x) * 4..][..4];
        assert_eq!(at(16, 16), [255, 0, 0, 255]);
        assert_eq!(at(16, 0)[3], 0);
        assert_eq!(render(&[], 16).width, 0);
    }

    #[test]
    fn watcher_bookkeeping() {
        let mut watcher = Watcher::default();
        assert_eq!(watcher.register(":1.5", "org.kde.StatusNotifierItem-5-1").as_deref(), Some("org.kde.StatusNotifierItem-5-1/StatusNotifierItem"));
        assert_eq!(watcher.register(":1.7", "/org/ayatana/NotificationItem/app").as_deref(), Some(":1.7/org/ayatana/NotificationItem/app"));
        // Registering twice is a no-op.
        assert_eq!(watcher.register(":1.7", "/org/ayatana/NotificationItem/app"), None);
        assert_eq!(split(&watcher.items[1]), (":1.7", "/org/ayatana/NotificationItem/app"));
        assert!(watcher.vanished(":1.9").is_empty());
        assert_eq!(watcher.vanished(":1.7"), vec![":1.7/org/ayatana/NotificationItem/app".to_owned()]);
        assert_eq!(watcher.vanished("org.kde.StatusNotifierItem-5-1").len(), 1);
        assert!(watcher.items.is_empty());
    }

    #[test]
    fn items_from_properties() {
        let mut props = HashMap::new();
        props.insert("Title".to_owned(), OwnedValue::from(zbus::zvariant::Str::from("App")));
        props.insert("IconPixmap".to_owned(), Value::from(vec![(1i32, 1i32, vec![255u8, 1, 2, 3])]).try_into().unwrap());
        props.insert("Menu".to_owned(), Value::from(ObjectPath::from_static_str_unchecked("/MenuBar")).try_into().unwrap());
        let item = from_props(":1.5", "/StatusNotifierItem", &props).unwrap();
        assert_eq!((item.title.as_str(), item.menu.as_deref(), item.item_is_menu), ("App", Some("/MenuBar"), false));
        assert_eq!(*item.icon, vec![Image::Argb { width: 1, height: 1, pixels: vec![255, 1, 2, 3] }]);
        props.insert("Status".to_owned(), OwnedValue::from(zbus::zvariant::Str::from("Passive")));
        assert_eq!(from_props(":1.5", "/StatusNotifierItem", &props), None);
    }

    #[test]
    fn menu_layout_flattens() {
        let node = |id: i32, props: &[(&'static str, Value<'static>)], kids: Vec<Value<'static>>| {
            let mut dict = Dict::new(<&str>::SIGNATURE, Value::SIGNATURE);
            for (k, v) in props {
                dict.add(*k, Value::Value(Box::new(v.clone()))).unwrap();
            }
            let mut array = Array::new(Value::SIGNATURE);
            for kid in kids {
                array.append(Value::Value(Box::new(kid))).unwrap();
            }
            Value::Structure(StructureBuilder::new().add_field(id).append_field(Value::Dict(dict)).append_field(Value::Array(array)).build().unwrap())
        };
        let root = node(
            0,
            &[],
            vec![
                node(1, &[("label", "_Open".into())], vec![]),
                node(2, &[("type", "separator".into())], vec![]),
                node(3, &[("label", "Mute".into()), ("toggle-type", "checkmark".into()), ("toggle-state", 1i32.into())], vec![]),
                node(4, &[("label", "Gone".into()), ("visible", false.into())], vec![]),
                node(
                    5,
                    &[("label", "More".into()), ("children-display", "submenu".into())],
                    vec![node(6, &[("label", "A__B".into()), ("enabled", false.into())], vec![])],
                ),
            ],
        );
        let mut rows = Vec::new();
        children(&root, 0, &mut rows);
        let row = |id, label: &str, enabled| Some(MenuRow { id, label: label.into(), enabled });
        assert_eq!(rows, vec![row(1, "Open", true), None, row(3, "✓ Mute", true), row(5, "More", false), row(6, "    A_B", false)]);
    }
}
