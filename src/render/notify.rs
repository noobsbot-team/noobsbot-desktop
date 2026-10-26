//! Notification banners (E4.1) and Notification Center (E4.2), from the layouts in shell/notify.rs. Banners are
//! `card` glass, radius 16, elevation 2; Notification Center is a `panel` column holding the same cards, grouped by
//! app, then the calendar and batteries widgets.

use std::sync::Arc;

use smithay::{
    backend::renderer::{
        element::{Kind, memory::MemoryRenderBufferRenderElement},
        gles::{GlesRenderer, element::PixelShaderElement},
    },
    utils::{Point, Rectangle},
};

use super::{
    Material, SceneElement, Shaders,
    calendar::{CalendarWidget, Month},
    titlebar::{Label, keep_buffers, label_element},
};
use crate::{
    events::Upcoming,
    services::{
        weather::{Weather, condition},
        widgets::Widget,
    },
    shell::notify::{BODY, CLOSE, Card, Center, LINE, Notification, PLATE, Store, buttons},
    symbols::{Symbol, Symbols, Weight},
    text::{Rendered, Style, Text, ts},
    wm::Rect,
};

const CARD_RADIUS: f64 = 16.0;

/// One glass shape: what it is and where.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Shape {
    Panel,
    /// A notification card; banners float, so they cast a shadow.
    Card {
        shadow: bool,
    },
    /// A widget or the Do Not Disturb strip: `card`, radius 12.
    Widget,
    /// The plate peeking out under a collapsed group.
    Plate,
    /// D8 secondary button: `fill`, radius 6.
    Button,
    /// The round ✕ on a hovered card.
    Close,
    /// Behind the bell of a notification without an icon.
    IconBack,
    Track,
    Bar,
}

/// The glass depends on the shapes, dark, accent, opacity and scale.
type Key = (Vec<(Rect, Shape)>, bool, [u8; 3], u32, u64);

/// What is on screen.
pub enum View<'a> {
    Banners {
        cards: &'a [Card],
        more: Option<(Rect, usize)>,
    },
    Center {
        center: &'a Center,
        month: &'a Month,
        up_next: &'a [Upcoming],
        batteries: &'a [(String, u8)],
        dnd: bool,
        weather: Option<&'a Weather>,
        widgets: &'a [&'a Widget],
    },
    /// Other apps' widgets on the desktop, each in its rect.
    Widgets {
        rects: &'a [Rect],
        widgets: &'a [&'a Widget],
    },
}

/// "22°", never "-0°".
fn degrees(t: f32) -> String {
    format!("{}°", t.round() as i32)
}

/// "Updated 40 min ago", "Updated 3 h ago", "Updated 2 d ago".
fn updated(ago: i64) -> String {
    match ago / 60 {
        m if m < 120 => format!("Updated {m} min ago"),
        m if m < 48 * 60 => format!("Updated {} h ago", m / 60),
        m => format!("Updated {} d ago", m / (24 * 60)),
    }
}

/// The Weather widget (E7.2), medium size: place, temperature and condition with today's high and low,
/// then the next hours.
#[expect(clippy::too_many_arguments, reason = "the widget's pieces in one place")]
fn weather_pieces(pieces: &mut Pieces, text: &mut Text, symbols: &mut Symbols, ink: &Ink, w: &Weather, card: Rect, hour24: bool, scale: f64) {
    let at = |x: i32, y: i32, width: i32, h: i32| Rectangle::new((card.loc.x + x, card.loc.y + y).into(), (width, h).into());
    let inner = card.size.w - 28;
    let (words, symbol) = condition(w.code, w.day);
    // Rows grow with the text size (E8.4); icons keep their size.
    pieces.put(text.label(&w.place, ink.headline(), (inner - 120) as u32, scale), at(14, ts(12), inner - 120, ts(16)), Align::Left);
    let big = Style { size: 34, line_height: 40, weight: 400, color: ink.label, tabular: false };
    pieces.put(text.label(&degrees(w.temp), big, 120, scale), at(12, ts(30), 120, ts(40)), Align::Left);
    let now = chrono::Utc::now().timestamp();
    if w.stale(now) {
        pieces.put(text.label(&updated(now - w.fetched), ink.caption(), 200, scale), at(14, ts(72), 200, ts(13)), Align::Left);
    }
    pieces.put(symbols.get(symbol, Weight::Fill, 20, scale, ink.label), at(card.size.w - 14 - 20, 12, 20, 20), Align::Centre);
    let right = |y: i32| at(card.size.w - 14 - 160, ts(y), 160, ts(16));
    pieces.put(text.label(words, Style { size: 12, ..ink.headline() }, 160, scale), right(38), Align::Right);
    pieces.put(text.label(&format!("H:{}  L:{}", degrees(w.high), degrees(w.low)), Style { size: 12, ..ink.body() }, 160, scale), right(54), Align::Right);
    let each = inner / 6;
    for (i, &(hour, code, day, temp)) in w.hours.iter().enumerate() {
        let x = 14 + i as i32 * each;
        let name = match i {
            0 => "Now".to_owned(),
            _ if hour24 => format!("{hour:02}"),
            _ => format!("{}{}", (hour + 11) % 12 + 1, if hour < 12 { "AM" } else { "PM" }),
        };
        pieces.put(text.label(&name, ink.caption(), each as u32, scale), at(x, ts(92), each, ts(13)), Align::Centre);
        pieces.put(symbols.get(condition(code, day).1, Weight::Fill, 18, scale, ink.label), at(x + (each - 18) / 2, ts(110), 18, 18), Align::Centre);
        pieces.put(text.label(&degrees(temp), Style { tabular: true, ..ink.body() }, each as u32, scale), at(x, ts(132), each, ts(16)), Align::Centre);
    }
}

/// Another app's widget (E8.8): icon and title, up to four lines, and a progress bar along the bottom.
#[expect(clippy::too_many_arguments, reason = "the widget's pieces in one place")]
fn widget_pieces(pieces: &mut Pieces, shapes: &mut Vec<(Rect, Shape)>, text: &mut Text, symbols: &mut Symbols, ink: &Ink, w: &Widget, card: Rect, scale: f64) {
    let at = |x: i32, y: i32, width: i32, h: i32| Rectangle::new((card.loc.x + x, card.loc.y + y).into(), (width.max(1), h).into());
    let inner = card.size.w - 28;
    let indent = if w.icon.is_some() { 28 } else { 0 };
    if let Some(icon) = w.icon {
        pieces.put(symbols.get(icon, Weight::Fill, 20, scale, ink.label), at(14, 12, 20, 20), Align::Centre);
    }
    pieces.put(text.label(&w.title, ink.headline(), (inner - indent).max(1) as u32, scale), at(14 + indent, ts(14), inner - indent, ts(16)), Align::Left);
    for (i, line) in w.lines.iter().enumerate() {
        pieces.put(text.label(line, ink.body(), inner.max(1) as u32, scale), at(14, ts(42 + i as i32 * 20), inner, ts(16)), Align::Left);
    }
    if let Some(value) = w.progress {
        let track = at(14, card.size.h - 20, inner, 6);
        let filled = ((f64::from(track.size.w) * value) as i32).max(track.size.h);
        shapes.push((Rectangle::new(track.loc, (filled, track.size.h).into()), Shape::Bar));
        shapes.push((track, Shape::Track));
    }
    shapes.push((card, Shape::Widget));
}

#[derive(Default)]
pub struct NotificationViews {
    glass: Option<(Key, Vec<PixelShaderElement>)>,
    labels: Vec<Label>,
    calendar: CalendarWidget,
}

#[derive(Clone, Copy, PartialEq)]
enum Align {
    Left,
    Right,
    Centre,
    /// Scaled to the rect (icons).
    Fill,
}

/// The text pieces with their rects, collected while laying out a frame.
struct Pieces(Vec<(Arc<Rendered>, Rect, Align)>);

impl Pieces {
    fn put(&mut self, piece: Arc<Rendered>, rect: Rect, align: Align) {
        self.0.push((piece, rect, align));
    }
}

/// Theme inks and type styles (Part D).
struct Ink {
    label: [u8; 4],
    secondary: [u8; 4],
}

impl Ink {
    fn new(dark: bool) -> Self {
        let ink = |alpha: u8| if dark { [255, 255, 255, alpha] } else { [0, 0, 0, alpha] };
        Ink { label: ink(217), secondary: ink(if dark { 140 } else { 128 }) }
    }
    fn headline(&self) -> Style {
        Style { size: 13, line_height: 16, weight: 600, color: self.label, tabular: false }
    }
    fn body(&self) -> Style {
        Style { color: self.label, ..BODY }
    }
    fn subheadline(&self) -> Style {
        Style { size: 11, line_height: 14, weight: 400, color: self.secondary, tabular: false }
    }
    fn caption(&self) -> Style {
        Style { size: 10, line_height: 13, weight: 500, color: self.secondary, tabular: true }
    }
    fn button(&self) -> Style {
        Style { size: 12, line_height: 15, weight: 500, color: self.label, tabular: false }
    }
}

/// "now" in a banner; the clock time today in Notification Center, else the date.
fn when(n: &Notification, banner: bool, hour24: bool) -> String {
    use chrono::TimeZone;
    let now = chrono::Local::now();
    match chrono::Local.timestamp_opt(n.time, 0).single() {
        _ if banner && now.timestamp() - n.time < 60 => "now".into(),
        Some(t) if banner => format!("{}m ago", (now - t).num_minutes()),
        Some(t) if t.date_naive() == now.date_naive() => t.format(if hour24 { "%H:%M" } else { "%-I:%M %p" }).to_string(),
        Some(t) => t.format("%-d %b").to_string(),
        None => String::new(),
    }
}

#[expect(clippy::too_many_arguments, reason = "one card's pieces in one place")]
fn card_pieces(
    pieces: &mut Pieces,
    shapes: &mut Vec<(Rect, Shape)>,
    text: &mut Text,
    symbols: &mut Symbols,
    ink: &Ink,
    card: &Card,
    n: &Notification,
    banner: bool,
    hour24: bool,
    scale: f64,
) {
    let width = |r: Rect| r.size.w.max(1) as u32;
    if let Some(close) = card.close {
        pieces.put(symbols.get(Symbol::X, Weight::Regular, 10, scale, ink.label), close, Align::Centre);
        shapes.push((close, Shape::Close));
    }
    for (rect, key) in &card.actions {
        let label = buttons(n).find(|(k, _)| k == key).map_or("", |(_, l)| l.as_str());
        pieces.put(text.label(label, ink.button(), width(*rect) - 8, scale), *rect, Align::Centre);
        shapes.push((*rect, Shape::Button));
    }
    match &n.icon {
        Some(icon) => pieces.put(icon.clone(), card.icon, Align::Fill),
        None => {
            pieces.put(symbols.get(Symbol::Bell, Weight::Fill, 18, scale, ink.secondary), card.icon, Align::Centre);
            shapes.push((card.icon, Shape::IconBack));
        }
    }
    pieces.put(text.label(&n.app, ink.subheadline(), width(card.app), scale), card.app, Align::Left);
    pieces.put(text.label(&when(n, banner, hour24), ink.caption(), width(card.time), scale), card.time, Align::Right);
    // Previews off: no body, so a body alone leaves the title row empty.
    let shown = if card.preview { n.body.as_str() } else { "" };
    let (title, body) = match n.summary.is_empty() {
        // A body alone takes the title row.
        true => (shown, ""),
        false => (n.summary.as_str(), shown),
    };
    pieces.put(text.label(title, ink.headline(), width(card.title), scale), card.title, Align::Left);
    if !body.is_empty() && card.body.size.h > 0 {
        let lines = (card.body.size.h / LINE).max(1) as u8;
        pieces.put(text.wrapped(body, ink.body(), width(card.body), lines, scale), card.body, Align::Left);
    }
    if let (Some(track), Some(value)) = (card.progress, n.progress) {
        let filled = (track.size.w * i32::from(value) / 100).max(track.size.h);
        shapes.push((Rectangle::new(track.loc, (filled, track.size.h).into()), Shape::Bar));
        shapes.push((track, Shape::Track));
    }
    shapes.push((card.rect, Shape::Card { shadow: banner }));
}

impl NotificationViews {
    /// Front to back: text and icons, buttons, cards, the panel, shadows.
    #[expect(clippy::too_many_arguments, reason = "the renderer, the caches, what to show and the theme")]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        symbols: &mut Symbols,
        view: View<'_>,
        store: &Store,
        hour24: bool,
        dark: bool,
        accent: [u8; 3],
        glass_opacity: f32,
        scale: f64,
    ) -> Vec<SceneElement> {
        let ink = Ink::new(dark);
        let mut pieces = Pieces(Vec::new());
        let mut shapes = Vec::new();
        let mut calendar = Vec::new();
        match view {
            View::Banners { cards, more } => {
                for card in cards {
                    if let Some(n) = store.get(card.id) {
                        card_pieces(&mut pieces, &mut shapes, text, symbols, &ink, card, n, true, hour24, scale);
                    }
                }
                if let Some((pill, count)) = more {
                    pieces.put(text.label(&format!("+{count} more"), ink.button(), pill.size.w as u32, scale), pill, Align::Centre);
                    shapes.push((pill, Shape::Card { shadow: true }));
                }
            }
            View::Widgets { rects, widgets } => {
                for (card, w) in rects.iter().zip(widgets) {
                    widget_pieces(&mut pieces, &mut shapes, text, symbols, &ink, w, *card, scale);
                }
            }
            View::Center { center, month, up_next, batteries, dnd, weather, widgets } => {
                let shown = |r: Rect| center.visible(r);
                let row_w = |r: Rect| r.size.w.max(1) as u32;
                if shown(center.date) {
                    let date = month.today.format("%A, %-d %B").to_string();
                    pieces.put(
                        text.label(&date, Style { size: 17, line_height: 22, weight: 600, color: ink.label, tabular: false }, row_w(center.date), scale),
                        center.date,
                        Align::Left,
                    );
                    // ponytail: ⚙ shows the sliders glyph and opens nothing until the Settings app (M7).
                    pieces.put(symbols.get(Symbol::Sliders, Weight::Regular, 16, scale, ink.secondary), center.settings, Align::Centre);
                }
                if let Some((strip, off)) = center.dnd.filter(|(strip, _)| dnd && shown(*strip)) {
                    let icon = Rectangle::new((strip.loc.x + 12, strip.loc.y + (strip.size.h - 16) / 2).into(), (16, 16).into());
                    pieces.put(symbols.get(Symbol::Moon, Weight::Fill, 16, scale, ink.label), icon, Align::Centre);
                    let words = Rectangle::new((strip.loc.x + 36, strip.loc.y).into(), (off.loc.x - strip.loc.x - 44, strip.size.h).into());
                    pieces.put(text.label("Do Not Disturb is on", ink.body(), row_w(words), scale), words, Align::Left);
                    pieces.put(text.label("Turn Off", ink.button(), row_w(off), scale), off, Align::Centre);
                    shapes.push((off, Shape::Button));
                    shapes.push((strip, Shape::Widget));
                }
                if let Some((title, clear)) = center.header.filter(|(t, _)| shown(*t)) {
                    pieces.put(text.label("Notifications", Style { weight: 600, ..ink.subheadline() }, row_w(title), scale), title, Align::Left);
                    pieces.put(text.label("Clear All", ink.button(), row_w(clear), scale), clear, Align::Centre);
                    shapes.push((clear, Shape::Button));
                }
                for group in &center.groups {
                    if let Some((name, less)) = group.header.filter(|(n, _)| shown(*n)) {
                        pieces.put(text.label(&group.app, ink.headline(), row_w(name), scale), name, Align::Left);
                        pieces.put(text.label("Show Less", ink.button(), row_w(less), scale), less, Align::Centre);
                        shapes.push((less, Shape::Button));
                    }
                    if let Some(close) = group.close.filter(|c| shown(*c)) {
                        pieces.put(symbols.get(Symbol::X, Weight::Regular, 10, scale, ink.label), close, Align::Centre);
                        shapes.push((close, Shape::Close));
                    }
                    for card in group.cards.iter().filter(|c| shown(c.rect)) {
                        if let Some(n) = store.get(card.id) {
                            card_pieces(&mut pieces, &mut shapes, text, symbols, &ink, card, n, false, hour24, scale);
                        }
                    }
                    if let Some(plate) = group.plate.filter(|p| shown(*p)) {
                        let peek = Rectangle::new((plate.loc.x, plate.loc.y + plate.size.h - ts(PLATE)).into(), (plate.size.w, ts(PLATE)).into());
                        pieces.put(text.label(&format!("{} more", group.count - 1), ink.caption(), row_w(peek), scale), peek, Align::Centre);
                        shapes.push((plate, Shape::Plate));
                    }
                }
                if let Some((card, rows)) = center.up_next.as_ref().filter(|(c, _)| shown(*c)) {
                    let title = Rectangle::new((card.loc.x + 12, card.loc.y + ts(10)).into(), (card.size.w - 24, ts(20)).into());
                    pieces.put(
                        text.label("Up Next", Style { size: 15, line_height: 20, weight: 600, color: ink.label, tabular: false }, row_w(title), scale),
                        title,
                        Align::Left,
                    );
                    let now = chrono::Local::now().timestamp();
                    for (row, u) in rows.iter().zip(up_next.iter()) {
                        // An accent bar, the title, then when and where.
                        shapes.push((Rectangle::new(row.loc, (3, row.size.h).into()), Shape::Bar));
                        let line = |dy: i32, h: i32| Rectangle::new((row.loc.x + 11, row.loc.y + dy).into(), (row.size.w - 11, h).into());
                        pieces.put(text.label(&u.summary, ink.headline(), row_w(line(0, 0)), scale), line(ts(1), ts(16)), Align::Left);
                        let place = if u.location.is_empty() { String::new() } else { format!(" · {}", u.location) };
                        let detail = format!("{}{place}", crate::events::when(u, now, hour24));
                        pieces.put(text.label(&detail, ink.subheadline(), row_w(line(0, 0)), scale), line(ts(19), ts(14)), Align::Left);
                    }
                    shapes.push((*card, Shape::Widget));
                }
                let (card, layout) = &center.calendar;
                if shown(*card) {
                    calendar = self.calendar.elements(renderer, shaders, text, symbols, month, layout, *card, dark, accent, glass_opacity, scale);
                }
                if let (Some(card), Some(w)) = (center.weather.filter(|c| shown(*c)), weather) {
                    weather_pieces(&mut pieces, text, symbols, &ink, w, card, hour24, scale);
                    shapes.push((card, Shape::Widget));
                }
                if let Some((card, slots)) = center.batteries.as_ref().filter(|(c, _)| shown(*c)) {
                    let title = Rectangle::new((card.loc.x + 12, card.loc.y + ts(10)).into(), (card.size.w - 24, ts(20)).into());
                    pieces.put(
                        text.label("Batteries", Style { size: 15, line_height: 20, weight: 600, color: ink.label, tabular: false }, row_w(title), scale),
                        title,
                        Align::Left,
                    );
                    for (slot, (name, percent)) in slots.iter().zip(batteries) {
                        let symbol = if name == "Laptop" { Symbol::BatteryHigh } else { Symbol::Headphones };
                        pieces.put(
                            symbols.get(symbol, Weight::Regular, 18, scale, ink.label),
                            Rectangle::new((slot.loc.x, slot.loc.y + (slot.size.h - 20) / 2).into(), (20, 20).into()),
                            Align::Centre,
                        );
                        let words = Rectangle::new((slot.loc.x + 26, slot.loc.y).into(), (slot.size.w - 30, slot.size.h).into());
                        pieces.put(text.label(&format!("{name} {percent}%"), Style { tabular: true, ..ink.body() }, row_w(words), scale), words, Align::Left);
                    }
                    shapes.push((*card, Shape::Widget));
                }
                for (card, w) in center.widgets.iter().zip(widgets).filter(|(c, _)| shown(**c)) {
                    widget_pieces(&mut pieces, &mut shapes, text, symbols, &ink, w, *card, scale);
                }
                shapes.push((center.rect, Shape::Panel));
            }
        }

        let key = (shapes, dark, accent, glass_opacity.to_bits(), scale.to_bits());
        if self.glass.as_ref().is_none_or(|(k, _)| *k != key) {
            let glass = glass(shaders, &key.0, dark, accent, glass_opacity, scale);
            self.glass = Some((key, glass));
        }
        self.labels = keep_buffers(&self.labels, pieces.0.iter().map(|(p, _, _)| p.clone()));
        let mut out: Vec<SceneElement> =
            self.labels.iter().zip(&pieces.0).filter_map(|(label, (piece, rect, align))| place(renderer, label, piece, *rect, *align, scale)).collect();
        // The calendar overlaps nothing but the panel, so all of it can go in front of the other glass.
        out.extend(calendar);
        if let Some((_, glass)) = &self.glass {
            out.extend(glass.iter().cloned().map(SceneElement::Shader));
        }
        out
    }
}

/// A piece in its rect: left, right or centred at its own size, or scaled to the rect.
fn place(renderer: &mut GlesRenderer, label: &Label, piece: &Rendered, rect: Rect, align: Align, scale: f64) -> Option<SceneElement> {
    let w = ((f64::from(piece.width) / scale).round() as i32).min(rect.size.w);
    let rect = match align {
        Align::Left => Rectangle::new(rect.loc, (w, rect.size.h).into()),
        Align::Right => Rectangle::new((rect.loc.x + rect.size.w - w, rect.loc.y).into(), (w, rect.size.h).into()),
        Align::Centre => rect,
        Align::Fill => {
            let (pixels, Some(buffer)) = label else { return None };
            let loc = Point::from((rect.loc.x, rect.loc.y)).to_f64().to_physical(scale);
            // The whole buffer, scaled into the rect (without `src` only a rect-sized corner of it is sampled).
            let src = Rectangle::from_size((f64::from(pixels.width), f64::from(pixels.height)).into());
            return MemoryRenderBufferRenderElement::from_buffer(renderer, loc, buffer, None, Some(src), Some(rect.size), Kind::Unspecified)
                .inspect_err(|err| tracing::warn!("notification icon: {err}"))
                .ok()
                .map(SceneElement::Memory);
        }
    };
    label_element(renderer, label, rect, scale)
}

/// The glass for `shapes` (front to back), then every shadow behind them all.
fn glass(shaders: &Shaders, shapes: &[(Rect, Shape)], dark: bool, accent: [u8; 3], glass_opacity: f32, scale: f64) -> Vec<PixelShaderElement> {
    let material = |m: Material, factor: f32| {
        let spec = m.spec(dark);
        let [r, g, b] = spec.tint.map(|c| f32::from(c) / 255.0);
        [r, g, b, spec.opacity * glass_opacity * factor]
    };
    let fill = if dark { [1.0, 1.0, 1.0, 0.16] } else { [0.0, 0.0, 0.0, 0.08] };
    let [r, g, b] = accent.map(|c| f32::from(c) / 255.0);
    let mut out = Vec::new();
    let mut shadows = Vec::new();
    for (rect, shape) in shapes {
        let rect = *rect;
        out.push(match shape {
            Shape::Panel => {
                shadows.extend(shaders.shadow(rect, CARD_RADIUS, Material::Panel.spec(dark).elevation, dark, scale, false));
                shaders.glass_shape(rect, material(Material::Panel, 1.0), CARD_RADIUS, true, dark, scale)
            }
            Shape::Card { shadow } => {
                if *shadow {
                    shadows.extend(shaders.shadow(rect, CARD_RADIUS.min(f64::from(rect.size.h) / 2.0), 2, dark, scale, false));
                }
                let radius = CARD_RADIUS.min(f64::from(rect.size.h) / 2.0);
                shaders.glass_shape(rect, material(Material::Card, 1.0), radius, true, dark, scale)
            }
            Shape::Widget => shaders.glass_shape(rect, material(Material::Card, 1.0), 12.0, true, dark, scale),
            Shape::Plate => shaders.glass_shape(rect, material(Material::Card, 0.6), 12.0, true, dark, scale),
            Shape::Button => shaders.glass_shape(rect, fill, 6.0, false, dark, scale),
            Shape::Close => shaders.glass_shape(rect, material(Material::Card, 1.0), f64::from(CLOSE) / 2.0, true, dark, scale),
            Shape::IconBack => shaders.glass_shape(rect, fill, 8.0, false, dark, scale),
            Shape::Track => shaders.glass_shape(rect, fill, f64::from(rect.size.h) / 2.0, false, dark, scale),
            Shape::Bar => shaders.glass_shape(rect, [r, g, b, 1.0], f64::from(rect.size.h.min(rect.size.w)) / 2.0, false, dark, scale),
        });
    }
    out.extend(shadows);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_naturally() {
        let now = chrono::Local::now().timestamp();
        let n = |ago: i64| Notification { time: now - ago, ..Default::default() };
        assert_eq!(when(&n(5), true, false), "now");
        assert_eq!(when(&n(300), true, false), "5m ago");
        assert!(when(&n(0), false, true).contains(':'));
    }

    #[test]
    fn weather_words() {
        assert_eq!((degrees(-0.4), degrees(21.5), degrees(-3.6)), ("0°".into(), "22°".into(), "-4°".into()));
        assert_eq!(
            (updated(50 * 60), updated(3 * 3600), updated(3 * 86400)),
            ("Updated 50 min ago".into(), "Updated 3 h ago".into(), "Updated 3 d ago".into())
        );
    }
}
