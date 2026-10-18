//! `noobsbot-desktop dock --import-plank` (E3.1): a one-time move from Plank. Reads Plank's dconf settings
//! (`dconf dump /net/launchpad/plank/`) and its `.dockitem` launchers, and writes the first enabled dock's pins, icon
//! size, zoom and hide mode into the `[dock]` table of config.toml, keeping the rest of the file and its comments.

use std::{collections::HashMap, fs, path::Path, process::Command};

use crate::config;

const USAGE: &str = "usage: noobsbot-desktop dock --import-plank";

/// dconf's text dump: section → key → the value as GVariant text.
type Dump = HashMap<String, HashMap<String, String>>;

fn parse_dump(text: &str) -> Dump {
    let mut dump = Dump::new();
    let mut section = String::new();
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.to_owned();
        } else if let Some((key, value)) = line.split_once('=') {
            dump.entry(section.clone()).or_default().insert(key.trim().to_owned(), value.trim().to_owned());
        }
    }
    dump
}

/// The single-quoted strings in a GVariant value: `['a', 'b']` → a, b; `'center'` → center.
fn strings(value: &str) -> Vec<String> {
    let (mut out, mut current, mut quoted, mut escaped) = (Vec::new(), String::new(), false, false);
    for c in value.chars() {
        match (quoted, escaped, c) {
            (true, false, '\\') => escaped = true,
            (true, false, '\'') => {
                out.push(std::mem::take(&mut current));
                quoted = false;
            }
            (true, ..) => {
                current.push(c);
                escaped = false;
            }
            (false, _, '\'') => quoted = true,
            _ => {}
        }
    }
    out
}

/// A `.dockitem`'s `Launcher=file:///…/kitty.desktop` → `kitty`; docklets (`docklet://clock`) have no app.
fn launcher_id(dockitem: &str) -> Option<String> {
    let uri = dockitem.lines().find_map(|l| l.trim().strip_prefix("Launcher="))?.trim();
    let path = uri.strip_prefix("file://")?;
    let name = path.rsplit('/').next()?;
    let name = name.strip_suffix(".desktop").unwrap_or(name);
    // Percent-escapes, as in `My%20App.desktop`.
    let bytes = name.as_bytes();
    let (mut out, mut i) = (Vec::new(), 0);
    while i < bytes.len() {
        match (bytes[i], name.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok().filter(|n| !n.is_empty())
}

/// The dock imported: Plank's first enabled one.
// ponytail: one dock; Plank's other docks (dock2 on HDMI-1) come with one dock per monitor (M6 multi-monitor).
fn first_dock(dump: &Dump) -> String {
    dump.get("/").and_then(|s| s.get("enabled-docks")).and_then(|v| strings(v).into_iter().next()).unwrap_or_else(|| "dock1".into())
}

/// The `[dock]` keys for Plank's settings: `(key, TOML value)`. `launchers` maps a `.dockitem` file name to its text.
fn dock_keys(dump: &Dump, launchers: &HashMap<String, String>) -> Vec<(&'static str, String)> {
    let settings = dump.get(&format!("docks/{}", first_dock(dump)));
    let get = |key: &str| settings.and_then(|s| s.get(key)).map(String::as_str);
    // Plank's order; without dconf, the launcher files by name.
    let order = get("dock-items").map(strings).unwrap_or_else(|| {
        let mut names: Vec<String> = launchers.keys().cloned().collect();
        names.sort();
        names
    });
    let items: Vec<String> = order.iter().filter_map(|n| launchers.get(n)).filter_map(|t| launcher_id(t)).map(|id| format!("{id:?}")).collect();
    let mut keys = vec![("items", format!("[{}]", items.join(", ")))];
    if let Some(size) = get("icon-size").and_then(|v| v.parse::<u16>().ok()) {
        keys.push(("icon_size", size.to_string()));
    }
    if let Some(zoom) = get("zoom-enabled").filter(|v| matches!(*v, "true" | "false")) {
        keys.push(("zoom", zoom.to_owned()));
    }
    if let Some(percent) = get("zoom-percent").and_then(|v| v.parse::<u16>().ok()) {
        keys.push(("zoom_percent", percent.to_string()));
    }
    // ponytail: Plank's autohide and dodge modes all become intelligent hide, the only hiding mode here yet.
    if let Some(mode) = get("hide-mode").and_then(|v| strings(v).into_iter().next()) {
        keys.push(("hide", if mode == "none" { "\"never\"".into() } else { "\"intelligent\"".into() }));
    }
    keys
}

/// `text` (a config.toml) with each key of `table` set, in place where it already is (a multi-line value replaced
/// whole), else added at the end of the table; the table is added when missing. Comments and the rest are kept.
fn set_keys(text: &str, table: &str, keys: &[(&str, String)]) -> String {
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let header = format!("[{table}]");
    let start = match lines.iter().position(|l| l.trim() == header) {
        Some(i) => i,
        None => {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(header);
            lines.len() - 1
        }
    };
    for (key, value) in keys {
        let end = lines[start + 1..].iter().position(|l| l.trim_start().starts_with('[')).map_or(lines.len(), |i| start + 1 + i);
        let is_key = |l: &String| !l.trim_start().starts_with('#') && l.split_once('=').is_some_and(|(k, _)| k.trim() == *key);
        let line = format!("{key} = {value}");
        match lines[start + 1..end].iter().position(is_key).map(|i| start + 1 + i) {
            Some(i) => {
                // An array spread over several lines ends where its brackets balance.
                let mut depth = 0i32;
                let mut last = i;
                for (j, l) in lines.iter().enumerate().skip(i) {
                    depth += l.matches('[').count() as i32 - l.matches(']').count() as i32;
                    last = j;
                    if depth <= 0 {
                        break;
                    }
                }
                lines.splice(i..=last, [line]);
            }
            None => {
                let at = lines[start..end].iter().rposition(|l| !l.trim().is_empty()).map_or(end, |i| start + i + 1);
                lines.insert(at, line);
            }
        }
    }
    lines.join("\n") + "\n"
}

/// Every `.dockitem` in `dir`, by file name.
fn read_launchers(dir: &Path) -> HashMap<String, String> {
    let Ok(entries) = fs::read_dir(dir) else { return HashMap::new() };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok().filter(|n| n.ends_with(".dockitem"))?;
            Some((name, fs::read_to_string(e.path()).ok()?))
        })
        .collect()
}

/// `noobsbot-desktop dock …`: the exit code.
pub fn cli(args: impl Iterator<Item = String>) -> i32 {
    let args: Vec<String> = args.collect();
    if args != ["--import-plank"] {
        eprintln!("{USAGE}");
        return if args.iter().any(|a| a == "-h" || a == "--help") { 0 } else { 2 };
    }
    // A missing dconf or an empty dump leaves only the launcher files.
    let dump = Command::new("dconf").args(["dump", "/net/launchpad/plank/"]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let dump = parse_dump(&dump);
    let dock = first_dock(&dump);
    let launchers = read_launchers(&config::xdg_dir("XDG_CONFIG_HOME", ".config").join("plank").join(&dock).join("launchers"));
    if launchers.is_empty() && !dump.contains_key(&format!("docks/{dock}")) {
        eprintln!("dock: no Plank settings found (dconf /net/launchpad/plank/, ~/.config/plank/{dock}/launchers)");
        return 1;
    }
    let path = config::path();
    let text = fs::read_to_string(&path).unwrap_or_else(|_| config::DEFAULT_TOML.to_owned());
    let keys = dock_keys(&dump, &launchers);
    let new = set_keys(&text, "dock", &keys);
    if let Err(err) = config::parse(&new) {
        eprintln!("dock: the imported settings don't parse, nothing written: {err}");
        return 1;
    }
    match config::save_atomic(&path, &new) {
        Ok(()) => {
            keys.iter().for_each(|(k, v)| println!("{k} = {v}"));
            println!("wrote {} from Plank's {dock}", path.display());
            0
        }
        Err(err) => {
            eprintln!("dock: writing {}: {err}", path.display());
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = "[/]\nenabled-docks=['dock1']\n\n[docks/dock1]\nalignment='center'\ndock-items=['kitty.dockitem', 'clock.dockitem', 'org.kde.kate.dockitem', 'gone.dockitem']\nhide-mode='intelligent'\nicon-size=84\nzoom-enabled=true\nzoom-percent=150\n\n[docks/dock2]\nicon-size=48\n";

    fn launchers() -> HashMap<String, String> {
        [
            ("kitty.dockitem", "[PlankDockItemPreferences]\nLauncher=file:///home/devil/Desktop/kitty.desktop\n"),
            ("clock.dockitem", "[PlankDockItemPreferences]\nLauncher=docklet://clock\n"),
            ("org.kde.kate.dockitem", "[PlankDockItemPreferences]\nLauncher=file:///usr/share/applications/org.kde.kate.desktop\n"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
    }

    #[test]
    fn dconf_dumps_and_gvariant_strings() {
        let dump = parse_dump(DUMP);
        assert_eq!(dump["docks/dock1"]["icon-size"], "84");
        assert_eq!(dump["docks/dock2"]["icon-size"], "48");
        assert_eq!(strings("['a.dockitem', 'b c']"), ["a.dockitem", "b c"]);
        assert_eq!(strings(r"'it\'s'"), ["it's"]);
        assert!(strings("[]").is_empty());
    }

    #[test]
    fn dockitems_name_their_desktop_file() {
        assert_eq!(launcher_id("[PlankDockItemPreferences]\nLauncher=file:///usr/share/applications/org.kde.kate.desktop").as_deref(), Some("org.kde.kate"));
        assert_eq!(launcher_id("Launcher=file:///home/u/My%20App.desktop").as_deref(), Some("My App"));
        assert_eq!(launcher_id("Launcher=docklet://clock"), None);
        assert_eq!(launcher_id("[x]\nnothing=1"), None);
    }

    #[test]
    fn plank_settings_become_dock_keys() {
        let keys = dock_keys(&parse_dump(DUMP), &launchers());
        let expected: Vec<(&str, String)> = vec![
            ("items", r#"["kitty", "org.kde.kate"]"#.into()),
            ("icon_size", "84".into()),
            ("zoom", "true".into()),
            ("zoom_percent", "150".into()),
            ("hide", r#""intelligent""#.into()),
        ];
        assert_eq!(keys, expected);
        // Without dconf: the launcher files by name, nothing else.
        let mut more = launchers();
        more.insert("brave-browser.dockitem".into(), "Launcher=file:///usr/share/applications/brave-browser.desktop".into());
        let keys = dock_keys(&Dump::new(), &more);
        assert_eq!(keys, [("items", r#"["brave-browser", "kitty", "org.kde.kate"]"#.to_owned())]);
    }

    #[test]
    fn keys_are_set_in_place_keeping_the_rest() {
        let text = "[appearance]\nmode = \"dark\"\n\n[dock]\n# Pinned apps.\nitems = [\n  \"a\",\n  \"b\",\n]\nicon_size = 64\n\n[bindings]\nx = \"y\"\n";
        let keys = [("items", r#"["kitty"]"#.to_owned()), ("icon_size", "84".into()), ("zoom", "true".into())];
        let out = set_keys(text, "dock", &keys);
        assert_eq!(out, "[appearance]\nmode = \"dark\"\n\n[dock]\n# Pinned apps.\nitems = [\"kitty\"]\nicon_size = 84\nzoom = true\n\n[bindings]\nx = \"y\"\n");
        // No [dock] table: added at the end.
        assert_eq!(set_keys("version = 1\n", "dock", &keys[1..2]), "version = 1\n\n[dock]\nicon_size = 84\n");
        // The shipped defaults take it and still parse to the imported values.
        let out = set_keys(config::DEFAULT_TOML, "dock", &dock_keys(&parse_dump(DUMP), &launchers()));
        let parsed = config::parse(&out).expect("parses");
        assert_eq!(parsed.dock.items, ["kitty", "org.kde.kate"]);
        assert_eq!(out.matches("[dock]").count(), 1);
        assert!(out.contains("# Pinned apps, left to right"), "comments kept");
    }
}
