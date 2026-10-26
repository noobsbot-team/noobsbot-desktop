//! The calendar widget in Notification Center (E2.5, E4.2): a `card`, radius 12. Title and ‹ › on top, weekday
//! initials, then the month grid; today in an accent circle, other months' days at `label-tertiary`. Week numbers,
//! when on, in a narrow column at the left.

use chrono::{Datelike, NaiveDate, Weekday};
use smithay::{
    backend::renderer::gles::{GlesRenderer, element::PixelShaderElement},
    utils::Rectangle,
};

use super::{
    Material, SceneElement, Shaders,
    titlebar::{Label, keep_buffers, label_element},
};
use crate::{
    shell::{self, CalendarLayout},
    symbols::{Symbol, Symbols, Weight},
    text::{Style, Text, ts},
    wm::Rect,
};

const RADIUS: f64 = 12.0;
const TODAY: i32 = 26;

/// What the card, the today circle and the event dots depend on: card, grid rect, today's cell, dots, dark, accent,
/// opacity, scale.
type Key = (Rect, Rect, Option<usize>, Vec<bool>, bool, [u8; 3], u32, u64);
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
        let grid = shell::grid(shown.year, shown.month, shown.week_start);
        let days: Vec<NaiveDate> = grid.iter().flatten().copied().collect();
        let today = days.iter().position(|d| *d == shown.today);

        let key = (card, layout.rect, today, shown.dots.clone(), dark, accent, glass_opacity.to_bits(), scale.to_bits());
        if self.glass.as_ref().is_none_or(|(k, _)| *k != key) {
            let spec = Material::Card.spec(dark);
            let [r, g, b] = spec.tint.map(|c| f32::from(c) / 255.0);
            let card = shaders.glass_shape(card, [r, g, b, spec.opacity * glass_opacity], RADIUS, true, dark, scale);
            let mut glass = Vec::new();
            // Event dots under the day numbers: white inside today's circle, else the number's ink, fainter.
            for (i, cell) in layout.cells.iter().enumerate().filter(|(i, _)| shown.dots.get(*i) == Some(&true)) {
                let dot = Rectangle::new((cell.loc.x + (cell.size.w - DOT) / 2, cell.loc.y + cell.size.h - ts(9)).into(), (DOT, DOT).into());
                let ink = if dark { 1.0 } else { 0.0 };
                let fill = match (Some(i) == today, days[i].month() == shown.month) {
                    (true, _) => [1.0, 1.0, 1.0, 1.0],
                    (false, true) => [ink, ink, ink, 0.5],
                    (false, false) => [ink, ink, ink, 0.25],
                };
                glass.push(shaders.light(dot, fill, false, scale));
            }
            if let Some(cell) = today.map(|i| layout.cells[i]) {
                // It holds the day's number, so it grows with the text, up to the cell's width.
                let d = ts(TODAY).min(cell.size.w);
                let circle = Rectangle::new((cell.loc.x + (cell.size.w - d) / 2, cell.loc.y + (cell.size.h - d) / 2).into(), (d, d).into());
                let [r, g, b] = accent.map(|c| f32::from(c) / 255.0);
                glass.push(shaders.light(circle, [r, g, b, 1.0], false, scale));
            }
            glass.push(card);
            self.glass = Some((key, glass));
        }

        // label, label-secondary, label-tertiary (Part D).
        let ink = |alpha_light: u8, alpha_dark: u8| if dark { [255, 255, 255, alpha_dark] } else { [0, 0, 0, alpha_light] };
        let (label, secondary, tertiary) = (ink(217, 217), ink(128, 140), ink(66, 64));
        let title3 = Style { color: label, ..super::ui::TITLE3 };
        let caption = Style { size: 10, line_height: 13, weight: 500, color: secondary, tabular: false };
        let body = Style { size: 13, line_height: 16, weight: 400, color: label, tabular: true };

        let first = NaiveDate::from_ymd_opt(shown.year, shown.month, 1);
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
        for (i, date) in days.iter().enumerate() {
            let color = match (Some(i) == today, date.month() == shown.month) {
                (true, _) => [255, 255, 255, 255],
                (false, true) => label,
                (false, false) => tertiary,
            };
            pieces.push(text.label(&date.day().to_string(), Style { color, ..body }, 30, scale));
        }
        // Week numbers: caption-sized, label-tertiary, one per row while the layout has their column.
        for row in grid.iter().take(layout.weeks.len()) {
            let number = crate::calendar::week_number(row).to_string();
            pieces.push(text.label(&number, Style { color: tertiary, tabular: true, ..caption }, shell::calendar::WEEK_W as u32, scale));
        }
        self.labels = keep_buffers(&self.labels, pieces);

        // The title is left-aligned: its rect shrinks to the text.
        let title_w = self.labels.first().map_or(0, |(r, _)| (f64::from(r.width) / scale).round() as i32);
        let title_rect = Rectangle::new(layout.title.loc, (title_w.min(layout.title.size.w), layout.title.size.h).into());
        let rects =
            [title_rect, layout.prev, layout.next].into_iter().chain(layout.header).chain(layout.cells.iter().copied()).chain(layout.weeks.iter().copied());
        let mut out: Vec<SceneElement> = self.labels.iter().zip(rects).filter_map(|(label, rect)| label_element(renderer, label, rect, scale)).collect();
        if let Some((_, glass)) = &self.glass {
            out.extend(glass.iter().cloned().map(SceneElement::Shader));
        }
        out
    }
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
}
