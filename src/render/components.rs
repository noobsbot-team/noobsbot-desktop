//! The E2.1 component gallery (`--show gallery`): every D8 component in its D7 states, on `window-bg` over the whole
//! output under the menu bar. Screenshot it in light and dark and compare with Part D.

use std::sync::Arc;

use smithay::{
    backend::renderer::gles::{GlesRenderer, element::PixelShaderElement},
    utils::{Logical, Point, Rectangle, Size},
};

use super::{
    Material, SceneElement, Shaders, menubar,
    titlebar::{Label, keep_buffers, label_element},
    ui::{Align, CAPTION, HEADLINE, MenuItem, State, Ui},
};
use crate::{
    symbols::{Symbol, Symbols, Weight},
    text::{Rendered, Style, Text},
    ui::field::TextField,
    wm::Rect,
};

const COLUMN: i32 = 380;
const GAP: i32 = 20;
const PAD: i32 = 16;
const INNER: i32 = COLUMN - 2 * PAD;

/// Output size, dark, accent, opacity, scale.
type Key = (Size<i32, Logical>, bool, [u8; 3], u32, u64);
/// Glass back to front reversed (front to back), and the text pieces.
type Drawn = (Key, Vec<PixelShaderElement>, Vec<(Arc<Rendered>, Rect)>);

#[derive(Default)]
pub struct Components {
    cache: Option<Drawn>,
    labels: Vec<Label>,
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    Rectangle::new((x, y).into(), (w, h).into())
}

fn name(state: State) -> &'static str {
    match state {
        State::Normal => "Normal",
        State::Hover => "Hover",
        State::Pressed => "Pressed",
        State::Disabled => "Disabled",
        State::Focused => "Focused",
    }
}

/// A `card` section titled `title`, `h` tall at (x, y), behind the glass drawn since `mark` (its contents, drawn
/// first to learn `h`); returns the y under it.
fn section(ui: &mut Ui, mark: usize, x: i32, y: i32, h: i32, title: &str) -> i32 {
    let n = ui.glass.len();
    ui.material(rect(x, y, COLUMN, h), Material::Card, None);
    let card: Vec<_> = ui.glass.drain(n..).collect();
    ui.glass.splice(mark..mark, card);
    let color = ui.label();
    ui.text(title, Style { color, ..HEADLINE }, rect(x + PAD, y + 12, INNER, 16), Align::Left);
    y + h + GAP
}

fn caption(ui: &mut Ui, words: &str, r: Rect) {
    let color = ui.secondary();
    ui.text(words, Style { color, ..CAPTION }, r, Align::Centre);
}

/// Everything, back to front.
fn draw(ui: &mut Ui, output: Size<i32, Logical>) {
    let bg = if ui.dark { 0x1E as f32 / 255.0 } else { 0xEC as f32 / 255.0 };
    ui.shape(rect(0, 0, output.w, output.h), [bg, bg, bg, 1.0], 0.0);
    let left = ((output.w - 3 * COLUMN - 2 * GAP) / 2).max(GAP);
    let top = menubar::height() + GAP;
    let (c1, c2, c3) = (left, left + COLUMN + GAP, left + 2 * (COLUMN + GAP));
    let body = |y: i32| y + 12 + 16 + 12;

    // Buttons: four kinds × five states.
    let mut y = top;
    let (w, step) = (64, (INNER - 64) / 4);
    let mark = ui.glass.len();
    let mut row = body(y);
    for (i, state) in State::ALL.into_iter().enumerate() {
        caption(ui, name(state), rect(c1 + PAD + i as i32 * step, row, w, 13));
    }
    row += 13 + 8;
    for (title, default, h) in [("OK", true, 22), ("Cancel", false, 22), ("Done", true, 28), ("Options", false, 28)] {
        for (i, state) in State::ALL.into_iter().enumerate() {
            ui.button(rect(c1 + PAD + i as i32 * step, row, w, h), title, default, state);
        }
        row += h + 12;
    }
    y = section(ui, mark, c1, y, row - y + 4, "Buttons");

    // Text fields.
    let half = (INNER - 12) / 2;
    let mark = ui.glass.len();
    let mut row = body(y);
    let filled = |text: &str, secure| TextField { text: text.into(), caret: text.len(), secure, all: false };
    let fields = [
        ("Placeholder", TextField::default(), "Name", State::Normal),
        ("Focused", filled("Noobsbot", false), "", State::Focused),
        ("Secure", filled("hunter22", true), "Password", State::Focused),
        ("Select all", TextField { all: true, ..filled("Selected", false) }, "", State::Focused),
        ("Disabled", filled("Read only", false), "", State::Disabled),
        ("Empty, focused", TextField::secure(), "Password", State::Focused),
    ];
    for pair in fields.chunks(2) {
        for (i, (label, field, placeholder, state)) in pair.iter().enumerate() {
            let x = c1 + PAD + i as i32 * (half + 12);
            let color = ui.secondary();
            ui.text(label, Style { color, ..CAPTION }, rect(x, row, half, 13), Align::Left);
            ui.field(rect(x, row + 17, half, 22), field, placeholder, *state);
        }
        row += 17 + 22 + 12;
    }
    ui.field(rect(c1 + PAD, row, INNER, 52), &TextField::default(), "Spotlight Search", State::Focused);
    row += 52 + 12;
    y = section(ui, mark, c1, y, row - y + 4, "Text Fields");

    // Progress bars and badges.
    let mark = ui.glass.len();
    let mut row = body(y);
    for percent in [0, 35, 100] {
        ui.progress(rect(c1 + PAD, row, INNER, 4), percent);
        row += 4 + 12;
    }
    row += 8;
    for (i, count) in ["1", "12", "99+"].into_iter().enumerate() {
        let icon = rect(c1 + PAD + 8 + i as i32 * 64, row, 40, 40);
        let tile = if ui.dark { [1.0, 1.0, 1.0, 0.16] } else { [0.0, 0.0, 0.0, 0.10] };
        ui.shape(icon, tile, 9.0);
        let color = ui.label();
        ui.symbol(Symbol::Bell, Weight::Fill, 22, icon, color);
        ui.badge(icon, count);
    }
    row += 40 + 12;
    section(ui, mark, c1, y, row - y + 4, "Progress & Badges");

    // Switches and round toggles.
    let mut y = top;
    let mark = ui.glass.len();
    let mut row = body(y);
    let cell = INNER / 6;
    let switches =
        [(true, State::Normal), (false, State::Normal), (true, State::Disabled), (false, State::Disabled), (true, State::Focused), (false, State::Focused)];
    for (i, (on, state)) in switches.into_iter().enumerate() {
        let x = c2 + PAD + i as i32 * cell;
        caption(ui, if on { "On" } else { "Off" }, rect(x, row, cell, 13));
        ui.switch((x + (cell - 26) / 2, row + 19).into(), on, state);
        caption(ui, name(state), rect(x, row + 40, cell, 13));
    }
    row += 40 + 13 + 16;
    let rounds =
        [(true, State::Normal), (false, State::Normal), (false, State::Hover), (false, State::Pressed), (true, State::Disabled), (true, State::Focused)];
    for (i, (on, state)) in rounds.into_iter().enumerate() {
        let x = c2 + PAD + i as i32 * cell;
        ui.round_toggle(rect(x + (cell - 28) / 2, row, 28, 28), Symbol::WifiHigh, on, state);
        caption(ui, name(state), rect(x, row + 34, cell, 13));
    }
    row += 34 + 13 + 12;
    y = section(ui, mark, c2, y, row - y + 4, "Toggles");

    // Sliders.
    let mark = ui.glass.len();
    let mut row = body(y);
    for (symbol, percent) in [(Symbol::Sun, 0), (Symbol::Sun, 45), (Symbol::SpeakerHigh, 100)] {
        ui.slider(rect(c2 + PAD, row, INNER, 28), percent, symbol);
        row += 28 + 10;
    }
    y = section(ui, mark, c2, y, row - y + 6, "Sliders");

    // Segmented controls.
    let mark = ui.glass.len();
    let mut row = body(y);
    ui.segmented(rect(c2 + PAD, row, INNER, 22), &["Light", "Dark", "Auto"], 1);
    row += 22 + 12;
    ui.segmented(rect(c2 + PAD, row, 200, 22), &["Day", "Week"], 0);
    row += 22 + 12;
    y = section(ui, mark, c2, y, row - y + 4, "Segmented Controls");

    // List rows.
    let mark = ui.glass.len();
    let mut row = body(y);
    let rows = [
        (Symbol::WifiHigh, "Home Network", false, State::Normal),
        (Symbol::WifiMedium, "Hover", false, State::Hover),
        (Symbol::WifiLow, "Pressed", false, State::Pressed),
        (Symbol::WifiHigh, "Selected", true, State::Normal),
        (Symbol::Lock, "Disabled", false, State::Disabled),
        (Symbol::Bluetooth, "Focused", false, State::Focused),
    ];
    for (symbol, title, selected, state) in rows {
        ui.list_row(rect(c2 + PAD, row, INNER, 28), symbol, title, selected, state);
        row += 28 + 4;
    }
    section(ui, mark, c2, y, row - y + 8, "List Rows");

    // Menu and tooltip.
    let y = top;
    let items: [MenuItem; 12] = [
        Some(("About This Computer", "", true, false)),
        None,
        Some(("System Settings…", "", true, false)),
        Some(("App Store…", "", false, false)),
        None,
        Some(("Recent Items", "", true, true)),
        None,
        Some(("Force Quit…", "⌥⌘⎋", true, false)),
        None,
        Some(("Sleep", "", true, false)),
        Some(("Restart…", "", true, false)),
        Some(("Lock Screen", "⌃⌘Q", true, false)),
    ];
    let mark = ui.glass.len();
    let menu = ui.menu((c3 + PAD, body(y)).into(), 230, &items, Some(7));
    ui.tooltip((c3 + PAD + 40, menu.loc.y + menu.size.h + 16).into(), "Wi-Fi: Home Network");
    let y = section(ui, mark, c3, y, menu.size.h + 12 + 16 + 12 + 16 + 27 + 16, "Menu & Tooltip");

    // Alert.
    let mark = ui.glass.len();
    let at = Point::from((c3 + (COLUMN - 260) / 2, body(y)));
    let alert = ui.alert(at, "Join “Home Network”?", "Enter the password for this Wi-Fi network.", ["Cancel", "Join"]);
    section(ui, mark, c3, y, alert.size.h + 12 + 16 + 12 + 16, "Alert");
}

impl Components {
    /// Front to back: text, then glass. Built once per output size, theme and scale.
    #[expect(clippy::too_many_arguments, reason = "the frame's drawing context")]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        symbols: &mut Symbols,
        output: Size<i32, Logical>,
        dark: bool,
        accent: [u8; 3],
        glass_opacity: f32,
        scale: f64,
    ) -> Vec<SceneElement> {
        let key = (output, dark, accent, glass_opacity.to_bits(), scale.to_bits());
        if self.cache.as_ref().is_none_or(|(k, _, _)| *k != key) {
            let mut ui = Ui { shaders, text, symbols, dark, accent, glass_opacity, scale, glass: Vec::new(), pieces: Vec::new() };
            draw(&mut ui, output);
            let (mut glass, pieces) = (ui.glass, ui.pieces);
            glass.reverse();
            self.cache = Some((key, glass, pieces));
        }
        let Some((_, glass, pieces)) = &self.cache else { return Vec::new() };
        self.labels = keep_buffers(&self.labels, pieces.iter().map(|(p, _)| p.clone()));
        let mut out: Vec<SceneElement> = self.labels.iter().zip(pieces).filter_map(|(label, (_, r))| label_element(renderer, label, *r, scale)).collect();
        out.extend(glass.iter().cloned().map(SceneElement::Shader));
        out
    }
}
