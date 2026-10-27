//! The calendar widget in Notification Center (E2.5, E4.2): a `card`, radius 12. Title and ‹ › on top, weekday
//! initials, then the month grid; today in an accent circle, other months' days at `label-tertiary`. Week numbers,
//! when on, in a narrow column at the left. Mid-slide the grid is drawn sideways with the month beside it, both cut
//! at the card's edge.

use chrono::{Datelike, NaiveDate, Weekday};
use smithay::{
    backend::renderer::{
        element::{Kind, memory::MemoryRenderBufferRenderElement},
        gles::{GlesRenderer, element::PixelShaderElement},
    },
    utils::{Logical, Rectangle, Size},
};

use super::{
    Material, SceneElement, Shaders,
    titlebar::{Label, keep_buffers},
};
use crate::{
    shell::{self, CalendarLayout},
    symbols::{Symbol, Symbols, Weight},
    text::{Style, Text, ts},
    wm::Rect,
};

const RADIUS: f64 = 12.0;
const TODAY: i32 = 26;

/// What the card, the today circles and the event dots depend on: card, week numbers, each grid's rect, today's cell
/// and dots, dark, accent, opacity, scale.
type Key = (Rect, bool, Vec<(Rect, Option<usize>, Vec<bool>)>, bool, [u8; 3], u32, u64);
const DOT: i32 = 4;

#[derive(Default)]
pub struct CalendarWidget {
    glass: Option<(Key, Vec<PixelShaderElement>)>,
    labels: Vec<Label>,
}

/// Everything the widget shows besides the theme.
pub struct Month {
    pub year: i32,
    pub month: u32,
    pub week_start: Weekday,
    pub today: NaiveDate,
    /// One per grid day: it has an event (E7.2).
    pub dots: Vec<bool>,
    /// Mid-slide: how far right its grid is drawn, px, and the month beside it with its dots.
    pub slide: Option<(i32, (i32, u32), Vec<bool>)>,
}

/// One month's grid where it is drawn: its weeks, today's cell, its dots and its layout.
struct Grid<'a> {
    year: i32,
    month: u32,
    rows: Vec<[NaiveDate; 7]>,
    today: Option<usize>,
    dots: &'a [bool],
    layout: CalendarLayout,
}

impl CalendarWidget {
    /// Front to back: text and arrows, the today circle, the `card` behind them.
    #[allow(clippy::too_many_arguments)]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        symbols: &mut Symbols,
        shown: &Month,
        layout: &CalendarLayout,
        card: Rect,
        dark: bool,
        accent: [u8; 3],
        glass_opacity: f32,
        scale: f64,
    ) -> Vec<SceneElement> {
        let weeks = !layout.weeks.is_empty();
        // This month's grid; mid-slide, the month beside it a card's width away.
        let mut months = vec![(shown.year, shown.month, &shown.dots[..], 0)];
        if let Some((dx, (year, month), dots)) = &shown.slide {
            months[0].3 = *dx;
            months.push((*year, *month, &dots[..], dx - dx.signum() * card.size.w));
        }
        let grids: Vec<Grid> = months
            .into_iter()
            .map(|(year, month, dots, dx)| {
                let rows = shell::grid(year, month, shown.week_start);
                let today = rows.iter().flatten().position(|d| *d == shown.today);
                let layout = shell::calendar_layout(layout.rect.loc.x + dx, layout.rect.loc.y, rows.len(), weeks);
                Grid { year, month, rows, today, dots, layout }
            })
            .collect();

        let each = grids.iter().map(|g| (g.layout.rect, g.today, g.dots.to_vec())).collect();
        let key = (card, weeks, each, dark, accent, glass_opacity.to_bits(), scale.to_bits());
        if self.glass.as_ref().is_none_or(|(k, _)| *k != key) {
            let spec = Material::Card.spec(dark);
            let [r, g, b] = spec.tint.map(|c| f32::from(c) / 255.0);
            let back = shaders.glass_shape(card, [r, g, b, spec.opacity * glass_opacity], RADIUS, true, dark, scale);
            let mut glass = Vec::new();
            for grid in &grids {
                let days: Vec<&NaiveDate> = grid.rows.iter().flatten().collect();
                // Event dots under the day numbers: white inside today's circle, else the number's ink, fainter.
                for (i, cell) in grid.layout.cells.iter().enumerate().filter(|(i, _)| grid.dots.get(*i) == Some(&true)) {
                    let dot = Rectangle::new((cell.loc.x + (cell.size.w - DOT) / 2, cell.loc.y + cell.size.h - ts(9)).into(), (DOT, DOT).into());
                    let ink = if dark { 1.0 } else { 0.0 };
                    let fill = match (Some(i) == grid.today, days[i].month() == grid.month) {
                        (true, _) => [1.0, 1.0, 1.0, 1.0],
                        (false, true) => [ink, ink, ink, 0.5],
                        (false, false) => [ink, ink, ink, 0.25],
                    };
                    glass.push((dot, fill));
                }
                if let Some(cell) = grid.today.map(|i| grid.layout.cells[i]) {
                    // It holds the day's number, so it grows with the text, up to the cell's width.
                    let d = ts(TODAY).min(cell.size.w);
                    let circle = Rectangle::new((cell.loc.x + (cell.size.w - d) / 2, cell.loc.y + (cell.size.h - d) / 2).into(), (d, d).into());
                    let [r, g, b] = accent.map(|c| f32::from(c) / 255.0);
                    glass.push((circle, [r, g, b, 1.0]));
                }
            }
            // ponytail: shapes can't be cut, so one partly off the card mid-slide is left out; a clip uniform if it shows.
            let mut glass: Vec<_> = glass.into_iter().filter(|(r, _)| card.contains_rect(*r)).map(|(r, fill)| shaders.light(r, fill, false, scale)).collect();
            glass.push(back);
            self.glass = Some((key, glass));
        }

        // label, label-secondary, label-tertiary (Part D).
        let ink = |alpha_light: u8, alpha_dark: u8| if dark { [255, 255, 255, alpha_dark] } else { [0, 0, 0, alpha_light] };
        let (label, secondary, tertiary) = (ink(217, 217), ink(128, 140), ink(66, 64));
        let title3 = Style { color: label, ..super::ui::TITLE3 };
        let caption = Style { size: 10, line_height: 13, weight: 500, color: secondary, tabular: false };
        let body = Style { size: 13, line_height: 16, weight: 400, color: label, tabular: true };

        // The month nearer the middle names the card.
        let named = grids.iter().min_by_key(|g| (g.layout.rect.loc.x - layout.rect.loc.x).abs()).map_or((shown.year, shown.month), |g| (g.year, g.month));
        let first = NaiveDate::from_ymd_opt(named.0, named.1, 1);
        let title = first.map(|d| d.format("%B %Y").to_string()).unwrap_or_default();
        let mut pieces = vec![
            text.label(&title, title3, layout.title.size.w as u32, scale),
            symbols.get(Symbol::CaretLeft, Weight::Regular, 16, scale, label),
            symbols.get(Symbol::CaretRight, Weight::Regular, 16, scale, label),
        ];
        let mut day = shown.week_start;
        for _ in 0..7 {
            pieces.push(text.label(&initial(day), caption, 30, scale));
            day = day.succ();
        }
        for grid in &grids {
            for (i, date) in grid.rows.iter().flatten().enumerate() {
                let color = match (Some(i) == grid.today, date.month() == grid.month) {
                    (true, _) => [255, 255, 255, 255],
                    (false, true) => label,
                    (false, false) => tertiary,
                };
                pieces.push(text.label(&date.day().to_string(), Style { color, ..body }, 30, scale));
            }
            // Week numbers: caption-sized, label-tertiary, one per row while the layout has their column.
            for row in grid.rows.iter().take(grid.layout.weeks.len()) {
                let number = crate::calendar::week_number(row).to_string();
                pieces.push(text.label(&number, Style { color: tertiary, tabular: true, ..caption }, shell::calendar::WEEK_W as u32, scale));
            }
        }
        self.labels = keep_buffers(&self.labels, pieces);

        // The title is left-aligned: its rect shrinks to the text.
        let title_w = self.labels.first().map_or(0, |(r, _)| (f64::from(r.width) / scale).round() as i32);
        let title_rect = Rectangle::new(layout.title.loc, (title_w.min(layout.title.size.w), layout.title.size.h).into());
        let rects = [title_rect, layout.prev, layout.next]
            .into_iter()
            .chain(layout.header)
            .chain(grids.iter().flat_map(|g| g.layout.cells.iter().chain(&g.layout.weeks).copied()));
        let mut out: Vec<SceneElement> = self.labels.iter().zip(rects).filter_map(|(label, rect)| cut_label(renderer, label, rect, card, scale)).collect();
        if let Some((_, glass)) = &self.glass {
            out.extend(glass.iter().cloned().map(SceneElement::Shader));
        }
        out
    }
}

/// A label centred in `rect` (like `titlebar::label_element`), cut to `clip`.
fn cut_label(renderer: &mut GlesRenderer, label: &Label, rect: Rect, clip: Rect, scale: f64) -> Option<SceneElement> {
    let (rendered, Some(buffer)) = label else { return None };
    let size = Size::from(((f64::from(rendered.width) / scale).round() as i32, (f64::from(rendered.height) / scale).round() as i32));
    let drawn = Rectangle::new((rect.loc.x + (rect.size.w - size.w) / 2, rect.loc.y + (rect.size.h - size.h) / 2).into(), size);
    let (seen, src) = crop(drawn, clip, (rendered.width, rendered.height))?;
    MemoryRenderBufferRenderElement::from_buffer(renderer, seen.loc.to_f64().to_physical(scale), buffer, None, Some(src), Some(seen.size), Kind::Unspecified)
        .inspect_err(|err| tracing::warn!("calendar label: {err}"))
        .ok()
        .map(SceneElement::Memory)
}

/// The part of `drawn` inside `clip`, and the same part of a `w` × `h` px buffer drawn into `drawn`, in its pixels.
fn crop(drawn: Rect, clip: Rect, (w, h): (u32, u32)) -> Option<(Rect, Rectangle<f64, Logical>)> {
    let seen = drawn.intersection(clip)?;
    let (sx, sy) = (f64::from(w) / f64::from(drawn.size.w.max(1)), f64::from(h) / f64::from(drawn.size.h.max(1)));
    let at = |x: i32, y: i32| (f64::from(x) * sx, f64::from(y) * sy);
    Some((seen, Rectangle::new(at(seen.loc.x - drawn.loc.x, seen.loc.y - drawn.loc.y).into(), at(seen.size.w, seen.size.h).into())))
}

/// S M T W T F S.
fn initial(day: Weekday) -> String {
    ["M", "T", "W", "T", "F", "S", "S"][day.num_days_from_monday() as usize].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_follow_week_start() {
        let mut day = Weekday::Sun;
        let row: String = (0..7)
            .map(|_| {
                let s = initial(day);
                day = day.succ();
                s
            })
            .collect();
        assert_eq!(row, "SMTWTFS");
    }

    #[test]
    fn labels_are_cut_at_the_card() {
        let rect = |x, y, w, h| Rectangle::new((x, y).into(), (w, h).into());
        let card = rect(0, 0, 100, 50);
        // Inside: all of it, the whole buffer (here at 2x).
        let (seen, src) = crop(rect(10, 10, 20, 10), card, (40, 20)).unwrap();
        assert_eq!((seen, src), (rect(10, 10, 20, 10), Rectangle::new((0.0, 0.0).into(), (40.0, 20.0).into())));
        // Half off the left edge: its right half, from the buffer's middle.
        let (seen, src) = crop(rect(-10, 10, 20, 10), card, (40, 20)).unwrap();
        assert_eq!((seen, src), (rect(0, 10, 10, 10), Rectangle::new((20.0, 0.0).into(), (20.0, 20.0).into())));
        // Off the card: nothing.
        assert!(crop(rect(100, 10, 20, 10), card, (40, 20)).is_none());
    }
}
