//! The lock screen (E6.4), per the sketch: date (Title 2) and clock (Display, 96 Light) at the top, the user's
//! picture (64 px circle), name (Headline) and the `sheet` password pill (200 × 30) in the lower third, Sleep /
//! Restart / Shut Down (Callout) at the bottom and keyboard layout + battery at the bottom right, all white over the
//! wallpaper darkened 30%. The page and the field are cached apart, so typing and the shake redraw only the field.

use std::sync::Arc;

use smithay::{
    backend::renderer::gles::{GlesRenderer, element::PixelShaderElement},
    utils::{Logical, Rectangle, Size},
};

use super::{
    SceneElement, Shaders,
    titlebar::{Label, keep_buffers, label_element},
    ui::{Align, BODY, CALLOUT, HEADLINE, Ui},
};
use crate::{
    services::{media::Media, power::Battery},
    shell::lock::{self, ART, AVATAR, Button, Lock},
    symbols::{Symbol, Symbols, Weight},
    text::{Rendered, Style, Text, ts},
    ui::field::TextField,
    wm::Rect,
};

/// What the frame loop hands over.
pub struct Content<'a> {
    pub lock: &'a Lock,
    pub battery: Option<Battery>,
    /// The keyboard layout's short name ("EN").
    pub keyboard: &'a str,
    /// The layout pill's label, with more than one layout (`lock::layout_pill`).
    pub layout: Option<String>,
    /// `[idle] lock_message`; empty: none.
    pub message: &'a str,
    pub hour24: bool,
    pub now: chrono::DateTime<chrono::Local>,
    pub caps_lock: bool,
    /// Now Playing; `None` hides its card.
    pub media: Option<&'a Media>,
}

/// Date, time, missed count, hover, wrong, battery, (keyboard, message, layout pill, Now Playing, listed notifications),
/// size, dark, opacity, scale, greeter (user, session, error).
type PageKey = (
    String,
    String,
    usize,
    Option<Button>,
    bool,
    Option<(u8, bool)>,
    (String, String, Option<String>, Option<Media>, Vec<lock::Listed>),
    Size<i32, Logical>,
    bool,
    u32,
    u64,
    Option<(usize, usize, Option<String>)>,
);
type FieldKey = (TextField, Rect, bool, bool, bool, [u8; 3], u32, u64);
/// Full name and round picture per user.
type Users = Vec<(String, Option<Arc<Rendered>>)>;
type ShadeKey = (Size<i32, Logical>, u32, u64);
type Drawn<K> = (K, Vec<PixelShaderElement>, Vec<(Arc<Rendered>, Rect)>);

const WHITE: [u8; 4] = [255; 4];
const WHITE85: [u8; 4] = [255, 255, 255, 217];
const DISPLAY: Style = Style { size: 96, line_height: 100, weight: 300, color: WHITE, tabular: true };
const TITLE2: Style = Style { color: WHITE85, ..super::ui::TITLE2 };

#[derive(Default)]
pub struct LockView {
    page: Option<Drawn<PageKey>>,
    field: Option<Drawn<FieldKey>>,
    labels: Vec<Label>,
    /// The scale, and each user's full name and round picture (`None`: the initial in a circle); read on first
    /// lock. One user, or the greeter's row (E7.2).
    users: Option<(u64, Users)>,
    /// The idle shade, kept so it keeps its damage id while the screen stays dimmed.
    shade: Option<(ShadeKey, PixelShaderElement)>,
    /// Now Playing's album art: its path, the scale, and the picture with rounded corners (`None`: unreadable).
    art: Option<(String, u64, Option<Arc<Rendered>>)>,
}

impl LockView {
    /// Black at `alpha` over an output of `size` (E6.5's dim and "off").
    pub fn shade(&mut self, shaders: &Shaders, size: Size<i32, Logical>, alpha: f32, scale: f64) -> PixelShaderElement {
        let key = (size, alpha.to_bits(), scale.to_bits());
        match &self.shade {
            Some((k, e)) if *k == key => e.clone(),
            _ => self.shade.insert((key, shaders.glass_shape(Rectangle::from_size(size), [0.0, 0.0, 0.0, alpha], 0.0, false, false, scale))).1.clone(),
        }
    }

    /// Front to back: text, the field, the page's glass, the 30% shade. The caller puts the wallpaper behind.
    #[allow(clippy::too_many_arguments)]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        symbols: &mut Symbols,
        content: &Content,
        size: Size<i32, Logical>,
        dark: bool,
        accent: [u8; 3],
        glass_opacity: f32,
        scale: f64,
    ) -> Vec<SceneElement> {
        let layout = lock::layout(size, &content.lock.listed);
        let lock = content.lock;
        let count = lock.greeter.as_ref().map_or(1, |g| g.users.len());
        if self.users.as_ref().is_none_or(|(s, u)| *s != scale.to_bits() || u.len() != count) {
            let users = match &lock.greeter {
                Some(g) => g.users.iter().map(|u| (u.name.clone(), avatar(&u.login, &u.home, scale))).collect(),
                None => {
                    let (login, full) = crate::pam::user();
                    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
                    vec![(full, avatar(&login, &home, scale))]
                }
            };
            self.users = Some((scale.to_bits(), users));
        }
        let Some((_, users)) = &self.users else { return Vec::new() };
        let greeter = lock.greeter.as_ref().map(|g| (g.user, g.session, g.error.clone()));
        let date = content.now.format("%A, %-d %B").to_string();
        let time = content.now.format(if content.hour24 { "%H:%M" } else { "%-I:%M" }).to_string();
        let battery = content.battery.map(|b| (b.percent, b.charging));
        // Never on the login screen; only the player's own words show (privacy).
        let media = content.media.filter(|_| lock.greeter.is_none() && layout.media.is_some());
        let art = match media.map(|m| m.art.as_str()).filter(|a| !a.is_empty()) {
            Some(path) => {
                if self.art.as_ref().is_none_or(|(p, s, _)| p != path || *s != scale.to_bits()) {
                    self.art = Some((path.to_owned(), scale.to_bits(), album_art(path, scale)));
                }
                self.art.as_ref().and_then(|(_, _, a)| a.clone())
            }
            None => None,
        };
        let key = (
            date,
            time,
            lock.missed,
            lock.hover,
            lock.wrong.is_some(),
            battery,
            (content.keyboard.to_owned(), content.message.to_owned(), content.layout.clone(), media.cloned(), lock.listed.clone()),
            size,
            dark,
            glass_opacity.to_bits(),
            scale.to_bits(),
            greeter,
        );
        if self.page.as_ref().is_none_or(|(k, _, _)| *k != key) {
            let mut ui = Ui { shaders, text, symbols, dark, accent, glass_opacity, scale, glass: Vec::new(), pieces: Vec::new() };
            // D11: the wallpaper darkened 30% (40% over the pale light one, for the white text).
            ui.shape(Rectangle::from_size(size), [0.0, 0.0, 0.0, if dark { 0.3 } else { 0.4 }], 0.0);
            ui.text(&key.0, TITLE2, layout.date, Align::Centre);
            ui.text(&key.1, DISPLAY, layout.clock, Align::Centre);
            for (listed, card) in lock.listed.iter().zip(layout.cards.into_iter().flatten()) {
                notice(&mut ui, card, listed);
            }
            // Listed ones that didn't fit join the count.
            let shown = layout.cards.iter().flatten().count().min(lock.listed.len());
            let count = lock.missed + lock.listed.len() - shown;
            if count > 0 {
                let more = if shown > 0 { " more" } else { "" };
                let words = if count == 1 { format!("1{more} notification") } else { format!("{count}{more} notifications") };
                ui.text(&words, Style { color: WHITE85, ..CALLOUT }, layout.missed, Align::Centre);
            }
            if lock.greeter.is_none() && !content.message.is_empty() {
                ui.text(content.message, Style { color: WHITE85, ..CALLOUT }, layout.message, Align::Centre);
            }
            // The greeter's row (E7.2): the picked user ringed and named in white, the others in 85%.
            let row = crate::shell::greeter::row(&layout, size.w, users.len());
            let picked = lock.greeter.as_ref().map_or(0, |g| g.user);
            for (i, ((name, picture), (rect, name_rect))) in users.iter().zip(row.avatars.into_iter().zip(row.names)).enumerate() {
                if users.len() > 1 && i == picked {
                    ui.glass.push(shaders.ring(
                        Rectangle::new((rect.loc.x - 3, rect.loc.y - 3).into(), (AVATAR + 6, AVATAR + 6).into()),
                        f64::from(AVATAR) / 2.0 + 3.0,
                        2,
                        [1.0, 1.0, 1.0, 0.85],
                        scale,
                    ));
                }
                match picture {
                    Some(picture) => ui.pieces.push((picture.clone(), rect)),
                    None => {
                        ui.glass.push(shaders.light(rect, [1.0, 1.0, 1.0, 0.3], false, scale));
                        let initial: String = name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
                        ui.text(&initial, Style { size: 28, line_height: 34, weight: 500, color: WHITE, tabular: false }, rect, Align::Centre);
                    }
                }
                let color = if i == picked { WHITE } else { WHITE85 };
                ui.text(name, Style { color, ..HEADLINE }, name_rect, Align::Centre);
            }
            let error = lock.greeter.as_ref().and_then(|g| g.error.as_deref());
            if let Some(hint) = error.or(lock.wrong.map(|_| "Incorrect password")) {
                ui.text(hint, Style { color: WHITE85, ..CALLOUT }, layout.hint, Align::Centre);
            }
            if let Some(session) = lock.greeter.as_ref().and_then(|g| g.session()) {
                let r = row.session;
                ui.symbol(Symbol::Monitor, Weight::Regular, 16, Rectangle::new((r.loc.x, r.loc.y + (r.size.h - 16) / 2).into(), (16, 16).into()), WHITE85);
                let words = Rectangle::new((r.loc.x + 20, r.loc.y).into(), (r.size.w - 20, r.size.h).into());
                ui.text(&session.name, Style { color: WHITE85, ..BODY }, words, Align::Left);
            }
            for (button, rect) in Button::ALL.into_iter().zip(layout.buttons) {
                if lock.hover == Some(button) {
                    ui.shape(rect, [1.0, 1.0, 1.0, 0.16], 6.0);
                }
                ui.text(button.label(), Style { color: WHITE85, ..CALLOUT }, rect, Align::Centre);
            }
            if let (Some(media), Some(card)) = (media, layout.media) {
                now_playing(&mut ui, card, media, art);
            }
            // More than one layout: the pill beside the field says which, so the corner doesn't.
            match &content.layout {
                Some(code) => {
                    let r = layout.layouts;
                    let spec = super::Material::Sheet.spec(dark);
                    let [red, g, b] = spec.tint.map(|c| f32::from(c) / 255.0);
                    ui.glass.push(shaders.glass_shape(r, [red, g, b, spec.opacity * glass_opacity], f64::from(r.size.h) / 2.0, true, dark, scale));
                    let color = ui.ink(0.85);
                    ui.text(code, Style { color, ..CALLOUT }, r, Align::Centre);
                    status(&mut ui, layout.status, "", content.battery);
                }
                None => status(&mut ui, layout.status, content.keyboard, content.battery),
            }
            self.page = Some((key, std::mem::take(&mut ui.glass), std::mem::take(&mut ui.pieces)));
        }

        // The field, shaken sideways after a wrong password.
        let shake = lock.shake(std::time::Instant::now()).unwrap_or(0);
        let field_rect = Rectangle::new((layout.field.loc.x + shake, layout.field.loc.y).into(), layout.field.size);
        let field_key = (lock.field.clone(), field_rect, lock.checking.is_some(), content.caps_lock, dark, accent, glass_opacity.to_bits(), scale.to_bits());
        if self.field.as_ref().is_none_or(|(k, _, _)| *k != field_key) {
            let mut ui = Ui { shaders, text, symbols, dark, accent, glass_opacity, scale, glass: Vec::new(), pieces: Vec::new() };
            let typed = !lock.field.text.is_empty();
            let pill = field_rect;
            ui.pill_field(pill, &lock.field, "Enter Password", if typed { 22 } else { 0 });
            if typed {
                let alpha = if lock.checking.is_some() { 0.26 } else { 0.55 };
                let arrow = Rectangle::new((pill.loc.x + pill.size.w - 24, pill.loc.y + (pill.size.h - 16) / 2).into(), (16, 16).into());
                let color = ui.ink(alpha);
                ui.symbol(Symbol::ArrowRight, Weight::Regular, 16, arrow, color);
            }
            // Caps Lock on: ⇪ at the field's right, left of the arrow (E8.6).
            if content.caps_lock {
                let right = if typed { 44 } else { 24 };
                let mark = Rectangle::new((pill.loc.x + pill.size.w - right, pill.loc.y).into(), (16, pill.size.h).into());
                let color = ui.ink(0.55);
                ui.text("⇪", Style { color, ..BODY }, mark, Align::Centre);
            }
            self.field = Some((field_key, std::mem::take(&mut ui.glass), std::mem::take(&mut ui.pieces)));
        }

        let (Some((_, page_glass, page_pieces)), Some((_, field_glass, field_pieces))) = (&self.page, &self.field) else { return Vec::new() };
        let pieces: Vec<&(Arc<Rendered>, Rect)> = page_pieces.iter().chain(field_pieces).collect();
        self.labels = keep_buffers(&self.labels, pieces.iter().map(|(p, _)| p.clone()));
        let mut out: Vec<SceneElement> =
            self.labels.iter().zip(&pieces).filter_map(|(label, (_, rect))| label_element(renderer, label, *rect, scale)).collect();
        out.extend(field_glass.iter().rev().cloned().map(SceneElement::Shader));
        out.extend(page_glass.iter().rev().cloned().map(SceneElement::Shader));
        out
    }
}

/// A listed notification's card: its app, then its title, then its body when previews allow it.
fn notice(ui: &mut Ui, card: Rect, listed: &lock::Listed) {
    let spec = super::Material::Sheet.spec(ui.dark);
    let [r, g, b] = spec.tint.map(|c| f32::from(c) / 255.0);
    ui.glass.push(ui.shaders.glass_shape(card, [r, g, b, spec.opacity * ui.glass_opacity], spec.radius, true, ui.dark, ui.scale));
    let (label, secondary) = (ui.label(), ui.secondary());
    let (callout_h, line) = (ts(15), ts(16));
    let row = |y: i32, h: i32| Rectangle::new((card.loc.x + 12, y).into(), (card.size.w - 24, h).into());
    let y = card.loc.y + 8;
    ui.text(&listed.app, Style { color: secondary, ..CALLOUT }, row(y, callout_h), Align::Left);
    ui.text(&listed.title, Style { color: label, ..HEADLINE }, row(y + callout_h + 2, line), Align::Left);
    if !listed.body.is_empty() {
        ui.text(&listed.body, Style { color: label, ..BODY }, row(y + callout_h + line + 4, line), Align::Left);
    }
}

/// The Now Playing card: art (or a note), title and artist, previous / play-pause / next.
fn now_playing(ui: &mut Ui, card: Rect, media: &Media, art: Option<Arc<Rendered>>) {
    let spec = super::Material::Sheet.spec(ui.dark);
    let [r, g, b] = spec.tint.map(|c| f32::from(c) / 255.0);
    ui.glass.push(ui.shaders.glass_shape(card, [r, g, b, spec.opacity * ui.glass_opacity], spec.radius, true, ui.dark, ui.scale));
    let square = Rectangle::new((card.loc.x + 8, card.loc.y + (card.size.h - ART) / 2).into(), (ART, ART).into());
    match art {
        Some(art) => ui.pieces.push((art, square)),
        None => {
            ui.shape(square, ui.fill(2.0), 8.0);
            let color = ui.secondary();
            ui.symbol(Symbol::MusicNote, Weight::Fill, 20, Rectangle::new((square.loc.x + 14, square.loc.y + 14).into(), (20, 20).into()), color);
        }
    }
    let buttons = crate::shell::control::media_buttons(card);
    let (x, right) = (square.loc.x + ART + 12, buttons[0].loc.x - 8);
    let row = |y: i32, h: i32| Rectangle::new((x, y).into(), (right - x, h).into());
    // No artist (a browser tab): the title alone, centred.
    let line = ts(16);
    let y = card.loc.y + (card.size.h - if media.artist.is_empty() { line } else { 2 * line + 2 }) / 2;
    let (label, secondary) = (ui.label(), ui.secondary());
    ui.text(&media.title, Style { color: label, ..HEADLINE }, row(y, line), Align::Left);
    ui.text(&media.artist, Style { color: secondary, ..BODY }, row(y + line + 2, line), Align::Left);
    let middle = if media.playing { Symbol::Pause } else { Symbol::Play };
    for (button, symbol) in buttons.into_iter().zip([Symbol::SkipBack, middle, Symbol::SkipForward]) {
        let at = Rectangle::new((button.loc.x + (button.size.w - 18) / 2, button.loc.y + (button.size.h - 18) / 2).into(), (18, 18).into());
        ui.symbol(symbol, Weight::Fill, 18, at, label);
    }
}

/// The player's cached album art, cut to an `ART` px square with rounded corners at `scale`.
fn album_art(path: &str, scale: f64) -> Option<Arc<Rendered>> {
    let path = std::path::Path::new(path);
    // A regular file of sane size only: the player names the path, and reading `/dev/zero` would hang the frame.
    if !std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() <= 8 << 20) {
        return None;
    }
    let px = (f64::from(ART) * scale).round() as u32;
    let image = super::wallpaper::decode(path, (px as i32, px as i32).into()).ok()?;
    Some(Arc::new(cut(&image.rgbx, image.size.w as u32, image.size.h as u32, px, 8.0 * scale)))
}

/// The keyboard layout and battery, right-aligned in `rect`.
fn status(ui: &mut Ui, rect: Rect, keyboard: &str, battery: Option<Battery>) {
    let style = Style { color: WHITE85, tabular: true, ..BODY };
    let mut right = rect.loc.x + rect.size.w;
    let mut item = |ui: &mut Ui, symbol: Symbol, words: &str| {
        let w = ui.text(words, style, Rectangle::new((rect.loc.x, rect.loc.y).into(), (right - rect.loc.x, rect.size.h).into()), Align::Right);
        let icon = Rectangle::new((right - w - 4 - 16, rect.loc.y + (rect.size.h - 16) / 2).into(), (16, 16).into());
        ui.symbol(symbol, Weight::Regular, 16, icon, WHITE85);
        right = icon.loc.x - 16;
    };
    if let Some(b) = battery {
        item(ui, crate::render::menubar::battery(&b, true).symbol, &format!("{}%", b.percent));
    }
    if !keyboard.is_empty() {
        item(ui, Symbol::Keyboard, keyboard);
    }
}

/// The user's picture (AccountsService's, else `~/.face`), cut to a `AVATAR` px circle at `scale`.
fn avatar(login: &str, home: &std::path::Path, scale: f64) -> Option<Arc<Rendered>> {
    let paths = [std::path::PathBuf::from("/var/lib/AccountsService/icons").join(login), home.join(".face"), home.join(".face.icon")];
    let px = (f64::from(AVATAR) * scale).round() as u32;
    let image = paths.iter().find_map(|p| super::wallpaper::decode(p, (px as i32, px as i32).into()).ok())?;
    Some(Arc::new(cut(&image.rgbx, image.size.w as u32, image.size.h as u32, px, f64::from(px) / 2.0)))
}

/// The centre square of an RGBX picture scaled to `px` (nearest pixel), its corners rounded by `radius` (`px / 2`: a
/// circle) with a 1 px soft edge, premultiplied.
fn cut(rgbx: &[u8], w: u32, h: u32, px: u32, radius: f64) -> Rendered {
    let side = w.min(h).max(1);
    let (x0, y0) = ((w - side) / 2, (h - side) / 2);
    let (c, r) = (f64::from(px) / 2.0, radius.min(f64::from(px) / 2.0));
    let mut pixels = Vec::with_capacity((px * px * 4) as usize);
    for y in 0..px {
        for x in 0..px {
            let (sx, sy) = (x0 + x * side / px.max(1), y0 + y * side / px.max(1));
            let i = ((sy * w + sx) * 4) as usize;
            // Distance outside the rounded square (negative inside).
            let (qx, qy) = ((f64::from(x) + 0.5 - c).abs() - (c - r), (f64::from(y) + 0.5 - c).abs() - (c - r));
            let d = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r;
            let a = (0.5 - d).clamp(0.0, 1.0);
            let p = rgbx.get(i..i + 3).unwrap_or(&[0, 0, 0]);
            pixels.extend(p.iter().map(|&c| (f64::from(c) * a).round() as u8));
            pixels.push((255.0 * a).round() as u8);
        }
    }
    Rendered { pixels, width: px, height: px, baseline: px, truncated: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picture_is_cut_round() {
        // A 4 × 2 red picture: the centre square, scaled to 16 px.
        let rgbx = [255, 0, 0, 255].repeat(8);
        let r = cut(&rgbx, 4, 2, 16, 8.0);
        assert_eq!((r.width, r.height, r.pixels.len()), (16, 16, 16 * 16 * 4));
        let at = |x: usize, y: usize| &r.pixels[(y * 16 + x) * 4..(y * 16 + x) * 4 + 4];
        assert_eq!(at(0, 0), [0, 0, 0, 0], "corners are clear");
        assert_eq!(at(8, 8), [255, 0, 0, 255], "the middle is opaque");
        assert!(at(8, 0)[3] > 0 && at(8, 0)[3] < 255, "the edge is soft");
    }

    #[test]
    fn art_has_rounded_corners() {
        let r = cut(&[0, 0, 255, 255].repeat(16 * 16), 16, 16, 16, 4.0);
        let alpha = |x: usize, y: usize| r.pixels[(y * 16 + x) * 4 + 3];
        assert_eq!(alpha(0, 0), 0, "the corner is cut");
        assert_eq!((alpha(8, 0), alpha(0, 8), alpha(8, 8)), (255, 255, 255), "the edges are straight, the middle opaque");
        assert!(alpha(1, 1) > 0 && alpha(1, 1) < 255, "the corner is soft");
    }
}
