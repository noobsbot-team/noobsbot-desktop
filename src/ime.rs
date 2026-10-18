//! Input methods (E6.12): input-method-v2 for fcitx5 behind the text-input-v3 that Qt 6, GTK 4 and Chromium speak,
//! the candidate popup placed under the text cursor, and the keyboard layouts the menu bar's input item lists.

use smithay::{
    delegate_input_method_manager,
    desktop::{PopupKind, PopupManager, utils::bbox_from_surface_tree},
    input::keyboard::Layout,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, Rectangle, Size},
    wayland::{
        input_method::{InputMethodHandler, PopupSurface},
        seat::WaylandFocus,
    },
};

use crate::{
    shell::{Osd, Row},
    shortcuts::Action,
    state::State,
    symbols::Symbol,
};

/// The keyboard's xkb layouts, as `[keyboard] layout = "us,np"` lists them.
#[derive(Debug, Default)]
pub struct Layouts {
    /// (code, name): ("np", "Nepali").
    pub list: Vec<(String, String)>,
    /// Index into `list`.
    // ponytail: only our own switches update it; an xkb `grp:` toggle option changes the layout behind its back.
    pub active: usize,
    /// fcitx5 is installed: the menu offers its settings.
    pub fcitx: bool,
}

/// Pairs the configured codes with xkb's names; a missing code (the layout fell back to xkb's default) becomes the
/// name's first two letters.
fn pair(codes: &str, names: Vec<String>) -> Vec<(String, String)> {
    let mut codes = codes.split(',').map(str::trim);
    names
        .into_iter()
        .map(|name| {
            let code = codes.next().filter(|c| !c.is_empty()).map_or_else(|| name.chars().take(2).collect::<String>().to_lowercase(), str::to_owned);
            (code, name)
        })
        .collect()
}

/// Where the candidate popup goes, in global coordinates: under the text cursor, or above it when there's no room
/// below, kept on `screen` sideways.
fn place(cursor: Rectangle<i32, Logical>, size: Size<i32, Logical>, screen: Rectangle<i32, Logical>) -> Point<i32, Logical> {
    let right = screen.loc.x + screen.size.w - size.w;
    let x = cursor.loc.x.min(right).max(screen.loc.x);
    let below = cursor.loc.y + cursor.size.h;
    let y = if below + size.h <= screen.loc.y + screen.size.h { below } else { (cursor.loc.y - size.h).max(screen.loc.y) };
    (x, y).into()
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

impl State {
    /// Reads the keymap's layouts after the keyboard got a new xkb config; `codes` is its `layout` string.
    pub fn read_layouts(&mut self, codes: &str) {
        let Some(keyboard) = self.seat.get_keyboard() else { return };
        let names = keyboard.with_xkb_state(self, |ctx| {
            let xkb = ctx.xkb().lock().unwrap();
            xkb.layouts().map(|l| xkb.layout_name(l).to_owned()).collect()
        });
        self.layouts = Layouts { list: pair(codes, names), active: 0, fcitx: on_path("fcitx5") };
    }

    /// Switches to layout `index`, or the next one (Super+Ctrl+Space), and says which in the OSD.
    pub fn switch_layout(&mut self, index: Option<usize>) {
        let count = self.layouts.list.len();
        let (Some(keyboard), true) = (self.seat.get_keyboard(), count > 1) else { return };
        let index = index.unwrap_or(self.layouts.active + 1) % count;
        keyboard.with_xkb_state(self, |mut ctx| ctx.set_layout(Layout(index as u32)));
        self.layouts.active = index;
        let text = Some(self.layouts.list[index].1.clone());
        self.show_osd(Osd { symbol: Symbol::Keyboard, level: None, text });
    }

    /// The menu bar's input item: the active layout's code, shown with more than one layout or with fcitx5.
    pub fn input_item(&self) -> Option<String> {
        let layouts = &self.layouts;
        let code = layouts.list.get(layouts.active).map_or_else(|| "EN".into(), |(code, _)| code.to_uppercase());
        (layouts.list.len() > 1 || layouts.fcitx).then_some(code)
    }

    /// The input menu: every layout (the active one ticked), then fcitx5's settings when it's installed.
    // ponytail: fcitx5's own input methods (m17n Nepali, Pinyin) are picked in its tray icon and configtool; listing
    // them here needs its D-Bus controller (org.fcitx.Fcitx.Controller1) on the services thread.
    pub fn input_menu_rows(&self) -> Vec<Option<Row>> {
        let layouts = &self.layouts;
        let mut rows: Vec<Option<Row>> = layouts
            .list
            .iter()
            .enumerate()
            .map(|(i, (_, name))| {
                Some(Row { label: name.clone(), shortcut: if i == layouts.active { "✓" } else { "" }, action: Some(Action::Layout(Some(i))) })
            })
            .collect();
        if layouts.fcitx {
            rows.push(None);
            rows.push(Some(Row { label: "Input Method Settings…".into(), shortcut: "", action: Some(Action::Spawn("fcitx5-configtool".into())) }));
        }
        rows
    }

    /// Moves an input-method popup to under its text cursor, now that its size or the cursor changed.
    fn place_ime_popup(&self, popup: &PopupSurface) {
        let cursor = popup.text_input_rectangle();
        let below = Point::from((cursor.loc.x, cursor.loc.y + cursor.size.h));
        // The parent's surface origin on screen; only windows are known here.
        let parent = popup.get_parent().map(|p| &p.surface);
        let window = self.space.elements().find(|w| w.wl_surface().as_deref() == parent);
        let Some((window, geo)) = window.and_then(|w| Some((w, self.space.element_geometry(w)?))) else {
            // ponytail: a popup or layer surface parent gets no screen-edge flip; add when such a text field shows up.
            popup.set_location(below);
            return;
        };
        let origin = geo.loc - window.geometry().loc;
        let screen = self.space.outputs().filter_map(|o| self.space.output_geometry(o)).find(|g| g.contains(origin + cursor.loc));
        let size = bbox_from_surface_tree(popup.wl_surface(), (0, 0)).size;
        let at = screen.map_or(origin + below, |screen| place(Rectangle::new(origin + cursor.loc, cursor.size), size, screen));
        popup.set_location(at - origin);
    }

    /// An input-method popup committed: its size may have changed.
    pub fn ime_commit(&mut self, surface: &WlSurface) {
        if let Some(PopupKind::InputMethod(popup)) = self.popups.find_popup(surface) {
            self.place_ime_popup(&popup);
        }
    }
}

impl InputMethodHandler for State {
    fn new_popup(&mut self, surface: PopupSurface) {
        self.place_ime_popup(&surface);
        if let Err(err) = self.popups.track_popup(PopupKind::from(surface)) {
            tracing::warn!("input method popup: {err}");
        }
    }

    fn dismiss_popup(&mut self, surface: PopupSurface) {
        if let Some(parent) = surface.get_parent().map(|p| p.surface.clone()) {
            let _ = PopupManager::dismiss_popup(&parent, &PopupKind::from(surface));
        }
        self.schedule_redraw();
    }

    fn popup_repositioned(&mut self, surface: PopupSurface) {
        self.place_ime_popup(&surface);
        self.schedule_redraw();
    }

    /// The window geometry, so the popup's location stays relative to the text field's surface (see
    /// `render::window_elements`).
    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
        self.space.elements().find(|w| w.wl_surface().as_deref() == Some(parent)).map(|w| w.geometry()).unwrap_or_default()
    }
}

delegate_input_method_manager!(State);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_pair_with_names() {
        let names = || vec!["English (US)".to_string(), "Nepali".to_string()];
        assert_eq!(pair("us, np", names()), [("us".into(), "English (US)".into()), ("np".into(), "Nepali".into())]);
        assert_eq!(pair("", names()), [("en".into(), "English (US)".into()), ("ne".into(), "Nepali".into())]);
    }

    #[test]
    fn popup_goes_below_else_above() {
        let screen = Rectangle::new((0, 0).into(), (1000, 800).into());
        let size = Size::from((300, 100));
        let cursor = |x, y| Rectangle::new((x, y).into(), (2, 20).into());
        assert_eq!(place(cursor(100, 100), size, screen), Point::from((100, 120)));
        // No room below: above the cursor.
        assert_eq!(place(cursor(100, 750), size, screen), Point::from((100, 650)));
        // Kept on screen at the right edge.
        assert_eq!(place(cursor(900, 100), size, screen), Point::from((700, 120)));
    }
}
