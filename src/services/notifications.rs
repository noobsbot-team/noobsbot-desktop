//! The notification server (E4.1): `org.freedesktop.Notifications` on the session bus. Calls become `Update`s; the
//! frame loop owns the store (shell/notify.rs) and answers with `Command`s that emit the closed and action signals.
//! Icons are decoded and scaled here, so the frame loop only uploads them. History lives in a file (E4.2, last 100).

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use resvg::tiny_skia;
use smithay::reexports::calloop::channel::Sender;
use zbus::{
    Connection,
    fdo::{RequestNameFlags, RequestNameReply},
    object_server::SignalEmitter,
    zvariant::{OwnedValue, Value},
};

use super::Update;
use crate::text::Rendered;

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
/// Icons are kept at this many px: 32 logical at up to 2× scale.
pub const ICON_PX: u32 = 64;
/// History kept across restarts (E4.2).
pub const HISTORY: usize = 100;

/// One notification as an app sent it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Notification {
    pub id: u32,
    /// The app's name, the group key in Notification Center.
    pub app: String,
    /// What the icon was looked up from (path or theme name), kept in the history file.
    pub icon_name: String,
    /// `ICON_PX` square, premultiplied RGBA.
    pub icon: Option<Arc<Rendered>>,
    pub summary: String,
    pub body: String,
    /// (key, label); the "default" key is the click on the notification itself, not a button.
    pub actions: Vec<(String, String)>,
    /// 0 low, 1 normal, 2 critical.
    pub urgency: u8,
    /// ms; -1 the server decides, 0 never.
    pub expire: i32,
    /// Gone once its banner goes; never in history.
    pub transient: bool,
    /// Stays after an action is invoked.
    pub resident: bool,
    /// The `value` hint, percent.
    pub progress: Option<u8>,
    /// The `desktop-entry` hint: the app's id, to find its window on a click.
    pub desktop_entry: String,
    /// Unix seconds.
    pub time: i64,
    /// Body lines as laid out (0–2), measured by the frame loop when it arrives.
    pub body_lines: u8,
}

/// Why a notification went (the spec's reasons).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Expired = 1,
    Dismissed = 2,
    /// By a `CloseNotification` call.
    Closed = 3,
}

struct Server {
    updates: Sender<Update>,
    next: u32,
    icons: HashMap<String, Option<Arc<Rendered>>>,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    #[expect(clippy::too_many_arguments, reason = "the D-Bus method's signature")]
    fn notify(
        &mut self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let id = if replaces_id != 0 {
            replaces_id
        } else {
            let id = self.next;
            self.next = self.next.checked_add(1).unwrap_or(1);
            id
        };
        let mut n = parse(id, app_name, app_icon, summary, body, &actions, &hints, expire_timeout);
        n.time = chrono::Local::now().timestamp();
        // The spec's order: image-data, image-path, app_icon; the app's own icon by its entry or name last.
        n.icon = ["image-data", "image_data"].iter().find_map(|k| hints.get(*k)).and_then(|v| image_data(v)).map(Arc::new);
        if n.icon.is_none() {
            n.icon = self.icon(&n.icon_name);
        }
        if n.icon.is_none() && hints.contains_key("icon_data") {
            n.icon = hints.get("icon_data").and_then(|v| image_data(v)).map(Arc::new);
        }
        tracing::debug!(id, app = n.app, summary = n.summary, "notification");
        let _ = self.updates.send(Update::Notify(Box::new(n)));
        id
    }

    fn close_notification(&self, id: u32) {
        let _ = self.updates.send(Update::CloseNotification(id));
    }

    fn get_capabilities(&self) -> Vec<&'static str> {
        vec!["actions", "body", "icon-static", "persistence"]
    }

    #[zbus(out_args("name", "vendor", "version", "spec_version"))]
    fn get_server_information(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        ("Noobsbot Desktop", "Noobsbot", env!("CARGO_PKG_VERSION"), "1.2")
    }

    #[zbus(signal)]
    async fn notification_closed(emitter: &SignalEmitter<'_>, id: u32, reason: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn action_invoked(emitter: &SignalEmitter<'_>, id: u32, action_key: &str) -> zbus::Result<()>;
}

impl Server {
    /// A theme name or a file, decoded once per name.
    fn icon(&mut self, name: &str) -> Option<Arc<Rendered>> {
        if name.is_empty() {
            return None;
        }
        self.icons.entry(name.to_owned()).or_insert_with(|| load_icon(name).map(Arc::new)).clone()
    }
}

/// Everything but the icon pixels and the time, from a `Notify` call.
#[expect(clippy::too_many_arguments, reason = "the D-Bus method's arguments")]
fn parse(
    id: u32,
    app: String,
    app_icon: String,
    summary: String,
    body: String,
    actions: &[String],
    hints: &HashMap<String, OwnedValue>,
    expire: i32,
) -> Notification {
    let text = |key: &str| hints.get(key).and_then(|v| <&str>::try_from(&**v).ok()).unwrap_or_default().to_owned();
    let flag = |key: &str| hints.get(key).is_some_and(|v| matches!(**v, Value::Bool(true)));
    let desktop_entry = text("desktop-entry");
    let image_path = [text("image-path"), text("image_path")].into_iter().find(|s| !s.is_empty()).unwrap_or_default();
    let icon_name = [image_path, app_icon, desktop_entry.clone()].into_iter().find(|s| !s.is_empty()).unwrap_or_default();
    let urgency = match hints.get("urgency").map(|v| &**v) {
        Some(Value::U8(u)) => (*u).min(2),
        _ => 1,
    };
    let progress = match hints.get("value").map(|v| &**v) {
        Some(Value::I32(v)) => Some((*v).clamp(0, 100) as u8),
        Some(Value::U32(v)) => Some((*v).min(100) as u8),
        _ => None,
    };
    let app = if app.is_empty() { desktop_entry.clone() } else { app };
    Notification {
        id,
        app,
        icon_name,
        summary,
        body,
        actions: actions.as_chunks::<2>().0.iter().map(|[key, label]| (key.clone(), label.clone())).collect(),
        urgency,
        expire,
        transient: flag("transient"),
        resident: flag("resident"),
        progress,
        desktop_entry,
        ..Default::default()
    }
}

/// The `(iiibiiay)` image hint: width, height, rowstride, has alpha, bits per sample, channels, data.
fn image_data(value: &Value) -> Option<Rendered> {
    let Value::Structure(s) = value else { return None };
    let f = s.fields();
    let int = |i: usize| match f.get(i) {
        Some(Value::I32(v)) => usize::try_from(*v).ok(),
        _ => None,
    };
    let data = Vec::<u8>::try_from(f.get(6)?.try_clone().ok()?).ok()?;
    if int(4)? != 8 {
        return None;
    }
    let (width, height, stride, channels) = (int(0)?, int(1)?, int(2)?, int(5)?);
    fit(&to_rgba(width, height, stride, channels, &data)?, width as u32, height as u32)
}

/// Packed straight RGB or RGBA rows (each `stride` bytes) as packed premultiplied RGBA.
fn to_rgba(width: usize, height: usize, stride: usize, channels: usize, data: &[u8]) -> Option<Vec<u8>> {
    if !(3..=4).contains(&channels) || width == 0 || height == 0 || stride < width * channels || data.len() < stride * (height - 1) + width * channels {
        return None;
    }
    let mut out = Vec::with_capacity(width * height * 4);
    for row in data.chunks(stride).take(height) {
        for p in row[..width * channels].chunks_exact(channels) {
            let a = if channels == 4 { p[3] } else { 255 };
            out.extend([p[0], p[1], p[2]].map(|c| (u16::from(c) * u16::from(a) / 255) as u8));
            out.push(a);
        }
    }
    Some(out)
}

/// Premultiplied RGBA scaled to fit an `ICON_PX` square, centred.
fn fit(rgba: &[u8], width: u32, height: u32) -> Option<Rendered> {
    let source = tiny_skia::Pixmap::from_vec(rgba.to_vec(), tiny_skia::IntSize::from_wh(width, height)?)?;
    let mut target = tiny_skia::Pixmap::new(ICON_PX, ICON_PX)?;
    let s = ICON_PX as f32 / width.max(height) as f32;
    let (dx, dy) = ((ICON_PX as f32 - width as f32 * s) / 2.0, (ICON_PX as f32 - height as f32 * s) / 2.0);
    let paint = tiny_skia::PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..Default::default() };
    target.draw_pixmap(0, 0, source.as_ref(), &paint, tiny_skia::Transform::from_row(s, 0.0, 0.0, s, dx, dy), None);
    Some(Rendered { pixels: target.take(), width: ICON_PX, height: ICON_PX, baseline: 0, truncated: false })
}

/// A file (path or `file://` URI) or an icon theme name.
pub fn load_icon(name: &str) -> Option<Rendered> {
    let path = name.strip_prefix("file://").unwrap_or(name);
    if path.starts_with('/') {
        return decode(&PathBuf::from(path));
    }
    // The dock's lookup: WhiteSur first, like every other icon on the desktop.
    crate::apps::find_icon(name).and_then(|path| decode(&path))
}

/// SVG, PNG or JPEG, scaled to fit `ICON_PX`.
fn decode(path: &std::path::Path) -> Option<Rendered> {
    let bytes = std::fs::read(path).ok()?;
    if path.extension().is_some_and(|e| e == "svg" || e == "svgz") {
        return crate::apps::rasterise(path, ICON_PX);
    }
    let (width, height, channels, pixels) = if bytes.starts_with(b"\x89PNG") {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(&bytes));
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = decoder.read_info().ok()?;
        let mut pixels = vec![0; reader.output_buffer_size()?];
        let info = reader.next_frame(&mut pixels).ok()?;
        let channels = info.color_type.samples();
        (info.width as usize, info.height as usize, channels, pixels)
    } else {
        use zune_jpeg::zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
        let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
        let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(&bytes), options);
        let pixels = decoder.decode().ok()?;
        let (width, height) = decoder.dimensions()?;
        (width, height, 4, pixels)
    };
    // Grey and grey-alpha widen to RGB(A).
    let pixels: Vec<u8> = match channels {
        1 => pixels.iter().flat_map(|&v| [v, v, v]).collect(),
        2 => pixels.as_chunks::<2>().0.iter().flat_map(|&[v, a]| [v, v, v, a]).collect(),
        _ => pixels,
    };
    let channels = if channels <= 2 { channels + 2 } else { channels };
    fit(&to_rgba(width, height, width * channels, channels, &pixels)?, width as u32, height as u32)
}

/// Serves the interface; a session that already has a notification daemon keeps it (no replacing, no queueing).
pub async fn serve(bus: &Connection, updates: &Sender<Update>) {
    let history = load_history();
    let next = history.iter().map(|n| n.id).max().unwrap_or(0).checked_add(1).unwrap_or(1);
    let mut server = Server { updates: updates.clone(), next, icons: HashMap::new() };
    let history = history
        .into_iter()
        .map(|mut n| {
            n.icon = server.icon(&n.icon_name.clone());
            n
        })
        .collect();
    let _ = updates.send(Update::History(history));
    if let Err(err) = bus.object_server().at(PATH, server).await {
        return tracing::warn!("notifications off: {err}");
    }
    match bus.request_name_with_flags(NAME, RequestNameFlags::DoNotQueue.into()).await {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => tracing::info!("notification server running"),
        Ok(reply) => tracing::info!("another notification server owns {NAME} ({reply:?}); ours stays off"),
        Err(err) => tracing::warn!("notifications off: {err}"),
    }
}

/// Emits `NotificationClosed`.
pub async fn closed(bus: &Connection, id: u32, reason: Reason) {
    if let Ok(emitter) = SignalEmitter::new(bus, PATH)
        && let Err(err) = Server::notification_closed(&emitter, id, reason as u32).await
    {
        tracing::warn!("NotificationClosed: {err}");
    }
}

/// Emits `ActionInvoked`.
pub async fn invoked(bus: &Connection, id: u32, key: &str) {
    if let Ok(emitter) = SignalEmitter::new(bus, PATH)
        && let Err(err) = Server::action_invoked(&emitter, id, key).await
    {
        tracing::warn!("ActionInvoked: {err}");
    }
}

fn history_path() -> PathBuf {
    let state = std::env::var("XDG_STATE_HOME").unwrap_or_else(|_| format!("{}/.local/state", std::env::var("HOME").unwrap_or_default()));
    PathBuf::from(state).join("noobsbot-desktop/notifications")
}

fn load_history() -> Vec<Notification> {
    std::fs::read_to_string(history_path()).map(|text| decode_history(&text)).unwrap_or_default()
}

/// Writes the history file whole (a temporary file renamed over it, so a crash never leaves half of one).
pub fn save_history(text: &str) {
    let path = history_path();
    let temporary = path.with_extension("new");
    let result =
        path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&temporary, text)).and_then(|()| std::fs::rename(&temporary, &path));
    if let Err(err) = result {
        tracing::warn!("saving notification history to {}: {err}", path.display());
    }
}

/// One line per notification: id, time, app, icon, summary, body, tab-separated and escaped. Actions are not kept:
/// the app that would answer them is gone after a restart.
pub fn encode_history(list: &[Notification]) -> String {
    let escape = |s: &str| s.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n");
    list.iter().map(|n| format!("{}\t{}\t{}\t{}\t{}\t{}\n", n.id, n.time, escape(&n.app), escape(&n.icon_name), escape(&n.summary), escape(&n.body))).collect()
}

pub fn decode_history(text: &str) -> Vec<Notification> {
    let unescape = |s: &str| {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            out.push(match (c, c == '\\') {
                (_, true) => match chars.next() {
                    Some('t') => '\t',
                    Some('n') => '\n',
                    Some(other) => other,
                    None => break,
                },
                (c, false) => c,
            });
        }
        out
    };
    let mut list: Vec<Notification> = text
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            let [id, time, app, icon, summary, body] = f[..] else { return None };
            Some(Notification {
                id: id.parse().ok()?,
                time: time.parse().ok()?,
                app: unescape(app),
                icon_name: unescape(icon),
                summary: unescape(summary),
                body: unescape(body),
                urgency: 1,
                expire: -1,
                ..Default::default()
            })
        })
        .collect();
    list.drain(..list.len().saturating_sub(HISTORY));
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::StructureBuilder;

    fn hints(pairs: Vec<(&str, Value<'static>)>) -> HashMap<String, OwnedValue> {
        pairs.into_iter().map(|(k, v)| (k.to_owned(), OwnedValue::try_from(v).unwrap())).collect()
    }

    #[test]
    fn image_data_becomes_premultiplied_rgba_at_icon_size() {
        // 2 × 1 RGBA with a padded row: opaque red, half-transparent white.
        let data = vec![255u8, 0, 0, 255, 255, 255, 255, 128, 9, 9];
        let image = StructureBuilder::new()
            .add_field(2i32)
            .add_field(1i32)
            .add_field(10i32)
            .add_field(true)
            .add_field(8i32)
            .add_field(4i32)
            .add_field(data)
            .build()
            .unwrap();
        let icon = image_data(&Value::Structure(image)).unwrap();
        assert_eq!((icon.width, icon.height, icon.pixels.len()), (ICON_PX, ICON_PX, (ICON_PX * ICON_PX * 4) as usize));
        // Fitted and centred: the 2 × 1 picture is 64 × 32 in the middle; left half red, right half white at half alpha.
        let at = |x: u32, y: u32| &icon.pixels[((y * ICON_PX + x) * 4) as usize..][..4];
        assert_eq!(at(5, 32), [255, 0, 0, 255]);
        assert_eq!(at(60, 32), [128, 128, 128, 128]);
        assert_eq!(at(32, 2)[3], 0);
        assert_eq!(to_rgba(1, 1, 3, 3, &[10, 20, 30]), Some(vec![10, 20, 30, 255]));
        // Short data, odd channels and 16-bit samples are refused.
        assert_eq!(to_rgba(2, 2, 8, 4, &[0; 10]), None);
        assert_eq!(to_rgba(1, 1, 1, 1, &[0]), None);
        let deep = StructureBuilder::new()
            .add_field(1i32)
            .add_field(1i32)
            .add_field(8i32)
            .add_field(true)
            .add_field(16i32)
            .add_field(4i32)
            .add_field(vec![0u8; 8])
            .build()
            .unwrap();
        assert!(image_data(&Value::Structure(deep)).is_none());
    }

    #[test]
    fn notify_arguments_parse() {
        let actions = ["default", "Open", "reply", "Reply", "odd"].map(String::from);
        let h = hints(vec![
            ("urgency", Value::U8(2)),
            ("desktop-entry", Value::from("org.kde.dolphin")),
            ("transient", Value::Bool(true)),
            ("value", Value::I32(140)),
        ]);
        let n = parse(7, String::new(), String::new(), "Copied".into(), "3 files".into(), &actions, &h, 0);
        assert_eq!(n.app, "org.kde.dolphin");
        assert_eq!(n.icon_name, "org.kde.dolphin");
        assert_eq!(n.actions, vec![("default".into(), "Open".into()), ("reply".into(), "Reply".into())]);
        assert_eq!((n.urgency, n.expire, n.transient, n.resident, n.progress), (2, 0, true, false, Some(100)));
        // image-path wins over app_icon.
        let h = hints(vec![("image-path", Value::from("/tmp/a.png"))]);
        assert_eq!(parse(1, "Helium".into(), "helium".into(), String::new(), String::new(), &[], &h, -1).icon_name, "/tmp/a.png");
        assert_eq!(parse(1, "Helium".into(), "helium".into(), String::new(), String::new(), &[], &HashMap::new(), -1).urgency, 1);
    }

    #[test]
    fn history_round_trips_and_keeps_the_last_hundred() {
        let n = Notification {
            id: 3,
            time: 1_700_000_000,
            app: "Kate".into(),
            icon_name: "kate".into(),
            summary: "Saved\tfile".into(),
            body: "line 1\nline \\2".into(),
            urgency: 1,
            expire: -1,
            ..Default::default()
        };
        let text = encode_history(std::slice::from_ref(&n));
        assert_eq!(text.lines().count(), 1);
        assert_eq!(decode_history(&text), vec![n.clone()]);
        let many: Vec<Notification> = (1..=150).map(|id| Notification { id, ..n.clone() }).collect();
        let back = decode_history(&(encode_history(&many) + "garbage line\n"));
        assert_eq!((back.len(), back[0].id, back[99].id), (HISTORY, 51, 150));
    }
}
