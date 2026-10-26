//! Spotlight (E4.3): Super+Space opens a search box over everything; typing ranks the installed apps (exact, prefix,
//! word prefix, substring, then fuzzy, so "hlm" finds Helium) and evaluates arithmetic ("12*7" → "= 84"). ↑/↓ select,
//! Enter launches the app or copies the answer, Esc clears the text and then closes. The app index is in memory, so a
//! keystroke costs one pass over a few hundred names and no disk access. Every Settings page and setting is matched
//! too; Enter opens Settings on its page. Files come from the recent files and `plocate` (Open-4: no index of our
//! own), asked on a worker thread once typing pauses; they arrive under the apps without moving them. Enter opens a
//! file, Super+Enter shows it in the file manager.

use smithay::{
    input::keyboard::{Keysym, ModifiersState},
    utils::{Logical, Point, Rectangle, Size},
};

use std::{
    path::Path,
    sync::{
        LazyLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use smithay::reexports::calloop::{
    channel,
    timer::{TimeoutAction, Timer},
};

use crate::{apps::App, state::State, symbols::Symbol, text::ts, ui::field::Edit, ui::field::TextField, wm::Rect};

/// Most rows shown (E4.3).
pub const MAX_ROWS: usize = 8;

/// E4.3 and D13: 680 wide (90% of a narrower screen), top edge at 22% of the height; the field 52 px, rows 40.
/// `WIDTH`, `FIELD` and `ROW` grow with the text size (`text::ts`).
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
    use layout::{MAGNIFIER, PAD};
    let (w, field_h, row_h) = (ts(layout::WIDTH).min(output.w * 9 / 10), ts(layout::FIELD), ts(layout::ROW));
    let (x, y) = ((output.w - w) / 2, output.h * 22 / 100);
    let field_x = x + PAD + 8 + MAGNIFIER + 8;
    let field = Rectangle::new((field_x, y + PAD).into(), (x + w - PAD - field_x, field_h).into());
    let magnifier = Rectangle::new((x + PAD + 8, y + PAD + (field_h - MAGNIFIER) / 2).into(), (MAGNIFIER, MAGNIFIER).into());
    let top = y + PAD + field_h + PAD;
    let fits = ((bottom - top - PAD).max(0) / row_h) as usize;
    let rows: Vec<Rect> =
        (0..count.min(MAX_ROWS).min(fits) as i32).map(|i| Rectangle::new((x + PAD, top + i * row_h).into(), (w - 2 * PAD, row_h).into())).collect();
    let h = if rows.is_empty() { PAD + field_h + PAD } else { top - y + rows.len() as i32 * row_h + PAD };
    Layout { rect: Rectangle::new((x, y).into(), (w, h).into()), magnifier, field, rows }
}

/// One result row.
#[derive(Debug, Clone, PartialEq)]
pub enum Hit {
    App(App),
    /// The calculator's answer, formatted.
    Answer(String),
    Setting(&'static Setting),
    File(File),
}

/// A file or folder found for the query.
#[derive(Debug, Clone, PartialEq)]
pub struct File {
    pub path: String,
    /// Its type (`image/png`), "Folder", or "Document" when the extension says nothing.
    pub kind: String,
    /// An icon name from the theme.
    pub icon: String,
}

/// A Settings page, or one row of it.
#[derive(Debug, Clone, PartialEq)]
pub struct Setting {
    /// What is matched and shown: the page's name, or the row's label.
    pub label: String,
    /// The page's name, `--page` slug, symbol and squircle colour.
    pub page: &'static str,
    pub slug: String,
    pub symbol: Symbol,
    pub color: u32,
}

/// Every Settings page, then every row of its `SETTINGS` table, read from the Settings app's source so the list
/// can't drift from it (the shell doesn't link the Settings crate).
pub static SETTINGS: LazyLock<Vec<Setting>> = LazyLock::new(|| parse_settings(include_str!("../../settings/src/pages.rs")));

/// `pages.rs`: its `Page::name` arms (`Page::Wifi => "Wi-Fi",`), `Page::icon` arms (`Page::Wifi => ("wifi-high",
/// 0x007AFF),`) and the `SETTINGS` rows (`s(Page::Sound, "group", "key", "Label", …)`, one line or spread over
/// several). Pages whose symbol the shell lacks are left out (a test checks there are none).
pub fn parse_settings(src: &'static str) -> Vec<Setting> {
    let (mut names, mut icons) = (Vec::new(), Vec::new());
    for (variant, rhs) in src.lines().filter_map(|l| l.trim().strip_prefix("Page::")?.split_once(" => ")) {
        if let Some(name) = rhs.strip_prefix('"').and_then(|r| r.strip_suffix("\",")) {
            names.push((variant, name));
        } else if let Some((file, color)) = rhs.strip_prefix("(\"").and_then(|r| r.strip_suffix("),")?.split_once("\", 0x")) {
            icons.push((variant, file, color));
        }
    }
    let page = |variant: &str| {
        let &(_, name) = names.iter().find(|(v, _)| *v == variant)?;
        let &(_, file, color) = icons.iter().find(|(v, ..)| *v == variant)?;
        let slug = name.to_lowercase().replace(" & ", "-").replace(' ', "-");
        Some(Setting { label: name.into(), page: name, slug, symbol: Symbol::named(file)?, color: u32::from_str_radix(color, 16).ok()? })
    };
    let mut out: Vec<Setting> = names.iter().filter_map(|(v, _)| page(v)).collect();
    let table = src.split_once("pub const SETTINGS: &[Setting] = &[").and_then(|(_, t)| t.split_once("\n];")).map_or("", |(t, _)| t);
    for row in table.split("\n    s(").skip(1) {
        let Some((variant, rest)) = row.trim_start().strip_prefix("Page::").and_then(|r| r.split_once(',')) else { continue };
        // The string literals in order: group, key, label.
        let Some(label) = rest.split('"').skip(1).step_by(2).nth(2) else { continue };
        out.extend(page(variant).filter(|p| p.label != label).map(|p| Setting { label: label.into(), ..p }));
    }
    out
}

impl Setting {
    /// A row's title says its page: "Alert sound — Sound".
    pub fn title(&self) -> String {
        if self.label == self.page { self.label.clone() } else { format!("{} — {}", self.label, self.page) }
    }
}

impl Hit {
    pub fn title(&self) -> String {
        match self {
            Hit::App(app) => app.name.clone(),
            Hit::Answer(answer) => format!("= {answer}"),
            Hit::Setting(setting) => setting.title(),
            Hit::File(file) => name_of(&file.path).into(),
        }
    }

    /// The kind, on the right of the row.
    pub fn kind(&self) -> &str {
        match self {
            Hit::App(_) => "Application",
            Hit::Answer(_) => "Calculator",
            Hit::Setting(_) => "System Settings",
            Hit::File(file) => &file.kind,
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

/// Most Settings rows; Files keep `FILES` rows free under the apps, so their late answer never moves a row above them.
const SETTINGS_ROWS: usize = 2;
const FILES: usize = 3;

/// The rows for `query`, grouped as E4.3 orders them: the top hit (the calculator's answer when it is arithmetic,
/// else the best app or setting, an app on a tie), the other apps best first (shorter names win ties, so "Kate"
/// beats "Kate Sessions"), then `files`, then settings. At most `MAX_ROWS`. Apps and settings don't depend on
/// `files`, so files arriving later only push down the settings under them.
// ponytail: one linear scan per keystroke (a few hundred apps and settings, well under a millisecond); an incremental
// matcher (nucleo) only if the index grows to thousands.
pub fn results(apps: &[App], files: &[Hit], query: &str) -> Vec<Hit> {
    let query = fold(query.trim());
    if query.is_empty() {
        return Vec::new();
    }
    let answer = calculate(&query).map(|v| Hit::Answer(format_number(v)));
    let mut ranked: Vec<(i32, &App)> = apps.iter().filter(|a| !a.no_display).filter_map(|a| Some((app_score(a, &query)?, a))).collect();
    ranked.sort_by(|(sa, a), (sb, b)| sb.cmp(sa).then(a.name.len().cmp(&b.name.len())).then_with(|| a.name.cmp(&b.name)));
    let mut settings: Vec<(i32, &'static Setting)> = SETTINGS.iter().filter_map(|s| Some((score(&s.label, &query)?, s))).collect();
    settings.sort_by(|(sa, a), (sb, b)| sb.cmp(sa).then(a.label.len().cmp(&b.label.len())));
    settings.truncate(SETTINGS_ROWS);
    let room = MAX_ROWS - FILES - settings.len() - usize::from(answer.is_some());
    let mut apps: Vec<(i32, Hit)> = ranked.into_iter().take(room).map(|(s, a)| (s, Hit::App(a.clone()))).collect();
    let mut settings: Vec<(i32, Hit)> = settings.into_iter().map(|(s, h)| (s, Hit::Setting(h))).collect();
    let top = match (answer, apps.first(), settings.first()) {
        (Some(answer), ..) => Some(answer),
        (None, Some((a, _)), Some((s, _))) if s > a => Some(settings.remove(0).1),
        (None, Some(_), _) => Some(apps.remove(0).1),
        (None, None, Some(_)) => Some(settings.remove(0).1),
        (None, None, None) => None,
    };
    let mut out: Vec<Hit> = top.into_iter().chain(apps.into_iter().map(|(_, h)| h)).collect();
    out.extend(files.iter().take(FILES).cloned());
    out.extend(settings.into_iter().map(|(_, h)| h));
    out.truncate(MAX_ROWS);
    out
}

/// Quiet time after a keystroke before files are asked for.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// The files shown and the latest keystroke's ticket: a search starts only if its keystroke is still the latest once
/// `DEBOUNCE` has passed, and its answer is kept only if nothing was typed since.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Files {
    pub hits: Vec<Hit>,
    ticket: u64,
}

/// Tickets are unique across openings, so an answer meant for a closed Spotlight never lands in the next one.
static TICKETS: AtomicU64 = AtomicU64::new(1);

impl Files {
    /// A keystroke: the files shown no longer match; returns its ticket.
    pub fn typed(&mut self) -> u64 {
        self.hits.clear();
        self.ticket = TICKETS.fetch_add(1, Ordering::Relaxed);
        self.ticket
    }

    /// Whether `ticket` is still the latest keystroke's.
    pub fn due(&self, ticket: u64) -> bool {
        ticket == self.ticket
    }

    /// Shows `hits` if they answer the latest keystroke; `false` if they are stale and dropped.
    pub fn answer(&mut self, ticket: u64, hits: Vec<Hit>) -> bool {
        let fresh = self.due(ticket);
        if fresh {
            self.hits = hits;
        }
        fresh
    }
}

/// The row to select once rows change around it: the same result where it went, else the same place.
pub fn reselect(before: &[Hit], selected: usize, after: &[Hit]) -> usize {
    before.get(selected).and_then(|h| after.iter().position(|a| a == h)).unwrap_or(selected).min(after.len().saturating_sub(1))
}

/// The last part of `path`.
fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Whether `path` is inside `home` with no hidden (dot) folder or file on the way.
pub fn shown_in_home(path: &str, home: &str) -> bool {
    let inside = path.strip_prefix(home).and_then(|r| r.strip_prefix('/')).filter(|r| !r.is_empty());
    inside.is_some_and(|r| !r.split('/').any(|part| part.starts_with('.')))
}

/// `locate`'s output (one path a line, matched anywhere in the path) narrowed to the shown files in `home` whose own
/// name has `query` (folded), best match first, then shorter paths.
pub fn parse_locate(out: &str, home: &str, query: &str) -> Vec<String> {
    let mut found: Vec<(i32, &str)> = out
        .lines()
        .filter(|p| shown_in_home(p, home))
        .filter_map(|p| Some((score(name_of(p), query).filter(|_| fold(name_of(p)).contains(query))?, p)))
        .collect();
    found.sort_by(|(sa, a), (sb, b)| sb.cmp(sa).then(a.len().cmp(&b.len())));
    found.into_iter().map(|(_, p)| p.to_owned()).collect()
}

/// Up to `FILES` paths for `text`: recent files that match (newest first), then `plocate`'s (or `locate`'s) in the
/// home folder. Without either, only the recent ones. Runs on a worker thread.
// ponytail: `locate` is asked for 200 paths and the hidden ones dropped after, so a home full of dot-folder matches
// can crowd real files out; a `--regex` that skips them if that shows up.
fn find_files(text: &str) -> Vec<String> {
    let query = fold(text);
    let home = std::env::var("HOME").unwrap_or_default();
    let (_, uris) = crate::shell::dock::recent_items(&std::fs::read_to_string(crate::shell::dock::recent_path()).unwrap_or_default());
    let mut out: Vec<String> =
        uris.iter().filter_map(|u| crate::apps::file_path(u)).filter(|p| score(name_of(p), &query).is_some() && Path::new(p).exists()).take(FILES).collect();
    for program in ["plocate", "locate"] {
        match std::process::Command::new(program).args(["-i", "-A", "-l", "200", "--", &format!("{home}/"), text]).output() {
            Ok(done) => {
                let located = parse_locate(&String::from_utf8_lossy(&done.stdout), &home, &query);
                out.extend(located.into_iter().filter(|p| !out.contains(p)).collect::<Vec<_>>());
                break;
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                tracing::warn!("spotlight: {program}: {err}");
                break;
            }
        }
    }
    out.truncate(FILES);
    out
}

/// `path` as a row: its type and icon by extension, or a folder.
fn file_hit(path: String, mime: &crate::shell::desktop::Mime) -> Hit {
    let (kind, icon) = if Path::new(&path).is_dir() {
        ("Folder".to_owned(), "folder".to_owned())
    } else {
        let icon = mime.icons(name_of(&path)).into_iter().find(|i| crate::apps::find_icon(i).is_some());
        (mime.of(name_of(&path)).unwrap_or("Document").to_owned(), icon.unwrap_or_else(|| crate::shell::desktop::FALLBACK_ICON.into()))
    };
    Hit::File(File { path, kind, icon })
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
    pub files: Files,
    /// The display it shows on (`focus_output` when it opened).
    pub output: Option<smithay::output::Output>,
}

impl State {
    /// Super+Space: opens Spotlight, or closes it when open.
    pub fn toggle_spotlight(&mut self) {
        if !self.close_spotlight() {
            self.popup = None;
            self.spotlight = Some(Spotlight { output: self.focus_output(), ..Default::default() });
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
        self.spotlight.as_ref().map(|s| results(self.apps.list(), &s.files.hits, &s.field.text)).unwrap_or_default()
    }

    /// The layout for `count` rows, above the dock, relative to its display.
    pub fn spotlight_layout(&self, count: usize) -> Option<Layout> {
        let size = self.overlay_geometry(self.spotlight.as_ref()?.output.as_ref())?.size;
        Some(layout(size, size.h - crate::shell::dock::reserve(&self.config.dock), count))
    }

    /// While open Spotlight takes every key except bound shortcuts (Super+Space closes it, volume keys still work).
    /// `true` if the key was taken.
    pub fn spotlight_key(&mut self, sym: Keysym, raw: Keysym, mods: &ModifiersState) -> bool {
        let Some(mut open) = self.spotlight.clone() else { return false };
        let hits = self.spotlight_results();
        let last = self.spotlight_layout(hits.len()).map_or(0, |l| l.rows.len()).saturating_sub(1);
        // Super+Enter on a file shows it in the file manager (before the Super+Return binding).
        if mods.logo
            && matches!(sym, Keysym::Return | Keysym::KP_Enter)
            && let Some(Hit::File(file)) = hits.get(open.selected.min(last))
        {
            let path = std::path::PathBuf::from(&file.path);
            self.close_spotlight();
            self.show_in_files(&path);
            return true;
        }
        if crate::shortcuts::lookup(&self.bindings, mods, raw, self.nested).is_some() {
            return false;
        }
        let typed = sym.key_char().map(String::from);
        match sym {
            Keysym::Up | Keysym::KP_Up => open.selected = open.selected.saturating_sub(1),
            Keysym::Down | Keysym::KP_Down => open.selected = (open.selected + 1).min(last),
            _ => match open.field.key(sym, typed.as_deref(), mods.ctrl) {
                Edit::Changed => {
                    open.selected = 0;
                    let ticket = open.files.typed();
                    self.spotlight_ask_files(ticket, open.field.text.trim().to_owned());
                }
                Edit::Moved | Edit::Ignored => {}
                Edit::Cancel if open.field.text.is_empty() => return self.close_spotlight(),
                Edit::Cancel => open = Spotlight { output: open.output.take(), ..Default::default() },
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

    /// After `DEBOUNCE` with nothing typed since keystroke `ticket`, looks for files named like `text` on a worker
    /// thread; the answer shows only if still nothing was typed.
    fn spotlight_ask_files(&mut self, ticket: u64, text: String) {
        if text.is_empty() {
            return;
        }
        let waited = self.loop_handle.insert_source(Timer::from_duration(DEBOUNCE), move |_, _, state| {
            if state.spotlight.as_ref().is_some_and(|s| s.files.due(ticket)) {
                state.spotlight_find_files(ticket, text.clone());
            }
            TimeoutAction::Drop
        });
        let _ = waited.inspect_err(|err| tracing::warn!("spotlight files: {err}"));
    }

    fn spotlight_find_files(&mut self, ticket: u64, text: String) {
        static MIME: LazyLock<crate::shell::desktop::Mime> = LazyLock::new(crate::shell::desktop::Mime::load);
        let (tx, rx) = channel::channel();
        // The channel goes once the worker drops its end.
        let inserted = self.loop_handle.insert_source(rx, move |event, _, state| {
            if let channel::Event::Msg(hits) = event {
                state.spotlight_files(ticket, hits);
            }
        });
        if let Err(err) = inserted {
            return tracing::warn!("spotlight files: {err}");
        }
        let spawned = std::thread::Builder::new().name("spotlight-files".into()).spawn(move || {
            let _ = tx.send(find_files(&text).into_iter().map(|p| file_hit(p, &MIME)).collect::<Vec<_>>());
        });
        let _ = spawned.inspect_err(|err| tracing::warn!("spotlight files: {err}"));
    }

    /// Files for keystroke `ticket`: shown if it is still the latest, keeping the selection on the same row.
    fn spotlight_files(&mut self, ticket: u64, hits: Vec<Hit>) {
        let before = self.spotlight_results();
        let Some(open) = self.spotlight.as_mut() else { return };
        if !open.files.answer(ticket, hits) {
            return;
        }
        let selected = open.selected;
        let after = self.spotlight_results();
        if let Some(open) = self.spotlight.as_mut() {
            open.selected = reselect(&before, selected, &after);
        }
        self.schedule_redraw();
    }

    /// Launches the app (as the dock does), copies the answer, opens Settings on the page or opens the file with its
    /// app; then Spotlight closes.
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
            Hit::Setting(setting) => {
                self.spawn(&["noobsbot-desktop-settings".into(), "--page".into(), setting.slug.clone()]);
            }
            Hit::File(file) => {
                self.spawn(&["xdg-open".into(), file.path]);
            }
        }
    }

    /// A press at `location` (first-output-relative): a row opens its result; outside, Spotlight closes. `true`: taken.
    pub fn spotlight_press(&mut self, location: Point<f64, Logical>) -> bool {
        let Some(open) = &self.spotlight else { return false };
        let location = self.to_display(open.output.as_ref(), location);
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

    /// The app and calculator rows only.
    fn apps_of(hits: &[Hit]) -> Vec<String> {
        names(&hits.iter().filter(|h| !matches!(h, Hit::Setting(_))).cloned().collect::<Vec<_>>())
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
        assert_eq!(apps_of(&results(&apps, &[], "Kate")), ["Kate", "Kate Sessions"]);
        assert_eq!(apps_of(&results(&apps, &[], "hlm")), ["Helium"]);
        // By program prefix: "helium-browser".
        assert_eq!(apps_of(&results(&apps, &[], "helium-b")), ["Helium"]);
        assert!(results(&apps, &[], "").is_empty());
        assert!(results(&apps, &[], "   ").is_empty());
        // Apps leave room for files and settings.
        let many: Vec<App> = (0..20).map(|i| app(&format!("Zq {i}"), "x")).collect();
        assert_eq!(results(&many, &[], "zq").len(), MAX_ROWS - FILES);
    }

    #[test]
    fn settings_come_from_the_settings_app() {
        let src = include_str!("../../settings/src/pages.rs");
        let pages: Vec<&Setting> = SETTINGS.iter().filter(|s| s.label == s.page).collect();
        // Every page, with its symbol (a page missing here lacks a `Symbol`), and a slug its `Page::parse` takes.
        let all = src.split_once("pub const ALL: [Page; ").and_then(|(_, r)| r.split_once(']')).map(|(n, _)| n.parse::<usize>().unwrap());
        assert_eq!(Some(pages.len()), all);
        for (page, slug) in [("Wi-Fi", "wi-fi"), ("Desktop & Dock", "desktop-dock"), ("Date & Time", "date-time"), ("Lock Screen", "lock-screen")] {
            assert_eq!(pages.iter().find(|p| p.page == page).map(|p| p.slug.as_str()), Some(slug));
        }
        // Every `SETTINGS` row (bar the few named as their page: Appearance, Trackpad, Spaces), one line or several.
        let rows = SETTINGS.len() - pages.len();
        let table = src.split_once("pub const SETTINGS").unwrap().1.split_once("\n];").unwrap().0;
        assert!(rows > 100 && rows + 3 >= table.matches("\n    s(").count(), "{rows}");
        let mode = SETTINGS.iter().find(|s| s.label == "Alert sound").unwrap();
        assert_eq!((mode.page, mode.slug.as_str(), mode.symbol, mode.title().as_str()), ("Sound", "sound", Symbol::SpeakerHigh, "Alert sound — Sound"));
        assert!(SETTINGS.iter().any(|s| s.label == "Auto: sunset to sunrise" && s.page == "Appearance"), "a row spread over lines");
    }

    #[test]
    fn settings_match_and_group() {
        let titles = |q| names(&results(&[], &[], q));
        assert_eq!(titles("bluetooth")[0], "Bluetooth");
        assert_eq!(titles("date")[0], "Date & Time");
        assert_eq!(results(&[], &[], "wifi")[0].kind(), "System Settings");
        assert!(titles("alert").contains(&"Alert sound — Sound".to_string()));
        // An app wins a tie for the top hit; a better setting takes it.
        let apps = [app("Bluetooth", "blueman"), app("Bluetooth Manager", "blueman-manager")];
        let hits = results(&apps, &[], "bluetooth");
        assert!(matches!(hits[0], Hit::App(_)) && matches!(hits.last(), Some(Hit::Setting(_))));
        let hits = results(&[app("Bluetooth Manager", "blueman-manager")], &[], "bluetooth");
        assert!(matches!(hits[0], Hit::Setting(_)) && matches!(hits[1], Hit::App(_)));
    }

    #[test]
    fn groups_keep_their_order_and_files_move_only_settings() {
        let apps: Vec<App> = (0..20).map(|i| app(&format!("Sound {i}"), "x")).collect();
        let file = |n: &str| Hit::Answer(n.into());
        let before = results(&apps, &[], "sound");
        let files = [file("a"), file("b"), file("c"), file("d")];
        let after = results(&apps, &files, "sound");
        assert_eq!(after.len(), MAX_ROWS);
        // The Sound page is the top hit, then the apps, unchanged by the files; files next (at most FILES), then the
        // other settings.
        assert!(matches!(&before[0], Hit::Setting(s) if s.label == "Sound"));
        let stable = 1 + before[1..].iter().take_while(|h| matches!(h, Hit::App(_))).count();
        assert_eq!((stable, before[..stable] == after[..stable]), (MAX_ROWS - FILES - 1, true));
        assert_eq!(after[stable..stable + FILES], files[..FILES]);
        assert!(after[stable + FILES..].iter().all(|h| matches!(h, Hit::Setting(_))));
    }

    #[test]
    fn locate_output_keeps_shown_home_files_named_like_the_query() {
        assert!(shown_in_home("/home/u/Documents/report.pdf", "/home/u"));
        assert!(!shown_in_home("/home/u/.cache/report.pdf", "/home/u"), "hidden folder");
        assert!(!shown_in_home("/home/u/notes/.report.pdf", "/home/u"), "hidden file");
        assert!(!shown_in_home("/home/user2/report.pdf", "/home/u"), "another home sharing the prefix");
        assert!(!shown_in_home("/home/u", "/home/u") && !shown_in_home("/usr/share/report", "/home/u"));
        let out = "/home/u/Reports/old/a.txt\n/home/u/.local/report.txt\n/usr/share/report.txt\n/home/u/Docs/Q3 Report.pdf\n/home/u/report\n";
        // Matched by the file's own name (not "Reports/" above it), exact before substring.
        assert_eq!(parse_locate(out, "/home/u", "report"), ["/home/u/report", "/home/u/Docs/Q3 Report.pdf"]);
        assert!(parse_locate("", "/home/u", "report").is_empty());
    }

    #[test]
    fn file_answers_wait_for_quiet_and_drop_stale_ones() {
        let mut files = Files::default();
        let first = files.typed();
        let second = files.typed();
        // Only the latest keystroke's timer searches.
        assert!(!files.due(first) && files.due(second));
        let hit = |n: &str| Hit::File(File { path: format!("/home/u/{n}"), kind: "Folder".into(), icon: "folder".into() });
        assert!(!files.answer(first, vec![hit("old")]) && files.hits.is_empty(), "a stale answer is dropped");
        assert!(files.answer(second, vec![hit("new")]));
        assert_eq!(names(&files.hits), ["new"]);
        // Typing again hides the answer until the next one.
        let third = files.typed();
        assert!(files.hits.is_empty() && !files.answer(second, vec![hit("new")]) && files.due(third));
        // Another opening's tickets never match.
        assert!(!Files::default().due(third) && !Spotlight::default().files.due(third));
    }

    #[test]
    fn selection_stays_on_its_row_as_files_arrive() {
        let (a, b, c, f) = (Hit::Answer("a".into()), Hit::Answer("b".into()), Hit::Answer("c".into()), Hit::Answer("f".into()));
        let before = [a.clone(), b.clone(), c.clone()];
        let after = [a.clone(), b.clone(), f.clone(), c.clone()];
        assert_eq!(reselect(&before, 1, &after), 1, "above the files: unchanged");
        assert_eq!(reselect(&before, 2, &after), 3, "a setting under them moves down with them");
        assert_eq!(reselect(&before, 2, &[a.clone(), f]), 1, "gone: same place, within the rows");
        assert_eq!(reselect(&before, 0, &[]), 0);
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
        assert_eq!(names(&results(&apps, &[], "12*7")), ["= 84"]);
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

    #[test]
    fn layout_grows_with_text_size() {
        let at = |k| crate::text::with_scale(k, || layout((1920, 1080).into(), 979, 3));
        let (one, big) = (at(1.0), at(1.5));
        assert_eq!((big.field.size.h, big.rows[0].size.h, big.rect.size.w), (78, 60, 1020));
        // Title 1 (26 px lines) and Body (16) at 1.5 still fit their boxes, and every box stays inside the panel.
        assert!(big.field.size.h >= 26 * 3 / 2 + 8 && big.rows[0].size.h >= 2 * 16 && big.rows[0].size.h >= layout::ICON);
        assert!(big.rows.iter().chain([&big.field, &big.magnifier]).all(|r| big.rect.contains_rect(*r)));
        assert!(big.rect.size.h > one.rect.size.h && big.rect.loc.y + big.rect.size.h <= 979);
    }
}
