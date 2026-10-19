//! Spotlight (E4.3): Super+Space opens a search box over everything; typing ranks the installed apps (exact, prefix,
//! word prefix, substring, then fuzzy, so "hlm" finds Helium) and evaluates arithmetic ("12*7" → "= 84"). ↑/↓ select,
//! Enter launches the app or copies the answer, Esc clears the text and then closes. The app index is in memory, so a
//! keystroke costs one pass over a few hundred names and no disk access.
// ponytail: Files and Settings results are not searched yet: files need an index (Open-4), settings the Settings app
// (M7). Add a `Hit` variant and a source in `results` for each then; Super+Enter (show in Dolphin) comes with files.

use smithay::{
    input::keyboard::{Keysym, ModifiersState},
    utils::{Logical, Point, Rectangle, Size},
};

use crate::{apps::App, state::State, ui::field::Edit, ui::field::TextField, wm::Rect};

/// Most rows shown (E4.3).
pub const MAX_ROWS: usize = 8;

/// E4.3 and D13: 680 wide (90% of a narrower screen), top edge at 22% of the height; the field 52 px, rows 40.
pub mod layout {
    pub const WIDTH: i32 = 680;
    pub const RADIUS: f64 = 14.0;
    pub const PAD: i32 = 8;
    pub const FIELD: i32 = 52;
    pub const MAGNIFIER: i32 = 20;
    pub const ROW: i32 = 40;
    pub const ICON: i32 = 32;
}

/// Where everything is, output-relative.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub rect: Rect,
    pub magnifier: Rect,
    pub field: Rect,
    /// One per result that fits: at most `MAX_ROWS`, and never into the dock's space.
    pub rows: Vec<Rect>,
}

/// The panel on an `output`-sized screen whose usable area ends at `bottom` (the dock's top), with `count` results.
pub fn layout(output: Size<i32, Logical>, bottom: i32, count: usize) -> Layout {
    use layout::*;
    let w = WIDTH.min(output.w * 9 / 10);
    let (x, y) = ((output.w - w) / 2, output.h * 22 / 100);
    let field_x = x + PAD + 8 + MAGNIFIER + 8;
    let field = Rectangle::new((field_x, y + PAD).into(), (x + w - PAD - field_x, FIELD).into());
    let magnifier = Rectangle::new((x + PAD + 8, y + PAD + (FIELD - MAGNIFIER) / 2).into(), (MAGNIFIER, MAGNIFIER).into());
    let top = y + PAD + FIELD + PAD;
    let fits = ((bottom - top - PAD).max(0) / ROW) as usize;
    let rows: Vec<Rect> =
        (0..count.min(MAX_ROWS).min(fits) as i32).map(|i| Rectangle::new((x + PAD, top + i * ROW).into(), (w - 2 * PAD, ROW).into())).collect();
    let h = if rows.is_empty() { PAD + FIELD + PAD } else { top - y + rows.len() as i32 * ROW + PAD };
    Layout { rect: Rectangle::new((x, y).into(), (w, h).into()), magnifier, field, rows }
}

/// One result row.
#[derive(Debug, Clone, PartialEq)]
#[expect(clippy::large_enum_variant, reason = "at most MAX_ROWS of them, rebuilt per keystroke")]
pub enum Hit {
    App(App),
    /// The calculator's answer, formatted.
    Answer(String),
}

impl Hit {
    pub fn title(&self) -> String {
        match self {
            Hit::App(app) => app.name.clone(),
            Hit::Answer(answer) => format!("= {answer}"),
        }
    }

    /// The kind, on the right of the row.
    pub fn kind(&self) -> &'static str {
        match self {
            Hit::App(_) => "Application",
            Hit::Answer(_) => "Calculator",
        }
    }
}

/// `s` lower-cased with Latin diacritics stripped ("Café" → "cafe"), so a search ignores both.
// ponytail: Latin only, by table (no unicode-normalization crate); an NFD pass if other scripts come up.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        match c {
            'æ' => out.push_str("ae"),
            'œ' => out.push_str("oe"),
            'ß' => out.push_str("ss"),
            'à'..='å' | 'ā' | 'ă' | 'ą' => out.push('a'),
            'ç' | 'ć' | 'č' => out.push('c'),
            'ď' | 'đ' => out.push('d'),
            'è'..='ë' | 'ē' | 'ę' | 'ě' => out.push('e'),
            'ğ' => out.push('g'),
            'ì'..='ï' | 'ī' | 'ı' => out.push('i'),
            'ł' => out.push('l'),
            'ñ' | 'ń' | 'ň' => out.push('n'),
            'ò'..='ö' | 'ø' | 'ō' | 'ő' => out.push('o'),
            'ř' => out.push('r'),
            'ś' | 'š' | 'ş' => out.push('s'),
            'ť' | 'ţ' => out.push('t'),
            'ù'..='ü' | 'ū' | 'ů' | 'ű' => out.push('u'),
            'ý' | 'ÿ' => out.push('y'),
            'ź' | 'ż' | 'ž' => out.push('z'),
            c => out.push(c),
        }
    }
    out
}

/// How well `query` (folded, see `fold`) matches `name`; higher is better, `None` is no match. Exact > prefix > a
/// word's prefix > substring > fuzzy (the letters in order, the first at a word start; gaps cost).
pub fn score(name: &str, query: &str) -> Option<i32> {
    let name = fold(name);
    if query.is_empty() {
        return None;
    }
    let starts: Vec<usize> = name.char_indices().filter(|&(i, _)| i == 0 || !name[..i].ends_with(char::is_alphanumeric)).map(|(i, _)| i).collect();
    if name == query {
        return Some(1000);
    }
    if name.starts_with(query) {
        return Some(900);
    }
    if starts.iter().any(|&i| name[i..].starts_with(query)) {
        return Some(800);
    }
    if name.contains(query) {
        return Some(700);
    }
    // Fuzzy: each query char at its next occurrence, starting from a word start.
    let first = query.chars().next()?;
    starts
        .iter()
        .filter(|&&i| name[i..].starts_with(first))
        .filter_map(|&i| {
            let (mut at, mut gaps) = (i + first.len_utf8(), 0);
            for c in query.chars().skip(1) {
                let found = name[at..].find(c)?;
                gaps += name[at..at + found].chars().count() as i32;
                at += found + c.len_utf8();
            }
            Some(500 - gaps.min(400))
        })
        .max()
}

/// An app's best score over its name and its program (`dolphin`, `kate`): the program only by prefix.
pub fn app_score(app: &App, query: &str) -> Option<i32> {
    let by_program = app.program().to_lowercase().starts_with(query).then_some(600);
    score(&app.name, query).max(by_program)
}

/// The rows for `query`: the calculator's answer first when it is arithmetic (it's the top hit), then launchable apps
/// best first (shorter names win ties, so "Kate" beats "Kate Sessions"), at most `MAX_ROWS`.
// ponytail: one linear scan per keystroke (a few hundred apps, well under a millisecond); an incremental matcher
// (nucleo) only if the index grows to thousands.
pub fn results(apps: &[App], query: &str) -> Vec<Hit> {
    let query = fold(query.trim());
    if query.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<Hit> = calculate(&query).map(|v| Hit::Answer(format_number(v))).into_iter().collect();
    let mut ranked: Vec<(i32, &App)> = apps.iter().filter(|a| !a.no_display).filter_map(|a| Some((app_score(a, &query)?, a))).collect();
    ranked.sort_by(|(sa, a), (sb, b)| sb.cmp(sa).then(a.name.len().cmp(&b.name.len())).then_with(|| a.name.cmp(&b.name)));
    out.extend(ranked.into_iter().map(|(_, a)| Hit::App(a.clone())));
    out.truncate(MAX_ROWS);
    out
}

/// `input` as arithmetic: + − × ÷ (also * / x), ^ powers, parentheses, decimals. `None` unless it is an expression
/// (a bare number isn't) with a finite value.
pub fn calculate(input: &str) -> Option<f64> {
    let input = input.trim();
    if input.parse::<f64>().is_ok() {
        return None;
    }
    let chars: Vec<char> = input.chars().filter(|c| !c.is_whitespace()).collect();
    let mut at = 0;
    let value = expr(&chars, &mut at)?;
    (at == chars.len() && value.is_finite()).then_some(value)
}

fn expr(s: &[char], at: &mut usize) -> Option<f64> {
    let mut value = term(s, at)?;
    while let Some(&op) = s.get(*at).filter(|c| matches!(c, '+' | '-' | '−')) {
        *at += 1;
        let rhs = term(s, at)?;
        value = if op == '+' { value + rhs } else { value - rhs };
    }
    Some(value)
}

fn term(s: &[char], at: &mut usize) -> Option<f64> {
    let mut value = unary(s, at)?;
    while let Some(&op) = s.get(*at).filter(|c| matches!(c, '*' | '×' | 'x' | '/' | '÷')) {
        *at += 1;
        let rhs = unary(s, at)?;
        value = if matches!(op, '/' | '÷') { value / rhs } else { value * rhs };
    }
    Some(value)
}

/// A sign binds looser than `^`: -2^2 = -4.
fn unary(s: &[char], at: &mut usize) -> Option<f64> {
    match s.get(*at)? {
        '-' | '−' => {
            *at += 1;
            Some(-unary(s, at)?)
        }
        '+' => {
            *at += 1;
            unary(s, at)
        }
        _ => power(s, at),
    }
}

/// `^` binds right: 2^3^2 = 2^9.
fn power(s: &[char], at: &mut usize) -> Option<f64> {
    let base = atom(s, at)?;
    if s.get(*at) == Some(&'^') {
        *at += 1;
        return Some(base.powf(unary(s, at)?));
    }
    Some(base)
}

fn atom(s: &[char], at: &mut usize) -> Option<f64> {
    match s.get(*at)? {
        '(' => {
            *at += 1;
            let value = expr(s, at)?;
            (s.get(*at) == Some(&')')).then(|| *at += 1)?;
            Some(value)
        }
        _ => {
            let len = s[*at..].iter().take_while(|c| c.is_ascii_digit() || **c == '.').count();
            let number: String = s[*at..*at + len].iter().collect();
            *at += len;
            number.parse().ok()
        }
    }
}

/// At most 10 decimals, no trailing zeros: 84, 0.3333333333, 1e21.
pub fn format_number(v: f64) -> String {
    let v = if v.abs() < 1e15 { (v * 1e10).round() / 1e10 } else { v };
    // + 0.0 turns −0 into 0.
    format!("{}", v + 0.0)
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Spotlight {
    pub field: TextField,
    /// The selected row; 0 is the top hit.
    pub selected: usize,
}

impl State {
    /// Super+Space: opens Spotlight, or closes it when open.
    pub fn toggle_spotlight(&mut self) {
        if !self.close_spotlight() {
            self.popup = None;
            self.spotlight = Some(Spotlight::default());
            self.schedule_redraw();
        }
    }

    pub fn close_spotlight(&mut self) -> bool {
        let open = self.spotlight.take().is_some();
        if open {
            self.schedule_redraw();
        }
        open
    }

    /// The rows for the current text.
    pub fn spotlight_results(&self) -> Vec<Hit> {
        self.spotlight.as_ref().map(|s| results(self.apps.list(), &s.field.text)).unwrap_or_default()
    }

    /// The layout for `count` rows, above the dock.
    pub fn spotlight_layout(&self, count: usize) -> Option<Layout> {
        let size = self.space.outputs().next().and_then(|o| self.space.output_geometry(o))?.size;
        Some(layout(size, size.h - crate::shell::dock::reserve(&self.config.dock), count))
    }

    /// While open Spotlight takes every key except bound shortcuts (Super+Space closes it, volume keys still work).
    /// `true` if the key was taken.
    pub fn spotlight_key(&mut self, sym: Keysym, raw: Keysym, mods: &ModifiersState) -> bool {
        let Some(mut open) = self.spotlight.clone() else { return false };
        if crate::shortcuts::lookup(&self.bindings, mods, raw, self.nested).is_some() {
            return false;
        }
        let hits = self.spotlight_results();
        let last = self.spotlight_layout(hits.len()).map_or(0, |l| l.rows.len()).saturating_sub(1);
        let typed = sym.key_char().map(String::from);
        match sym {
            Keysym::Up | Keysym::KP_Up => open.selected = open.selected.saturating_sub(1),
            Keysym::Down | Keysym::KP_Down => open.selected = (open.selected + 1).min(last),
            _ => match open.field.key(sym, typed.as_deref(), mods.ctrl) {
                Edit::Changed => open.selected = 0,
                Edit::Moved | Edit::Ignored => {}
                Edit::Cancel if open.field.text.is_empty() => return self.close_spotlight(),
                Edit::Cancel => open = Spotlight::default(),
                Edit::Submit => {
                    if let Some(hit) = hits.into_iter().nth(open.selected.min(last)) {
                        self.spotlight_open(hit);
                    }
                    return true;
                }
            },
        }
        self.spotlight = Some(open);
        self.schedule_redraw();
        true
    }

    /// Launches the app (as the dock does) or copies the answer; then Spotlight closes.
    fn spotlight_open(&mut self, hit: Hit) {
        self.close_spotlight();
        match hit {
            Hit::App(app) => {
                tracing::info!(app = app.id, "spotlight: launch");
                if self.launch(Some(&app)) {
                    self.remember_launch(&app.id);
                }
            }
            Hit::Answer(answer) => {
                let seat = self.seat.clone();
                self.clipboard.set_text(&self.display_handle, &seat, &answer);
            }
        }
    }

    /// A press at `location` (output-relative): a row opens its result; outside, Spotlight closes. `true`: taken.
    pub fn spotlight_press(&mut self, location: Point<f64, Logical>) -> bool {
        if self.spotlight.is_none() {
            return false;
        }
        let hits = self.spotlight_results();
        let Some(layout) = self.spotlight_layout(hits.len()) else { return false };
        if !layout.rect.to_f64().contains(location) {
            self.close_spotlight();
        } else if let Some(i) = layout.rows.iter().position(|r| r.to_f64().contains(location)) {
            self.spotlight_open(hits[i].clone());
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, exec: &str) -> App {
        App { id: name.to_lowercase(), name: name.into(), exec: exec.into(), icon: String::new(), wm_class: None, no_display: false, ..Default::default() }
    }

    fn names(hits: &[Hit]) -> Vec<String> {
        hits.iter().map(Hit::title).collect()
    }

    #[test]
    fn ranking_tiers() {
        assert_eq!(score("Kate", "kate"), Some(1000));
        assert_eq!(score("Kate Sessions", "kate"), Some(900));
        assert_eq!(score("System Monitor", "mon"), Some(800));
        assert_eq!(score("KWrite", "write"), Some(700));
        assert!(score("Helium", "hlm").is_some_and(|s| s < 700 && s > 0), "fuzzy, from the plan");
        assert_eq!(score("Helium", "lm"), None, "fuzzy starts at a word start");
        assert_eq!(score("Helium", "hx"), None);
        assert_eq!(score("Helium", ""), None);
        // Accents don't matter, either way round.
        assert_eq!(score("Café", "cafe"), Some(1000));
        assert_eq!(score("Cafe", &fold("CAFÉ")), Some(1000));
        assert_eq!(fold("Œuvre Straße Łódź"), "oeuvre strasse lodz");
    }

    #[test]
    fn results_order_and_filter() {
        let mut hidden = app("Kate Helper", "kate-helper");
        hidden.no_display = true;
        let apps = [
            app("Kate Sessions", "kate -s"),
            app("Kate", "kate %U"),
            app("Dolphin", "dolphin"),
            app("Konsole", "konsole"),
            app("Helium", "helium-browser"),
            hidden,
        ];
        assert_eq!(names(&results(&apps, "Kate")), ["Kate", "Kate Sessions"]);
        assert_eq!(names(&results(&apps, "hlm")), ["Helium"]);
        // By program prefix: "helium-browser".
        assert_eq!(names(&results(&apps, "helium-b")), ["Helium"]);
        assert!(results(&apps, "").is_empty());
        assert!(results(&apps, "   ").is_empty());
        let many: Vec<App> = (0..20).map(|i| app(&format!("App {i}"), "x")).collect();
        assert_eq!(results(&many, "app").len(), MAX_ROWS);
    }

    #[test]
    fn calculator() {
        assert_eq!(calculate("12*7"), Some(84.0));
        assert_eq!(calculate("1 + 2 * 3"), Some(7.0));
        assert_eq!(calculate("(1+2)*3"), Some(9.0));
        assert_eq!(calculate("2^3^2"), Some(512.0));
        assert_eq!(calculate("-2^2"), Some(-4.0));
        assert_eq!(calculate("2^-1"), Some(0.5));
        assert_eq!(calculate("10 ÷ 4"), Some(2.5));
        assert_eq!(calculate("3x4"), Some(12.0));
        assert_eq!(calculate("42"), None, "a bare number is no sum");
        assert_eq!(calculate("1/0"), None);
        assert_eq!(calculate("1+"), None);
        assert_eq!(calculate("(1+2"), None);
        assert_eq!(calculate("kate"), None);
        assert_eq!(format_number(84.0), "84");
        assert_eq!(format_number(1.0 / 3.0), "0.3333333333");
        assert_eq!(format_number(0.1 + 0.2), "0.3");
        assert_eq!(format_number(-0.0), "0");
        let apps = [app("Kate", "kate")];
        assert_eq!(names(&results(&apps, "12*7")), ["= 84"]);
    }

    #[test]
    fn layout_sizes() {
        // 1080p: 680 wide, centred, top at 22%.
        let l = layout((1920, 1080).into(), 1080 - 101, 3);
        assert_eq!((l.rect.loc.x, l.rect.loc.y, l.rect.size.w), (620, 237, 680));
        assert_eq!(l.field.size.h, 52);
        assert_eq!(l.rows.len(), 3);
        assert_eq!(l.rect.size.h, 8 + 52 + 8 + 3 * 40 + 8);
        assert!(l.rows.iter().chain([&l.field, &l.magnifier]).all(|r| l.rect.contains_rect(*r)));
        assert!(!l.magnifier.overlaps(l.field));
        // Nothing typed: just the field.
        assert_eq!(layout((1920, 1080).into(), 979, 0).rect.size.h, 68);
        // Narrow: 90% of the width.
        assert_eq!(layout((600, 800).into(), 700, 0).rect.size.w, 540);
        // Capped at 8, and at the space above the dock.
        assert_eq!(layout((1920, 1080).into(), 979, 20).rows.len(), 8);
        let short = layout((1280, 600).into(), 600 - 101, 20);
        assert_eq!(short.rows.len(), 7);
        assert!(short.rect.loc.y + short.rect.size.h <= 499);
    }
}
