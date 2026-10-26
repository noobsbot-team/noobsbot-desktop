//! The emoji picker (E7.2): Super+Ctrl+E (or Super+.) opens a popover at the text cursor with a search field, a
//! Recent row, the category's grid and category tabs. Arrow keys move, Return types the emoji into the focused text
//! field over text-input-v3 (or copies it when the app has none), Tab switches category, Esc closes. The names come
//! from Unicode's emoji-test.txt (`data/emoji.txt`, made by `scripts/emoji-table.py`).

use std::{path::PathBuf, sync::OnceLock};

use smithay::{
    input::keyboard::{Keysym, ModifiersState},
    utils::{Logical, Point, Rectangle, Size},
};

use crate::{shell::Osd, state::State, symbols::Symbol, ui::field::Edit, ui::field::TextField, wm::Rect};

pub mod layout {
    pub const PAD: i32 = 10;
    pub const COLS: usize = 9;
    pub const CELL: i32 = 36;
    /// The field and the headers grow with the text size (`text::ts`); the emoji cells and tabs don't.
    pub const FIELD: i32 = 28;
    pub const HEADER: i32 = 20;
    pub const TAB: i32 = 30;
    pub const RADIUS: f64 = 12.0;
    pub const WIDTH: i32 = 2 * PAD + COLS as i32 * CELL;

    /// The grid's height: a header and seven rows, or with the Recent row (and its header) five.
    pub fn body() -> i32 {
        use crate::text::ts;
        2 * ts(HEADER) - HEADER + 7 * CELL
    }

    pub fn height() -> i32 {
        PAD + crate::text::ts(FIELD) + 8 + body() + 8 + TAB + PAD
    }
}

/// Recent emoji kept and shown: one row.
const RECENT: usize = layout::COLS;

/// One per category tab, in Unicode's order.
pub const TAB_ICONS: [&str; 9] = ["😀", "👋", "🐻", "🍔", "🚗", "⚽", "💡", "🔣", "🏁"];

pub struct Category {
    pub name: &'static str,
    /// (emoji, name).
    pub emoji: Vec<(&'static str, &'static str)>,
}

/// The table, parsed on first use.
pub fn table() -> &'static [Category] {
    static TABLE: OnceLock<Vec<Category>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut out: Vec<Category> = Vec::new();
        for line in include_str!("../../data/emoji.txt").lines() {
            match (line.strip_prefix("= "), line.split_once(' '), out.last_mut()) {
                (Some(name), ..) => out.push(Category { name, emoji: Vec::new() }),
                (None, Some(pair), Some(category)) => category.emoji.push(pair),
                _ => {}
            }
        }
        out
    })
}

/// Emoji whose name matches `query` (lower case): whole words and substrings only, best first, in table order.
// ponytail: a linear scan of ~1900 names per keystroke (well under a millisecond); no keyword aliases ("lol") until
// CLDR annotations are bundled.
pub fn search(query: &str) -> Vec<(&'static str, &'static str)> {
    let query = query.trim().to_lowercase();
    let mut hits: Vec<(i32, (&str, &str))> = table()
        .iter()
        .flat_map(|c| c.emoji.iter())
        .filter_map(|&(e, name)| Some((crate::shell::spotlight::score(name, &query).filter(|s| *s >= 700)?, (e, name))))
        .collect();
    hits.sort_by_key(|(score, _)| -score);
    hits.into_iter().map(|(_, hit)| hit).collect()
}

/// `recent` with `emoji` in front, once, at most `RECENT` long.
fn remember(recent: &[String], emoji: &str) -> Vec<String> {
    std::iter::once(emoji.to_owned()).chain(recent.iter().filter(|e| *e != emoji).cloned()).take(RECENT).collect()
}

fn recent_path() -> PathBuf {
    crate::config::xdg_dir("XDG_STATE_HOME", ".local/state").join("noobsbot-desktop/emoji-recent")
}

/// A grid move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// Items are `recent` (one row, when any) then `grid` (rows of `COLS`): where `dir` takes item `i`.
pub fn step(recent: usize, grid: usize, i: usize, dir: Dir) -> usize {
    use layout::COLS;
    let total = recent + grid;
    if total == 0 {
        return 0;
    }
    // (row, col) and back; the Recent row is row 0 when there is one.
    let off = usize::from(recent > 0);
    let pos = |i: usize| if i < recent { (0, i) } else { (off + (i - recent) / COLS, (i - recent) % COLS) };
    let row_len = |r: usize| if r < off { recent } else { grid.saturating_sub((r - off) * COLS).min(COLS) };
    let index = |r: usize, c: usize| if r < off { c } else { recent + (r - off) * COLS + c };
    let (row, col) = pos(i.min(total - 1));
    let rows = off + grid.div_ceil(COLS);
    match dir {
        Dir::Left => i.saturating_sub(1),
        Dir::Right => (i + 1).min(total - 1),
        Dir::Up if row > 0 => index(row - 1, col.min(row_len(row - 1) - 1)),
        Dir::Down if row + 1 < rows => index(row + 1, col.min(row_len(row + 1) - 1)),
        _ => i.min(total - 1),
    }
}

/// The first grid row shown, so grid row `row` is on screen when `rows` fit.
pub fn scrolled(scroll: usize, row: usize, rows: usize) -> usize {
    scroll.min(row).max((row + 1).saturating_sub(rows))
}

/// Where everything is, output-relative.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub rect: Rect,
    pub magnifier: Rect,
    pub field: Rect,
    /// The Recent header and its row of cells, when there are recent emoji to show.
    pub recent: Option<(Rect, Vec<Rect>)>,
    /// The category (or "Results") header.
    pub header: Rect,
    /// The visible grid cells, row by row.
    pub grid: Vec<Rect>,
    pub tabs: Vec<Rect>,
}

/// The popover with its top-left at `at`; `recent`: the Recent row shows.
pub fn layout(at: Point<i32, Logical>, recent: bool) -> Layout {
    use layout::*;
    let (x, y) = (at.x, at.y);
    let (field_h, header_h) = (crate::text::ts(FIELD), crate::text::ts(HEADER));
    let rect = Rectangle::new(at, (WIDTH, height()).into());
    let field = Rectangle::new((x + PAD, y + PAD).into(), (WIDTH - 2 * PAD, field_h).into());
    let magnifier = Rectangle::new((x + PAD + 6, y + PAD + (field_h - 16) / 2).into(), (16, 16).into());
    let cells = |top: i32, rows: i32| -> Vec<Rect> {
        (0..rows * COLS as i32).map(|i| Rectangle::new((x + PAD + i % COLS as i32 * CELL, top + i / COLS as i32 * CELL).into(), (CELL, CELL).into())).collect()
    };
    let mut top = y + PAD + field_h + 8;
    let body_end = top + body();
    let recent = recent.then(|| {
        let header = Rectangle::new((x + PAD + 4, top).into(), (WIDTH - 2 * PAD - 8, header_h).into());
        let row = cells(top + header_h, 1);
        top += header_h + CELL + 8;
        (header, row)
    });
    let header = Rectangle::new((x + PAD + 4, top).into(), (WIDTH - 2 * PAD - 8, header_h).into());
    let grid = cells(top + header_h, (body_end - top - header_h) / CELL);
    let tab_y = body_end + 8;
    let tabs = (0..TAB_ICONS.len() as i32).map(|i| Rectangle::new((x + PAD + i * CELL, tab_y).into(), (CELL, TAB).into())).collect();
    Layout { rect, magnifier, field, recent, header, grid, tabs }
}

/// The popover's top-left for a text `cursor` (global, made output-relative by the caller) on an output of `size`:
/// under the cursor, or above it without room below; centred without a cursor.
pub fn anchor(cursor: Option<Rect>, size: Size<i32, Logical>) -> Point<i32, Logical> {
    let popover = Size::from((layout::WIDTH, layout::height()));
    match cursor {
        Some(cursor) => crate::ime::place(Rectangle::new(cursor.loc, (cursor.size.w, cursor.size.h + 4).into()), popover, Rectangle::from_size(size)),
        None => ((size.w - popover.w) / 2, (size.h - popover.h) / 2).into(),
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Picker {
    pub field: TextField,
    /// The category tab, an index into `table()`.
    pub tab: usize,
    /// Into `items()`: the Recent row first, then the grid.
    pub selected: usize,
    /// The first grid row shown.
    pub scroll: usize,
    pub recent: Vec<String>,
    /// Top-left, output-relative.
    pub at: Point<i32, Logical>,
}

impl Picker {
    /// The Recent row (empty while searching) and the grid: the category's emoji or the search results.
    pub fn items(&self) -> (&[String], Vec<(&'static str, &'static str)>) {
        if self.field.text.trim().is_empty() {
            (&self.recent, table().get(self.tab).map(|c| c.emoji.clone()).unwrap_or_default())
        } else {
            (&[], search(&self.field.text))
        }
    }

    pub fn layout(&self) -> Layout {
        layout(self.at, !self.items().0.is_empty())
    }

    /// The selected emoji and its name ("" for a recent one).
    fn current(&self) -> Option<(String, &'static str)> {
        let (recent, grid) = self.items();
        match recent.get(self.selected) {
            Some(e) => Some((e.clone(), table().iter().flat_map(|c| &c.emoji).find(|(x, _)| x == e).map_or("", |(_, n)| n))),
            None => grid.get(self.selected - recent.len()).map(|(e, n)| (e.to_string(), *n)),
        }
    }

    /// The header's words: the category or "Results", and the selected emoji's name.
    pub fn header(&self) -> (&'static str, &'static str) {
        let title = if self.field.text.trim().is_empty() { table().get(self.tab).map_or("", |c| c.name) } else { "Results" };
        (title, self.current().map_or("", |(_, name)| name))
    }

    /// `selected` moved: scroll it into view.
    fn follow(&mut self) {
        let (recent, grid) = (self.items().0.len(), self.layout().grid.len() / layout::COLS);
        if let Some(i) = self.selected.checked_sub(recent) {
            self.scroll = scrolled(self.scroll, i / layout::COLS, grid);
        }
    }

    fn show_tab(&mut self, tab: usize) {
        self.tab = tab % table().len().max(1);
        self.field = TextField::default();
        self.selected = self.recent.len();
        self.scroll = 0;
    }
}

impl State {
    /// Super+Ctrl+E: opens the picker at the focused text field's cursor (centred without one), or closes it.
    pub fn toggle_emoji(&mut self) {
        if self.emoji.take().is_none() {
            let cursor = self.text_cursor().map(|c| Rectangle::new(self.to_output(c.loc.to_f64()).to_i32_round(), c.size));
            let size = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map_or((1280, 800).into(), |g| g.size);
            let recent: Vec<String> = std::fs::read_to_string(recent_path()).unwrap_or_default().lines().take(RECENT).map(String::from).collect();
            self.emoji = Some(Picker { recent, at: anchor(cursor, size), ..Picker::default() });
            self.popup = None;
        }
        self.schedule_redraw();
    }

    /// While open the picker takes every key but bound shortcuts. `true` if the key was taken.
    pub fn emoji_key(&mut self, sym: Keysym, raw: Keysym, mods: &ModifiersState) -> bool {
        let Some(mut open) = self.emoji.clone() else { return false };
        if crate::shortcuts::lookup(&self.bindings, mods, raw, self.nested).is_some() {
            return false;
        }
        let (recent, grid) = {
            let (r, g) = open.items();
            (r.len(), g.len())
        };
        let dir = match sym {
            Keysym::Left | Keysym::KP_Left => Some(Dir::Left),
            Keysym::Right | Keysym::KP_Right => Some(Dir::Right),
            Keysym::Up | Keysym::KP_Up => Some(Dir::Up),
            Keysym::Down | Keysym::KP_Down => Some(Dir::Down),
            _ => None,
        };
        if let Some(dir) = dir {
            open.selected = step(recent, grid, open.selected, dir);
        } else if matches!(sym, Keysym::Tab | Keysym::ISO_Left_Tab) {
            let back = mods.shift || sym == Keysym::ISO_Left_Tab;
            let count = table().len();
            open.show_tab(if back { open.tab + count - 1 } else { open.tab + 1 });
        } else {
            let typed = sym.key_char().map(String::from);
            match open.field.key(sym, typed.as_deref(), mods.ctrl) {
                Edit::Changed => {
                    open.scroll = 0;
                    open.selected = 0;
                }
                Edit::Moved | Edit::Ignored => {}
                Edit::Cancel => {
                    self.emoji = None;
                    self.schedule_redraw();
                    return true;
                }
                Edit::Submit => {
                    if let Some((emoji, _)) = open.current() {
                        self.emoji_insert(&emoji, &open.recent);
                    }
                    return true;
                }
            }
        }
        open.follow();
        self.emoji = Some(open);
        self.schedule_redraw();
        true
    }

    /// Types `emoji` into the focused app (or copies it, with a toast), remembers it and closes the picker.
    fn emoji_insert(&mut self, emoji: &str, recent: &[String]) {
        self.emoji = None;
        self.schedule_redraw();
        if !self.commit_text(emoji) {
            let seat = self.seat.clone();
            self.clipboard.set_text(&self.display_handle, &seat, emoji);
            self.show_osd(Osd { symbol: Symbol::Check, level: None, text: Some("Copied to Clipboard".into()) });
        }
        let path = recent_path();
        let saved = path.parent().map(std::fs::create_dir_all).transpose().and_then(|_| std::fs::write(&path, remember(recent, emoji).join("\n")));
        if let Err(err) = saved {
            tracing::warn!("emoji: saving {}: {err}", path.display());
        }
    }

    /// A press at `location` (output-relative): a cell types its emoji, a tab switches; outside, the picker closes.
    /// `true`: taken.
    pub fn emoji_press(&mut self, location: Point<f64, Logical>) -> bool {
        let Some(mut open) = self.emoji.clone() else { return false };
        let l = open.layout();
        let hit = |rects: &[Rect]| rects.iter().position(|r| r.to_f64().contains(location));
        if !l.rect.to_f64().contains(location) {
            self.emoji = None;
        } else if let Some(tab) = hit(&l.tabs) {
            open.show_tab(tab);
            self.emoji = Some(open);
        } else {
            let (recent, grid) = open.items();
            let picked = match l.recent.as_ref().and_then(|(_, row)| hit(row)) {
                Some(i) => recent.get(i).cloned(),
                None => hit(&l.grid).and_then(|i| grid.get(open.scroll * layout::COLS + i)).map(|(e, _)| e.to_string()),
            };
            if let Some(emoji) = picked {
                let recent = open.recent.clone();
                self.emoji_insert(&emoji, &recent);
            }
        }
        self.schedule_redraw();
        true
    }

    /// The wheel over the picker scrolls its grid a row per step. `true`: taken.
    pub fn emoji_scroll(&mut self, location: Point<f64, Logical>, dy: f64) -> bool {
        let Some(open) = self.emoji.as_mut() else { return false };
        let l = open.layout();
        if !l.rect.to_f64().contains(location) {
            return false;
        }
        let rows = open.items().1.len().div_ceil(layout::COLS);
        let shown = l.grid.len() / layout::COLS;
        let step = if dy > 0.0 {
            1
        } else if dy < 0.0 {
            -1
        } else {
            0
        };
        open.scroll = open.scroll.saturating_add_signed(step).min(rows.saturating_sub(shown));
        self.schedule_redraw();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_every_category() {
        let t = table();
        assert_eq!(t.len(), TAB_ICONS.len());
        assert_eq!(t[0].name, "Smileys & Emotion");
        assert_eq!(t[0].emoji[0], ("😀", "grinning face"));
        assert!(t.iter().all(|c| c.emoji.len() > 50), "{:?}", t.iter().map(|c| c.emoji.len()).collect::<Vec<_>>());
        // The keycap "#️⃣" isn't mistaken for a header.
        assert!(t.iter().any(|c| c.emoji.iter().any(|(_, n)| *n == "keycap: #")));
        for (icon, c) in TAB_ICONS.iter().zip(t) {
            assert!(c.emoji.iter().any(|(e, _)| e == icon), "{icon} not in {}", c.name);
        }
    }

    #[test]
    fn search_by_name() {
        let names = |q| search(q).into_iter().map(|(_, n)| n).collect::<Vec<_>>();
        assert_eq!(names("grinning face").first(), Some(&"grinning face"));
        assert!(names("nepal").contains(&"flag: Nepal"));
        assert!(names("HEART").len() > 10);
        assert!(names("zzqx").is_empty());
        assert!(names("").is_empty());
    }

    #[test]
    fn recent_is_unique_and_capped() {
        let recent: Vec<String> = ["😀", "👋"].map(String::from).to_vec();
        assert_eq!(remember(&recent, "👋"), ["👋", "😀"]);
        let full: Vec<String> = (0..RECENT).map(|i| i.to_string()).collect();
        let r = remember(&full, "🏁");
        assert_eq!((r.len(), r[0].as_str(), r.last().map(String::as_str)), (RECENT, "🏁", Some("7")));
    }

    #[test]
    fn arrow_moves() {
        // 3 recent, 20 in the grid (rows of 9, 9, 2).
        assert_eq!(step(3, 20, 0, Dir::Left), 0);
        assert_eq!(step(3, 20, 2, Dir::Right), 3, "off the Recent row's end into the grid");
        assert_eq!(step(3, 20, 1, Dir::Down), 4, "same column");
        assert_eq!(step(3, 20, 3 + 8, Dir::Up), 2, "clamped to the shorter row");
        assert_eq!(step(3, 20, 3 + 9 + 5, Dir::Down), 3 + 18 + 1, "into the last, short row");
        assert_eq!(step(3, 20, 22, Dir::Down), 22);
        assert_eq!(step(3, 20, 0, Dir::Up), 0);
        assert_eq!(step(0, 20, 4, Dir::Down), 13);
        assert_eq!(step(0, 0, 0, Dir::Down), 0);
        assert_eq!(scrolled(0, 6, 5), 2);
        assert_eq!(scrolled(4, 1, 5), 1);
        assert_eq!(scrolled(2, 4, 5), 2);
    }

    #[test]
    fn layout_fits() {
        let at = Point::from((100, 50));
        for recent in [false, true] {
            let l = layout(at, recent);
            assert_eq!(l.rect, Rectangle::new(at, (layout::WIDTH, layout::height()).into()));
            let row = l.recent.iter().flat_map(|(h, r)| std::iter::once(h).chain(r));
            assert!(l.grid.iter().chain(&l.tabs).chain([&l.field, &l.header]).chain(row).all(|r| l.rect.contains_rect(*r)));
            assert_eq!(l.grid.len() % layout::COLS, 0);
            assert!(l.grid.last().is_some_and(|c| c.loc.y + c.size.h <= l.tabs[0].loc.y));
        }
        assert_eq!(layout(at, false).grid.len(), 7 * layout::COLS);
        assert_eq!(layout(at, true).grid.len(), 5 * layout::COLS);
    }

    #[test]
    fn layout_grows_with_text_size() {
        let at = Point::from((100, 50));
        for recent in [false, true] {
            let one = crate::text::with_scale(1.0, || layout(at, recent));
            assert_eq!(one, layout(at, recent), "1.0 is the default");
            let l = crate::text::with_scale(1.5, || layout(at, recent));
            // A Callout line at 1.5 fits the field, a Caption line the headers; the same emoji show.
            assert_eq!((l.field.size.h, l.header.size.h, l.grid.len(), l.rect.size.w), (42, 30, one.grid.len(), one.rect.size.w));
            let row = l.recent.iter().flat_map(|(h, r)| std::iter::once(h).chain(r));
            assert!(l.grid.iter().chain(&l.tabs).chain([&l.field, &l.header]).chain(row).all(|r| l.rect.contains_rect(*r)));
            assert!(l.grid.last().is_some_and(|c| c.loc.y + c.size.h <= l.tabs[0].loc.y) && l.grid[0].loc.y >= l.header.loc.y + 30);
        }
    }

    #[test]
    fn anchored_at_the_cursor() {
        let size = Size::from((1280, 800));
        assert_eq!(anchor(None, size), Point::from(((1280 - layout::WIDTH) / 2, (800 - layout::height()) / 2)));
        let cursor = Rectangle::new((200, 100).into(), (1, 18).into());
        assert_eq!(anchor(Some(cursor), size), Point::from((200, 122)));
        let low = Rectangle::new((200, 700).into(), (1, 18).into());
        assert_eq!(anchor(Some(low), size).y, 700 - layout::height());
    }
}
