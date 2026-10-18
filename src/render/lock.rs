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
    services::power::Battery,
    shell::lock::{self, AVATAR, Button, Lock},
    symbols::{Symbol, Symbols, Weight},
    text::{Rendered, Style, Text},
    ui::field::TextField,
    wm::Rect,
};

/// What the frame loop hands over.
pub struct Content<'a> {
    pub lock: &'a Lock,
    pub battery: Option<Battery>,
    /// The keyboard layout's short name ("EN").
    pub keyboard: &'a str,
    pub hour24: bool,
    pub now: chrono::DateTime<chrono::Local>,
}

/// Date, time, missed count, hover, wrong, battery, keyboard, size, dark, opacity, scale, greeter (user, session,
/// error).
type PageKey =
    (String, String, usize, Option<Button>, bool, Option<(u8, bool)>, String, Size<i32, Logical>, bool, u32, u64, Option<(usize, usize, Option<String>)>);
type FieldKey = (TextField, Rect, bool, bool, [u8; 3], u32, u64);
/// Full name and round picture per user.
type Users = Vec<(String, Option<Arc<Rendered>>)>;
type ShadeKey = (Size<i32, Logical>, u32, u64);
type Drawn<K> = (K, Vec<PixelShaderElement>, Vec<(Arc<Rendered>, Rect)>);

const WHITE: [u8; 4] = [255; 4];
const WHITE85: [u8; 4] = [255, 255, 255, 217];
const DISPLAY: Style = Style { size: 96, line_height: 100, weight: 300, color: WHITE, tabular: true };
const TITLE2: Style = Style { size: 17, line_height: 22, weight: 600, color: WHITE85, tabular: false };

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
        let layout = lock::layout(size);
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
        let key = (
            date,
            time,
            lock.missed,
            lock.hover,
            lock.wrong.is_some(),
            battery,
            content.keyboard.to_owned(),
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
            if lock.missed > 0 {
                let words = if lock.missed == 1 { "1 notification".to_owned() } else { format!("{} notifications", lock.missed) };
                ui.text(&words, Style { color: WHITE85, ..CALLOUT }, layout.missed, Align::Centre);
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
            status(&mut ui, layout.status, content.keyboard, content.battery);
            self.page = Some((key, std::mem::take(&mut ui.glass), std::mem::take(&mut ui.pieces)));
        }

        // The field, shaken sideways after a wrong password.
        let shake = lock.shake(std::time::Instant::now()).unwrap_or(0);
        let field_rect = Rectangle::new((layout.field.loc.x + shake, layout.field.loc.y).into(), layout.field.size);
        let field_key = (lock.field.clone(), field_rect, lock.checking.is_some(), dark, accent, glass_opacity.to_bits(), scale.to_bits());
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
        item(ui, crate::render::menubar::battery(&b).symbol, &format!("{}%", b.percent));
    }
    if !keyboard.is_empty() {
        item(ui, Symbol::Keyboard, keyboard);
    }
}

/// The user's picture (AccountsService's, else `~/.face`), cut to a `AVATAR` px circle at `scale`.
fn avatar(login: &str, home: &std::path::Path, scale: f64) -> Option<Arc<Rendered>> {
    let paths = [std::path::PathBuf::from("/var/lib/AccountsService/icons").join(login), home.join(".face"), home.join(".face.icon")];
    let image = paths.iter().find_map(|p| super::wallpaper::decode(p).ok())?;
    let px = (f64::from(AVATAR) * scale).round() as u32;
    Some(Arc::new(round(&image.rgbx, image.size.w as u32, image.size.h as u32, px)))
}

/// The centre square of an RGBX picture scaled to `px` (nearest pixel) inside a circle with a 1 px soft edge,
/// premultiplied.
fn round(rgbx: &[u8], w: u32, h: u32, px: u32) -> Rendered {
    let side = w.min(h).max(1);
    let (x0, y0) = ((w - side) / 2, (h - side) / 2);
    let r = f64::from(px) / 2.0;
    let mut pixels = Vec::with_capacity((px * px * 4) as usize);
    for y in 0..px {
        for x in 0..px {
            let (sx, sy) = (x0 + x * side / px.max(1), y0 + y * side / px.max(1));
            let i = ((sy * w + sx) * 4) as usize;
            let d = ((f64::from(x) + 0.5 - r).powi(2) + (f64::from(y) + 0.5 - r).powi(2)).sqrt();
            let a = (r - d + 0.5).clamp(0.0, 1.0);
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
        let r = round(&rgbx, 4, 2, 16);
        assert_eq!((r.width, r.height, r.pixels.len()), (16, 16, 16 * 16 * 4));
        let at = |x: usize, y: usize| &r.pixels[(y * 16 + x) * 4..(y * 16 + x) * 4 + 4];
        assert_eq!(at(0, 0), [0, 0, 0, 0], "corners are clear");
        assert_eq!(at(8, 8), [255, 0, 0, 255], "the middle is opaque");
        assert!(at(8, 0)[3] > 0 && at(8, 0)[3] < 255, "the edge is soft");
    }
}
