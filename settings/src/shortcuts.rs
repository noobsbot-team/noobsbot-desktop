//! Keyboard ▸ Keyboard Shortcuts (E8.7): `[bindings]` by category, with labels for the actions the desktop knows
//! (`src/shortcuts.rs` `action`; this crate doesn't depend on the desktop, so the names are listed again here).

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
    fn conflicting_bindings() {
        let all = |list: &[(&str, &str)]| list.iter().map(|(c, a)| (c.to_string(), a.to_string())).collect::<Vec<_>>();
        let found = conflicts(&all(&[("Super+Ctrl+Q", "lock"), ("Ctrl+Super+q", "quit"), ("Super+W", "close"), ("Super+ctrl+Q", "none")]));
        assert_eq!(found.len(), 2);
        assert_eq!(found["Super+Ctrl+Q"], "quit");
        assert_eq!(found["Ctrl+Super+q"], "lock");
    }
}
