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
    /// The row (by combo) whose keys the next key press sets; empty: the new shortcut's.
    pub recording: Option<String>,
    /// The custom shortcut − removes, by combo.
    pub selected: Option<String>,
    /// The new shortcut (+): its combo and action, until Add.
    pub draft: Option<(String, String)>,
}

#[derive(Clone, Debug)]
pub enum Msg {
    Open,
    Close,
    Category(&'static str),
    /// A shipped binding, by combo: on (its default applies) or off ("none").
    Enable(String, bool),
    /// Clicked a row's keys: the next key press replaces them.
    Record(String),
    /// A key pressed while recording.
    Key(iced::keyboard::Key, iced::keyboard::Modifiers),
    Select(String),
    /// + : a new shortcut's row, its keys recording.
    New,
    DraftAction(String),
    Add,
    Remove,
    /// Every `[bindings]` line of the user's file goes; the desktop's defaults apply.
    Restore,
}

/// An action in the + picker, shown by its label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pick(&'static str);

impl std::fmt::Display for Pick {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&label(self.0))
    }
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

/// Moving `action` from the keys `old` to `new` (`None`: no keys): the keys to remove and to set. A shipped binding
/// is switched off rather than removed, or its default would come back.
pub fn moves(old: &str, new: Option<&str>, action: &str, shipped: impl Fn(&str) -> bool) -> (Vec<String>, Vec<(String, String)>) {
    let key = |combo: &str| format!("bindings.{combo}");
    let mut set: Vec<_> = new.map(|new| (key(new), action.to_owned())).into_iter().collect();
    let mut remove = Vec::new();
    if new != Some(old) {
        if shipped(old) {
            set.push((key(old), "none".to_owned()));
        } else {
            remove.push(key(old));
        }
    }
    (remove, set)
}

/// Asks the desktop to pass every key to us (`Record(true)`) while a shortcut is recorded, so Super+… reaches the
/// sheet. Without it (desktop not running, or older), we still record whatever keys reach us.
async fn record(on: bool) {
    let call = async {
        let bus = zbus::connection::Builder::session()?.method_timeout(std::time::Duration::from_secs(2)).build().await?;
        bus.call_method(Some("org.noobsbot.Desktop"), "/org/noobsbot/Desktop", Some("org.noobsbot.Desktop.Shortcuts"), "Record", &(on,)).await?;
        zbus::Result::Ok(())
    };
    if let Err(err) = call.await {
        eprintln!("shortcut recording: {err}");
    }
}

/// A key press while recording, as a `Message`.
pub fn key_pressed(event: iced::Event, _: iced::event::Status, _: iced::window::Id) -> Option<Message> {
    let iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, modifiers, repeat: false, .. }) = event else { return None };
    Some(Message::Shortcuts(Msg::Key(key, modifiers)))
}

/// A key press as a binding: "Super+Ctrl+Alt+Shift+" and the xkb key name. None for a modifier on its own, or a key
/// a binding can't name.
pub fn combo(key: &iced::keyboard::Key, mods: iced::keyboard::Modifiers) -> Option<String> {
    use iced::keyboard::{Key, key::Named as N};
    let name = match key {
        Key::Character(c) => match KEY_NAMES.iter().find(|k| k.1 == c.as_str() && k.0.len() > 1) {
            Some(k) => k.0.to_owned(),
            None if c.chars().count() == 1 => c.to_uppercase(),
            None => return None,
        },
        Key::Named(named) => match named {
            N::Enter => "Return",
            N::Tab => "Tab",
            N::Space => "space",
            N::Escape => "Escape",
            N::Backspace => "BackSpace",
            N::Delete => "Delete",
            N::Insert => "Insert",
            N::Home => "Home",
            N::End => "End",
            N::PageUp => "Prior",
            N::PageDown => "Next",
            N::ArrowLeft => "Left",
            N::ArrowRight => "Right",
            N::ArrowUp => "Up",
            N::ArrowDown => "Down",
            N::PrintScreen => "Print",
            N::AudioVolumeUp => "XF86AudioRaiseVolume",
            N::AudioVolumeDown => "XF86AudioLowerVolume",
            N::AudioVolumeMute => "XF86AudioMute",
            N::MicrophoneVolumeMute => "XF86AudioMicMute",
            N::BrightnessUp => "XF86MonBrightnessUp",
            N::BrightnessDown => "XF86MonBrightnessDown",
            N::MediaPlayPause | N::MediaPlay => "XF86AudioPlay",
            N::MediaPause => "XF86AudioPause",
            N::MediaTrackNext => "XF86AudioNext",
            N::MediaTrackPrevious => "XF86AudioPrev",
            N::Power => "XF86PowerOff",
            // F1 … F35.
            other => {
                return Some(format!("{other:?}"))
                    .filter(|n| n.len() > 1 && n.starts_with('F') && n[1..].bytes().all(|b| b.is_ascii_digit()))
                    .map(|n| prefix(mods) + &n);
            }
        }
        .to_owned(),
        _ => return None,
    };
    Some(prefix(mods) + &name)
}

fn prefix(mods: iced::keyboard::Modifiers) -> String {
    [(mods.logo(), "Super+"), (mods.control(), "Ctrl+"), (mods.alt(), "Alt+"), (mods.shift(), "Shift+")].iter().filter(|m| m.0).map(|m| m.1).collect()
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    let key = |combo: &str| format!("bindings.{combo}");
    let done = match msg {
        Msg::Open => {
            app.shortcuts = Some(Editor { category: CATEGORIES[0], recording: None, selected: None, draft: None });
            Ok(())
        }
        Msg::Close => {
            let recording = app.shortcuts.take().is_some_and(|e| e.recording.is_some());
            return if recording { Task::future(record(false)).discard() } else { Task::none() };
        }
        Msg::Record(combo) => {
            let Some(editor) = &mut app.shortcuts else { return Task::none() };
            let was = editor.recording.replace(combo).is_some();
            return if was { Task::none() } else { Task::future(record(true)).discard() };
        }
        Msg::Key(pressed, mods) => {
            use iced::keyboard::{Key, key::Named};
            let Some(old) = app.shortcuts.as_mut().and_then(|e| e.recording.clone()) else { return Task::none() };
            let new = match pressed {
                Key::Named(Named::Escape) if mods.is_empty() => None,
                Key::Named(Named::Backspace) if mods.is_empty() => Some(None),
                _ => match combo(&pressed, mods) {
                    Some(new) => Some(Some(new)),
                    // A modifier on its own, or a key with no name in a binding: keep listening.
                    None => return Task::none(),
                },
            };
            let Some(editor) = &mut app.shortcuts else { return Task::none() };
            editor.recording = None;
            if old.is_empty() {
                if let (Some(draft), Some(new)) = (&mut editor.draft, new) {
                    draft.0 = new.unwrap_or_default();
                }
                return Task::future(record(false)).discard();
            }
            let action = app.config.bindings().into_iter().find(|(c, _)| *c == old).map(|(_, a)| a).unwrap_or_default();
            let action = if action.trim() == "none" { app.config.default_of(&key(&old)).unwrap_or_default().to_owned() } else { action };
            let shipped = |c: &str| app.config.default_of(&key(c)).is_some();
            let saved = match new {
                Some(new) => {
                    let (remove, set) = moves(&old, new.as_deref(), &action, shipped);
                    app.config.change(&remove, set.iter().map(|(k, v)| (k.as_str(), v.as_str().into())))
                }
                None => Ok(()),
            };
            app.error = saved.err().map(|err| format!("Couldn't save the shortcut: {err}"));
            return Task::future(record(false)).discard();
        }
        Msg::Category(category) => {
            if let Some(editor) = &mut app.shortcuts {
                editor.category = category;
            }
            Ok(())
        }
        Msg::Select(combo) => {
            if let Some(editor) = &mut app.shortcuts {
                editor.selected = Some(combo);
            }
            Ok(())
        }
        Msg::New => {
            let Some(editor) = &mut app.shortcuts else { return Task::none() };
            editor.draft = Some(Default::default());
            return update(app, Msg::Record(String::new()));
        }
        Msg::DraftAction(action) => {
            if let Some((_, draft)) = app.shortcuts.as_mut().and_then(|e| e.draft.as_mut()) {
                *draft = action;
            }
            Ok(())
        }
        Msg::Add => {
            let Some(editor) = &mut app.shortcuts else { return Task::none() };
            let Some((combo, action)) = editor.draft.take() else { return Task::none() };
            editor.category = category(&action);
            app.config.set(&key(&combo), action.trim())
        }
        Msg::Remove => match app.shortcuts.as_mut().and_then(|e| e.selected.take()) {
            Some(combo) => app.config.change(&[key(&combo)], []),
            None => Ok(()),
        },
        Msg::Restore => {
            let all: Vec<String> = app.config.keys("bindings").iter().map(|c| key(c)).collect();
            app.config.change(&all, [])
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
    let nudge = app.config.bool("windows.keyboard_nudge");
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
    if let Some((combo, action)) = &editor.draft {
        let recording = editor.recording.as_deref() == Some("");
        let shown = if recording {
            "Type keys…".to_owned()
        } else if combo.is_empty() {
            "Click to record".to_owned()
        } else {
            format(combo)
        };
        let keys = button(text(shown).size(13)).padding([2, 8]).style(t.segment(recording)).on_press(Message::Shortcuts(Msg::Record(String::new())));
        let typed = iced::widget::text_input("exec firefox", action)
            .on_input(|a| Message::Shortcuts(Msg::DraftAction(a)))
            .size(12)
            .padding([3, 8])
            .style(t.input(false));
        let picks: Vec<Pick> = known().map(Pick).collect();
        let pick = iced::widget::pick_list(picks, None::<Pick>, |p| Message::Shortcuts(Msg::DraftAction(p.0.to_owned())))
            .placeholder("Action")
            .text_size(12)
            .padding([3, 8])
            .style(t.pick())
            .menu_style(t.menu());
        let ready = normalise(combo).is_some() && !action.trim().is_empty();
        let add = button(text("Add").size(12)).padding([3, 10]).style(t.push(true)).on_press_maybe(ready.then_some(Message::Shortcuts(Msg::Add)));
        list = list.push(container(row![keys, typed, pick, add].spacing(6).align_y(Alignment::Center)).padding([4, 8]));
    }
    for r in rows.iter().filter(|r| category(&r.action) == editor.category) {
        // Nudge and resize keys do nothing while their switch is off.
        let inert = !nudge && needs_nudge(&r.action);
        let enable: Element<'static, Message> = if r.custom {
            space().width(16).into()
        } else {
            let combo = r.combo.clone();
            checkbox(r.on).on_toggle(move |on| Message::Shortcuts(Msg::Enable(combo.clone(), on))).size(14).into()
        };
        let name: Element<'static, Message> = if r.custom {
            let selected = editor.selected.as_ref() == Some(&r.combo);
            button(text(label(&r.action)).size(13))
                .padding(0)
                .width(Length::Fill)
                .style(t.sidebar_row(selected))
                .on_press(Message::Shortcuts(Msg::Select(r.combo.clone())))
                .into()
        } else if inert {
            let tip = container(text("Turn on Move and resize with the keyboard").size(11)).padding([4, 8]).style(t.sheet());
            tooltip(text(label(&r.action)).size(13).width(Length::Fill).color(t.tertiary()), tip, tooltip::Position::Top).into()
        } else {
            text(label(&r.action)).size(13).width(Length::Fill).into()
        };
        let mut line = row![enable, name].spacing(8).align_y(Alignment::Center);
        if let Some(other) = clashes.get(&r.combo) {
            let tip = container(text(format!("Also bound to {}", label(other))).size(11)).padding([4, 8]).style(t.sheet());
            line = line.push(tooltip(text("⚠").size(13).color(style::rgb(0xFF9500)), tip, tooltip::Position::Top));
        }
        let recording = editor.recording.as_ref() == Some(&r.combo);
        let shown = if recording { "Type keys…".to_owned() } else { format(&r.combo) };
        let keys = text(shown).size(13).color(if (r.on && !inert) || recording { t.label() } else { t.tertiary() });
        line = line.push(button(keys).padding([2, 8]).style(t.segment(recording)).on_press(Message::Shortcuts(Msg::Record(r.combo.clone()))));
        list = list.push(container(line).padding([4, 8]));
    }
    let body = row![
        container(scrollable(sidebar).direction(style::thin()).style(t.scroll())).padding(4).height(Length::Fill).style(t.sidebar()),
        scrollable(list).direction(style::thin()).style(t.scroll()).width(Length::Fill).height(Length::Fill),
    ]
    .spacing(12);
    let small = |label: &'static str, msg: Option<Msg>| {
        button(text(label).size(13).center()).padding([2, 10]).style(t.push(false)).on_press_maybe(msg.map(Message::Shortcuts))
    };
    let selected = editor.selected.is_some().then_some(Msg::Remove);
    let bar = row![
        small("+", Some(Msg::New)),
        small("−", selected),
        space().width(Length::Fill),
        small("Restore Defaults", Some(Msg::Restore)),
        button(text("Done").size(13)).padding([4, 14]).style(t.push(true)).on_press(Message::Shortcuts(Msg::Close))
    ]
    .spacing(8);
    let card = column![text("Keyboard Shortcuts").size(15).font(font(Weight::Semibold)), body, bar].spacing(12).max_width(640).height(Length::Fill);
    let sheet = container(card).padding(18).style(t.sheet());
    iced::widget::opaque(container(sheet).padding(24).center(Length::Fill).style(t.dim()))
}

/// The sidebar, in order; anything not listed below goes to "Other".
pub const CATEGORIES: &[&str] =
    &["Windows", "Spaces & Mission Control", "Dock", "Screenshots", "Apps & Launchers", "Media & Hardware", "Accessibility", "System", "Other"];

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
    ("screenshot-window", "Screenshot of the Window", "Screenshots"),
    ("screen-record", "Record the Screen", "Screenshots"),
    ("terminal", "Open Terminal", "Apps & Launchers"),
    ("spotlight", "Spotlight", "Apps & Launchers"),
    ("app-drawer", "App Drawer", "Apps & Launchers"),
    ("launchpad", "Launchpad", "Apps & Launchers"),
    ("notification-center", "Notification Center", "Apps & Launchers"),
    ("emoji", "Emoji & Symbols", "Apps & Launchers"),
    ("quake", "Drop-Down Terminal", "Apps & Launchers"),
    ("mute", "Mute", "Media & Hardware"),
    ("mic-mute", "Mute Microphone", "Media & Hardware"),
    ("eject", "Eject", "Media & Hardware"),
    ("screen-zoom", "Zoom On or Off", "Accessibility"),
    ("on-screen-keyboard", "On-Screen Keyboard", "Accessibility"),
    ("lock", "Lock Screen", "System"),
    ("sleep", "Sleep", "System"),
    ("restart", "Restart", "System"),
    ("shut-down", "Shut Down", "System"),
    ("log-out", "Log Out", "System"),
    ("power-button", "Sleep, Restart or Shut Down", "System"),
    ("input-source", "Next Input Source", "System"),
    ("keyboard-viewer", "Keyboard Viewer", "System"),
];

/// Actions with an argument (`space 3`, `exec firefox`): name and category.
const WITH_ARGUMENT: &[(&str, &str)] = &[
    ("tile", "Windows"),
    ("nudge", "Windows"),
    ("resize", "Windows"),
    ("move-to-display", "Windows"),
    ("space", "Spaces & Mission Control"),
    ("move-to-space", "Spaces & Mission Control"),
    ("dock-app", "Dock"),
    ("dock-app-new", "Dock"),
    ("exec", "Apps & Launchers"),
    ("volume", "Media & Hardware"),
    ("brightness", "Media & Hardware"),
    ("media", "Media & Hardware"),
    ("keyboard-backlight", "Media & Hardware"),
    ("touchpad", "Media & Hardware"),
    ("default-app", "Apps & Launchers"),
    ("screen-zoom", "Accessibility"),
    ("input-source", "System"),
];

/// Actions with an argument the + picker offers too (they have no default key).
const PICKABLE: &[&str] = &[
    "input-source previous",
    "input-source 1",
    "input-source 2",
    "input-source 3",
    "input-source 4",
    "input-source 5",
    "input-source 6",
    "input-source 7",
    "input-source 8",
    "input-source 9",
];

/// The argument-less actions and `PICKABLE`, for the + picker.
pub fn known() -> impl Iterator<Item = &'static str> {
    KNOWN.iter().map(|&(name, ..)| name).filter(|n| !["minimize", "cycle", "centre"].contains(n)).chain(PICKABLE.iter().copied())
}

/// Acts only with `[windows] keyboard_nudge` on.
fn needs_nudge(action: &str) -> bool {
    matches!(split(action).0, "nudge" | "resize")
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
        ("nudge", way) if !way.is_empty() => format!("Move Window {}", capital(way)),
        ("resize", "right") => "Widen Window".into(),
        ("resize", "left") => "Narrow Window".into(),
        ("resize", "down") => "Make Window Taller".into(),
        ("resize", "up") => "Make Window Shorter".into(),
        ("volume", "up-fine") => "Volume Up a Little".into(),
        ("volume", "down-fine") => "Volume Down a Little".into(),
        ("volume" | "brightness", way) if !way.is_empty() => format!("{} {}", capital(name), capital(way)),
        ("keyboard-backlight", way) if !way.is_empty() => format!("Keyboard Backlight {}", capital(way)),
        ("touchpad", way) if !way.is_empty() => format!("Trackpad {}", capital(way)),
        ("default-app", what) if !what.is_empty() => format!("Open Default App: {what}"),
        ("screen-zoom", way) if !way.is_empty() => format!("Zoom {}", capital(way)),
        ("input-source", "previous") => "Previous Input Source".into(),
        ("input-source", n) if !n.is_empty() => format!("Input Source {n}"),
        ("media", "play-pause") => "Play/Pause".into(),
        ("media", "stop") => "Stop".into(),
        ("media", "forward") => "Seek Forward".into(),
        ("media", "rewind") => "Seek Back".into(),
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

/// "Super+Ctrl+Left" as shown: ⌃⌥⇧ first, then "Super+" (as the README writes it), then the key.
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
        assert_eq!(label("nudge up"), "Move Window Up");
        assert_eq!(label("resize left"), "Narrow Window");
        assert!(needs_nudge("resize left") && !needs_nudge("tile left"));
        assert_eq!(label("volume up"), "Volume Up");
        assert_eq!(label("media previous"), "Previous Track");
        assert_eq!(label("frobnicate"), "frobnicate");
        assert_eq!(label("volume up-fine"), "Volume Up a Little");
        assert_eq!(label("media rewind"), "Seek Back");
        assert_eq!(label("screen-zoom in"), "Zoom In");
        assert_eq!(label("input-source previous"), "Previous Input Source");
        assert_eq!(label("input-source 2"), "Input Source 2");
        assert_eq!(category("input-source 2"), "System");
        assert_eq!(category("screen-zoom"), "Accessibility");
        assert_eq!(category("keyboard-backlight up"), "Media & Hardware");
        assert_eq!(category("screen-record"), "Screenshots");
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
    fn moving_keys() {
        let shipped = |c: &str| c == "Super+Q";
        let set = |list: &[(&str, &str)]| list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
        assert_eq!(moves("Super+Q", Some("Super+K"), "quit", shipped), (vec![], set(&[("bindings.Super+K", "quit"), ("bindings.Super+Q", "none")])));
        assert_eq!(moves("Super+K", Some("Super+J"), "quit", shipped), (vec!["bindings.Super+K".into()], set(&[("bindings.Super+J", "quit")])));
        assert_eq!(moves("Super+K", None, "quit", shipped), (vec!["bindings.Super+K".into()], vec![]));
        assert_eq!(moves("Super+Q", Some("Super+Q"), "quit", shipped), (vec![], set(&[("bindings.Super+Q", "quit")])));
    }

    #[test]
    fn pressed_keys() {
        use iced::keyboard::{Key, Modifiers, key::Named};
        let ch = |c: &str| Key::Character(c.into());
        assert_eq!(combo(&ch("q"), Modifiers::LOGO | Modifiers::CTRL).as_deref(), Some("Super+Ctrl+Q"));
        assert_eq!(combo(&ch("."), Modifiers::LOGO).as_deref(), Some("Super+period"));
        assert_eq!(combo(&Key::Named(Named::ArrowLeft), Modifiers::LOGO | Modifiers::SHIFT).as_deref(), Some("Super+Shift+Left"));
        assert_eq!(combo(&Key::Named(Named::F11), Modifiers::empty()).as_deref(), Some("F11"));
        assert_eq!(combo(&Key::Named(Named::Shift), Modifiers::SHIFT), None);
        assert_eq!(combo(&Key::Named(Named::Super), Modifiers::LOGO), None);
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
