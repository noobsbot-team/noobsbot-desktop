//! Keyboard ▸ Keyboard Shortcuts (E8.7): `[bindings]` by category, with labels for the actions the desktop knows
//! (`src/shortcuts.rs` `action`; this crate doesn't depend on the desktop, so the names are listed again here).

use iced::{
    Alignment, Element, Length, Task,
    font::Weight,
    widget::{button, checkbox, column, container, row, scrollable, space, text, tooltip},
};

use crate::{App, Message, font, style};

/// The open "Keyboard Shortcuts…" sheet.
pub struct Editor {
    pub category: &'static str,
}

#[derive(Clone, Debug)]
pub enum Msg {
    Open,
    Close,
    Category(&'static str),
    /// A shipped binding, by combo: on (its default applies) or off ("none").
    Enable(String, bool),
}

/// One row: the combo as written, the action it runs (the default's when switched off), and whether it is on.
#[derive(Debug, PartialEq)]
pub struct Row {
    pub combo: String,
    pub action: String,
    pub on: bool,
    /// Not a shipped binding: − removes it.
    pub custom: bool,
}

/// The rows from every binding (`Config::bindings`) and the shipped defaults. A default switched off whose action is
/// bound to other keys has moved there, and isn't listed.
pub fn rows(all: &[(String, String)], default: impl Fn(&str) -> Option<String>) -> Vec<Row> {
    let mut rows = Vec::new();
    for (combo, action) in all {
        let shipped = default(combo);
        let on = action.trim() != "none";
        let action = if on { action.clone() } else { shipped.clone().unwrap_or_default() };
        let moved = all.iter().any(|(c, a)| c != combo && *a == action);
        if on || (shipped.is_some() && !moved) {
            rows.push(Row { combo: combo.clone(), action, on, custom: shipped.is_none() });
        }
    }
    rows
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    let key = |combo: &str| format!("bindings.{combo}");
    let done = match msg {
        Msg::Open => {
            app.shortcuts = Some(Editor { category: CATEGORIES[0] });
            Ok(())
        }
        Msg::Close => {
            app.shortcuts = None;
            Ok(())
        }
        Msg::Category(category) => {
            if let Some(editor) = &mut app.shortcuts {
                editor.category = category;
            }
            Ok(())
        }
        Msg::Enable(combo, true) => app.config.change(&[key(&combo)], []),
        Msg::Enable(combo, false) => app.config.set(&key(&combo), "none"),
    };
    app.error = done.err().map(|err| format!("Couldn't save the shortcut: {err}"));
    Task::none()
}

/// The sheet: categories left, that category's shortcuts right.
pub fn view(app: &App, editor: &Editor) -> Element<'static, Message> {
    let t = app.tokens;
    let all = app.config.bindings();
    let rows = rows(&all, |combo| app.config.default_of(&format!("bindings.{combo}")).map(str::to_owned));
    let clashes = conflicts(&all);
    let mut sidebar = column![].spacing(1).width(190);
    for &name in CATEGORIES {
        if name == "Other" && !rows.iter().any(|r| category(&r.action) == "Other") {
            continue;
        }
        let item = text(name).size(13);
        sidebar = sidebar.push(
            button(item).padding([4, 8]).width(Length::Fill).style(t.sidebar_row(editor.category == name)).on_press(Message::Shortcuts(Msg::Category(name))),
        );
    }
    let mut list = column![].spacing(2);
    for r in rows.iter().filter(|r| category(&r.action) == editor.category) {
        let enable: Element<'static, Message> = if r.custom {
            space().width(16).into()
        } else {
            let combo = r.combo.clone();
            checkbox(r.on).on_toggle(move |on| Message::Shortcuts(Msg::Enable(combo.clone(), on))).size(14).into()
        };
        let mut line = row![enable, text(label(&r.action)).size(13).width(Length::Fill)].spacing(8).align_y(Alignment::Center);
        if let Some(other) = clashes.get(&r.combo) {
            let tip = container(text(format!("Also bound to {}", label(other))).size(11)).padding([4, 8]).style(t.sheet());
            line = line.push(tooltip(text("⚠").size(13).color(style::rgb(0xFF9500)), tip, tooltip::Position::Top));
        }
        let keys = text(format(&r.combo)).size(13).color(if r.on { t.label() } else { t.tertiary() });
        line = line.push(keys);
        list = list.push(container(line).padding([4, 8]));
    }
    let body = row![
        container(scrollable(sidebar).direction(style::thin()).style(t.scroll())).padding(4).height(Length::Fill).style(t.sidebar()),
        scrollable(list).direction(style::thin()).style(t.scroll()).width(Length::Fill).height(Length::Fill),
    ]
    .spacing(12);
    let bar = row![space().width(Length::Fill), button(text("Done").size(13)).padding([4, 14]).style(t.push(true)).on_press(Message::Shortcuts(Msg::Close))]
        .spacing(8);
    let card = column![text("Keyboard Shortcuts").size(15).font(font(Weight::Semibold)), body, bar].spacing(12).max_width(640).height(Length::Fill);
    let sheet = container(card).padding(18).style(t.sheet());
    iced::widget::opaque(container(sheet).padding(24).center(Length::Fill).style(t.dim()))
}

/// The sidebar, in order; anything not listed below goes to "Other".
pub const CATEGORIES: &[&str] = &["Windows", "Spaces & Mission Control", "Dock", "Screenshots", "Apps & Launchers", "Media & Hardware", "System", "Other"];

/// Actions without an argument: name, label, category.
const KNOWN: &[(&str, &str, &str)] = &[
    ("quit", "Quit App", "Windows"),
    ("close", "Close Window", "Windows"),
    ("minimise", "Minimise Window", "Windows"),
    ("minimize", "Minimise Window", "Windows"),
    ("zoom", "Zoom Window", "Windows"),
    ("fullscreen", "Full Screen", "Windows"),
    ("hide", "Hide App", "Windows"),
    ("hide-others", "Hide Others", "Windows"),
    ("show-all", "Show All", "Windows"),
    ("switcher", "App Switcher", "Windows"),
    ("cycle", "App Switcher", "Windows"),
    ("cycle-app-windows", "Next Window of App", "Windows"),
    ("fill", "Fill Screen", "Windows"),
    ("center", "Centre Window", "Windows"),
    ("centre", "Centre Window", "Windows"),
    ("keep-on-top", "Keep on Top", "Windows"),
    ("force-quit", "Force Quit", "Windows"),
    ("move-to-new-space", "Move Window to New Space", "Spaces & Mission Control"),
    ("mission-control", "Mission Control", "Spaces & Mission Control"),
    ("app-expose", "App Exposé", "Spaces & Mission Control"),
    ("show-desktop", "Show Desktop", "Spaces & Mission Control"),
    ("screenshot", "Screenshot of the Screen", "Screenshots"),
    ("screenshot-region", "Screenshot of a Region", "Screenshots"),
    ("screenshot-toolbar", "Screenshot Toolbar", "Screenshots"),
    ("terminal", "Open Terminal", "Apps & Launchers"),
    ("spotlight", "Spotlight", "Apps & Launchers"),
    ("app-drawer", "App Drawer", "Apps & Launchers"),
    ("launchpad", "Launchpad", "Apps & Launchers"),
    ("notification-center", "Notification Center", "Apps & Launchers"),
    ("emoji", "Emoji & Symbols", "Apps & Launchers"),
    ("quake", "Drop-Down Terminal", "Apps & Launchers"),
    ("mute", "Mute", "Media & Hardware"),
    ("mic-mute", "Mute Microphone", "Media & Hardware"),
    ("lock", "Lock Screen", "System"),
    ("sleep", "Sleep", "System"),
    ("restart", "Restart", "System"),
    ("shut-down", "Shut Down", "System"),
    ("log-out", "Log Out", "System"),
    ("input-source", "Next Input Source", "System"),
];

/// Actions with an argument (`space 3`, `exec firefox`): name and category.
const WITH_ARGUMENT: &[(&str, &str)] = &[
    ("tile", "Windows"),
    ("move-to-display", "Windows"),
    ("space", "Spaces & Mission Control"),
    ("move-to-space", "Spaces & Mission Control"),
    ("dock-app", "Dock"),
    ("dock-app-new", "Dock"),
    ("exec", "Apps & Launchers"),
    ("volume", "Media & Hardware"),
    ("brightness", "Media & Hardware"),
    ("media", "Media & Hardware"),
];

/// The argument-less actions, for the + picker.
pub fn known() -> impl Iterator<Item = &'static str> {
    KNOWN.iter().map(|&(name, ..)| name).filter(|n| !["minimize", "cycle", "centre"].contains(n))
}

fn split(action: &str) -> (&str, &str) {
    action.trim().split_once(' ').map_or((action.trim(), ""), |(a, rest)| (a, rest.trim()))
}

pub fn category(action: &str) -> &'static str {
    let (name, arg) = split(action);
    let fixed = KNOWN.iter().find(|k| arg.is_empty() && k.0 == name).map(|k| k.2);
    fixed.or_else(|| WITH_ARGUMENT.iter().find(|k| !arg.is_empty() && k.0 == name).map(|k| k.1)).unwrap_or("Other")
}

/// "Lock Screen" for `lock`, "Run: firefox" for `exec firefox`, the text itself when unknown.
pub fn label(action: &str) -> String {
    let (name, arg) = split(action);
    let words = |side: &str| side.split('-').map(capital).collect::<Vec<_>>().join(" ");
    match (name, arg) {
        ("exec", command) if !command.is_empty() => format!("Run: {command}"),
        ("space", "left") => "Previous Space".into(),
        ("space", "right") => "Next Space".into(),
        ("space", n) if !n.is_empty() => format!("Go to Space {n}"),
        ("move-to-space", "left") => "Move Window to Previous Space".into(),
        ("move-to-space", "right") => "Move Window to Next Space".into(),
        ("move-to-space", n) if !n.is_empty() => format!("Move Window to Space {n}"),
        ("move-to-display", side) if !side.is_empty() => format!("Move Window to {} Display", words(side)),
        ("dock-app", n) if !n.is_empty() => format!("Dock App {n}"),
        ("dock-app-new", n) if !n.is_empty() => format!("New Window of Dock App {n}"),
        ("tile", side) if !side.is_empty() => format!("Tile {}", words(side)),
        ("volume" | "brightness", way) if !way.is_empty() => format!("{} {}", capital(name), capital(way)),
        ("media", "play-pause") => "Play/Pause".into(),
        ("media", way) if !way.is_empty() => format!("{} Track", capital(way)),
        _ => KNOWN.iter().find(|k| arg.is_empty() && k.0 == name).map_or_else(|| action.trim().to_owned(), |k| k.1.to_owned()),
    }
}

fn capital(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
}

/// A binding as the desktop compares them (`shortcuts::parse`): modifiers (Super, Ctrl, Alt, Shift) and the key in
/// lower case, `Enter` and `Esc` as `return` and `escape`. None when it doesn't parse.
pub fn normalise(combo: &str) -> Option<([bool; 4], String)> {
    let mut parts: Vec<&str> = combo.split('+').map(str::trim).collect();
    let key = parts.pop().filter(|k| !k.is_empty())?.to_ascii_lowercase();
    let mut mods = [false; 4];
    for part in parts {
        let i = ["super", "ctrl", "alt", "shift"].iter().position(|m| part.eq_ignore_ascii_case(m)).or((part.eq_ignore_ascii_case("control")).then_some(1))?;
        mods[i] = true;
    }
    let key = match key.as_str() {
        "enter" => "return".into(),
        "esc" => "escape".into(),
        _ => key,
    };
    Some((mods, key))
}

/// "Super+Ctrl+Left" as shown: ⌃⌥⇧ first (macOS order), then "Super+" (as the README writes it), then the key.
pub fn format(combo: &str) -> String {
    let Some((mods, key)) = normalise(combo) else { return combo.to_owned() };
    let original = combo.rsplit('+').next().unwrap_or_default().trim();
    let mut shown: String = [(mods[1], "⌃"), (mods[2], "⌥"), (mods[3], "⇧")].iter().filter(|m| m.0).map(|m| m.1).collect();
    if mods[0] {
        shown.push_str("Super+");
    }
    let named = KEY_NAMES.iter().find(|k| k.0.eq_ignore_ascii_case(&key)).map(|k| k.1);
    match named {
        Some(name) => shown.push_str(name),
        None if key.chars().count() == 1 => shown.push_str(&key.to_uppercase()),
        None => shown.push_str(original.strip_prefix("XF86").unwrap_or(original)),
    }
    shown
}

/// xkb key names and how they show.
const KEY_NAMES: &[(&str, &str)] = &[
    ("return", "↩"),
    ("escape", "⎋"),
    ("tab", "⇥"),
    ("space", "Space"),
    ("backspace", "⌫"),
    ("delete", "⌦"),
    ("left", "←"),
    ("right", "→"),
    ("up", "↑"),
    ("down", "↓"),
    ("prior", "Page Up"),
    ("next", "Page Down"),
    ("print", "Print Screen"),
    ("period", "."),
    ("comma", ","),
    ("grave", "`"),
    ("minus", "-"),
    ("equal", "="),
    ("slash", "/"),
    ("backslash", "\\"),
    ("semicolon", ";"),
    ("apostrophe", "'"),
    ("bracketleft", "["),
    ("bracketright", "]"),
    ("xf86audioraisevolume", "Volume Up"),
    ("xf86audiolowervolume", "Volume Down"),
    ("xf86audiomute", "Mute"),
    ("xf86audiomicmute", "Mic Mute"),
    ("xf86monbrightnessup", "Brightness Up"),
    ("xf86monbrightnessdown", "Brightness Down"),
    ("xf86audioplay", "Play"),
    ("xf86audiopause", "Pause"),
    ("xf86audionext", "Next Track"),
    ("xf86audioprev", "Previous Track"),
    ("xf86poweroff", "Power"),
    ("xf86sleep", "Sleep"),
];

/// For each active binding (not "none") that parses to the same keys as another: its combo and the other's action.
pub fn conflicts(bindings: &[(String, String)]) -> std::collections::HashMap<String, String> {
    let active: Vec<_> = bindings.iter().filter(|(_, a)| a.trim() != "none").filter_map(|(c, a)| Some((c, a, normalise(c)?))).collect();
    let mut found = std::collections::HashMap::new();
    for (combo, _, keys) in &active {
        if let Some((_, other, _)) = active.iter().find(|(c, _, k)| c != combo && k == keys) {
            found.insert((*combo).clone(), (*other).clone());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_categories() {
        assert_eq!(label("lock"), "Lock Screen");
        assert_eq!(label("terminal"), "Open Terminal");
        assert_eq!(label("exec helium-browser"), "Run: helium-browser");
        assert_eq!(label("space 3"), "Go to Space 3");
        assert_eq!(label("tile top-right"), "Tile Top Right");
        assert_eq!(label("volume up"), "Volume Up");
        assert_eq!(label("media previous"), "Previous Track");
        assert_eq!(label("frobnicate"), "frobnicate");
        assert_eq!(category("lock"), "System");
        assert_eq!(category("exec foot"), "Apps & Launchers");
        assert_eq!(category("dock-app-new 2"), "Dock");
        assert_eq!(category("space"), "Other");
        assert_eq!(category("lock now"), "Other");
        assert_eq!(category("frobnicate"), "Other");
    }

    #[test]
    fn combos() {
        assert_eq!(normalise("Ctrl+Super+x"), normalise("Super+Ctrl+X"));
        assert_eq!(normalise("Super+Enter"), normalise("Super+Return"));
        assert_eq!(normalise("Super+Hyper+X"), None);
        assert_eq!(normalise("Super+"), None);
        assert_eq!(format("Super+Ctrl+Left"), "⌃Super+←");
        assert_eq!(format("Super+Shift+3"), "⇧Super+3");
        assert_eq!(format("Super+period"), "Super+.");
        assert_eq!(format("XF86AudioRaiseVolume"), "Volume Up");
        assert_eq!(format("Super+F11"), "Super+F11");
        assert_eq!(format("Super+q"), "Super+Q");
    }

    #[test]
    fn moved_defaults_are_not_listed() {
        let all = [("Super+K", "quit"), ("Super+Q", "none"), ("Super+W", "none"), ("Super+X", "none"), ("Super+Y", "exec foot")];
        let all: Vec<_> = all.iter().map(|(c, a)| (c.to_string(), a.to_string())).collect();
        let shipped = |c: &str| ["Super+Q", "Super+W"].contains(&c).then(|| if c == "Super+Q" { "quit" } else { "close" }.to_string());
        let shown: Vec<_> = rows(&all, shipped).into_iter().map(|r| (r.combo, r.action, r.on, r.custom)).collect();
        let row = |c: &str, a: &str, on, custom| (c.to_string(), a.to_string(), on, custom);
        assert_eq!(shown, [row("Super+K", "quit", true, true), row("Super+W", "close", false, false), row("Super+Y", "exec foot", true, true)]);
    }

    #[test]
    fn conflicting_bindings() {
        let all = |list: &[(&str, &str)]| list.iter().map(|(c, a)| (c.to_string(), a.to_string())).collect::<Vec<_>>();
        let found = conflicts(&all(&[("Super+Ctrl+Q", "lock"), ("Ctrl+Super+q", "quit"), ("Super+W", "close"), ("Super+ctrl+Q", "none")]));
        assert_eq!(found.len(), 2);
        assert_eq!(found["Super+Ctrl+Q"], "quit");
        assert_eq!(found["Ctrl+Super+q"], "lock");
    }
}
