//! The app index (E2.1): `.desktop` entries from the XDG data dirs, kept current with `notify`, and their icons from
//! the icon theme (WhiteSur first), rasterised on first use. The dock uses it today; Spotlight and Launchpad (M4) too.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::Duration,
};

use notify::{RecursiveMode, Watcher};
use resvg::{tiny_skia, usvg};

use crate::text::Rendered;

/// An installed application.
#[derive(Debug, Clone, PartialEq)]
pub struct App {
    /// Desktop-file id: the file name without `.desktop` (`org.kde.kate`), subdirectories joined with `-`.
    pub id: String,
    /// `Name`, localised.
    pub name: String,
    /// `Exec`, with the file format's escapes undone; field codes are dropped by `command`.
    pub exec: String,
    /// An icon-theme name or an absolute path.
    pub icon: String,
    pub wm_class: Option<String>,
    /// `NoDisplay=true`: kept out of launchers, but still matched to its windows.
    pub no_display: bool,
}

impl App {
    /// The argv `Exec` runs, with field codes expanded or dropped (we never pass files or URLs).
    pub fn command(&self) -> Vec<String> {
        let mut argv = Vec::new();
        for word in split_exec(&self.exec) {
            match word.as_str() {
                "%f" | "%F" | "%u" | "%U" | "%d" | "%D" | "%n" | "%N" | "%v" | "%m" | "%k" => {}
                "%i" if !self.icon.is_empty() => argv.extend(["--icon".to_owned(), self.icon.clone()]),
                "%i" => {}
                "%c" => argv.push(self.name.clone()),
                _ => {
                    // Inside a word, `%%` is a literal % and `%c` the name; other codes are dropped.
                    let mut out = String::new();
                    let mut chars = word.chars();
                    while let Some(c) = chars.next() {
                        if c != '%' {
                            out.push(c);
                            continue;
                        }
                        match chars.next() {
                            Some('%') => out.push('%'),
                            Some('c') => out.push_str(&self.name),
                            _ => {}
                        }
                    }
                    argv.push(out);
                }
            }
        }
        argv
    }
}

/// `Exec` split into words: blanks separate, `"…"` quotes, and inside quotes `\` escapes `"`, `` ` ``, `$` and `\`.
fn split_exec(exec: &str) -> Vec<String> {
    let (mut words, mut word, mut quoted, mut started) = (Vec::new(), String::new(), false, false);
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' if quoted => word.extend(chars.next()),
            ' ' | '\t' if !quoted => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            _ => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// A value with the desktop-entry escapes `\s \n \t \r \\` undone; other backslashes stay.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            // Not a key-file escape: kept for the Exec quoting rules (`\"` in a quoted argument).
            Some(other) => out.extend(['\\', other]),
            None => {}
        }
    }
    out
}

/// The `Name[…]` keys to try for `locale` (`de_DE.UTF-8@euro`), most specific first, as the spec orders them.
fn locale_keys(locale: &str) -> Vec<String> {
    let (rest, modifier) = locale.split_once('@').map_or((locale, None), |(r, m)| (r, Some(m)));
    let rest = rest.split('.').next().unwrap_or(rest);
    let (lang, country) = rest.split_once('_').map_or((rest, None), |(l, c)| (l, Some(c)));
    if lang.is_empty() || lang == "C" || lang == "POSIX" {
        return Vec::new();
    }
    let mut keys = Vec::new();
    if let (Some(c), Some(m)) = (country, modifier) {
        keys.push(format!("{lang}_{c}@{m}"));
    }
    if let Some(c) = country {
        keys.push(format!("{lang}_{c}"));
    }
    if let Some(m) = modifier {
        keys.push(format!("{lang}@{m}"));
    }
    keys.push(lang.to_owned());
    keys
}

/// The user's message locale, from the environment.
fn locale() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"].iter().filter_map(|k| std::env::var(k).ok()).find(|v| !v.is_empty()).unwrap_or_default()
}

/// What one file says.
#[derive(Debug, PartialEq)]
enum Entry {
    App(App),
    /// `Hidden=true`: deleted, and it hides the same id in lower-priority dirs.
    Hidden,
}

/// Parses the `[Desktop Entry]` group of a `.desktop` file. `None`: not a launchable application.
fn parse(id: &str, text: &str, locale: &str) -> Option<Entry> {
    let mut fields = HashMap::new();
    let mut in_main = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
        } else if in_main
            && !line.starts_with('#')
            && let Some((key, value)) = line.split_once('=')
        {
            fields.entry(key.trim()).or_insert(value.trim());
        }
    }
    let flag = |key: &str| fields.get(key).is_some_and(|v| *v == "true");
    if flag("Hidden") {
        return Some(Entry::Hidden);
    }
    let exec = fields.get("Exec").map(|e| unescape(e)).filter(|e| !e.is_empty())?;
    if fields.get("Type") != Some(&"Application") {
        return None;
    }
    let name = locale_keys(locale).iter().find_map(|l| fields.get(format!("Name[{l}]").as_str())).or(fields.get("Name"))?;
    Some(Entry::App(App {
        id: id.to_owned(),
        name: unescape(name),
        exec,
        icon: fields.get("Icon").map(|i| unescape(i)).unwrap_or_default(),
        wm_class: fields.get("StartupWMClass").map(|c| unescape(c)).filter(|c| !c.is_empty()),
        no_display: flag("NoDisplay"),
    }))
}

fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"))
}

/// The XDG data dirs, highest priority first.
fn data_dirs() -> Vec<PathBuf> {
    let system = std::env::var("XDG_DATA_DIRS").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    std::iter::once(data_home()).chain(system.split(':').filter(|d| !d.is_empty()).map(PathBuf::from)).collect()
}

/// Every installed app, by desktop-file id.
#[derive(Debug, Default)]
pub struct Apps {
    apps: Vec<App>,
}

impl Apps {
    /// Scans `applications/` in every data dir (~10 ms for a few hundred files: run it off the frame thread).
    pub fn load() -> Self {
        Self::scan(&data_dirs().iter().map(|d| d.join("applications")).collect::<Vec<_>>(), &locale())
    }

    /// The first file of an id wins, so `~/.local/share` overrides `/usr/share`, and a `Hidden` copy removes it.
    fn scan(dirs: &[PathBuf], locale: &str) -> Self {
        let mut seen = std::collections::HashSet::new();
        let mut apps = Vec::new();
        for dir in dirs {
            let mut files = Vec::new();
            walk(dir, &mut files);
            files.sort();
            for path in files {
                let Some(id) = path.strip_prefix(dir).ok().and_then(|p| p.to_str()).and_then(|p| p.strip_suffix(".desktop")).map(|p| p.replace('/', "-"))
                else {
                    continue;
                };
                if !seen.insert(id.clone()) {
                    continue;
                }
                if let Some(Entry::App(app)) = std::fs::read_to_string(&path).ok().and_then(|t| parse(&id, &t, locale)) {
                    apps.push(app);
                }
            }
        }
        apps.sort_by_cached_key(|a| a.name.to_lowercase());
        Self { apps }
    }

    /// Every app, by name; launchers skip `no_display` ones.
    pub fn list(&self) -> &[App] {
        &self.apps
    }

    /// The app a key names: a desktop-file id (with or without `.desktop`), a window's app id or `StartupWMClass`, the
    /// last part of a reverse-DNS id (`kate` → `org.kde.kate`), or the program `Exec` runs. Case-insensitive.
    pub fn find(&self, key: &str) -> Option<&App> {
        let key = key.strip_suffix(".desktop").unwrap_or(key);
        let is = |s: &str| s.eq_ignore_ascii_case(key);
        let program = |a: &App| split_exec(&a.exec).first().map(|p| p.rsplit('/').next().unwrap_or(p).to_owned()).unwrap_or_default();
        self.apps
            .iter()
            .find(|a| a.id == key)
            .or_else(|| self.apps.iter().find(|a| is(&a.id)))
            .or_else(|| self.apps.iter().find(|a| a.wm_class.as_deref().is_some_and(is)))
            .or_else(|| self.apps.iter().find(|a| a.id.rsplit('.').next().is_some_and(is)))
            .or_else(|| self.apps.iter().find(|a| is(&program(a))))
    }
}

/// Every `.desktop` file under `dir`.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().is_some_and(|e| e == "desktop") {
            out.push(path);
        }
    }
}

/// Loads the index on a worker thread, then reloads it whenever an `applications/` dir changes (an app installed or
/// removed), after 500 ms of quiet. Stops when `send` returns false.
pub fn watch(mut send: impl FnMut(Apps) -> bool + Send + 'static) {
    let spawned = std::thread::Builder::new().name("app-index".into()).spawn(move || {
        if !send(Apps::load()) {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let mut watcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(err) => return tracing::warn!("app index: no live updates: {err}"),
        };
        for dir in data_dirs().iter().map(|d| d.join("applications")).filter(|d| d.is_dir()) {
            if let Err(err) = watcher.watch(&dir, RecursiveMode::Recursive) {
                tracing::warn!("app index: {}: {err}", dir.display());
            }
        }
        while let Ok(event) = rx.recv() {
            if !event.is_ok_and(|e| e.kind.is_create() || e.kind.is_modify() || e.kind.is_remove()) {
                continue;
            }
            while rx.recv_timeout(Duration::from_millis(500)).is_ok() {}
            if !send(Apps::load()) {
                return;
            }
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("app index: {err}");
    }
}

/// Themes searched for app icons, in order.
const THEMES: [&str; 4] = ["WhiteSur", "hicolor", "Adwaita", "breeze"];

/// The file of icon `name`: in the first theme that has it, its scalable version, else its largest bitmap; then
/// `/usr/share/pixmaps`. Symbolic (monochrome) variants are skipped.
// ponytail: follows the fixed THEMES list, not each theme's `Inherits`; read index.theme chains if a theme needs it.
pub fn find_icon(name: &str) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    if Path::new(name).is_absolute() {
        return Some(PathBuf::from(name)).filter(|p| p.is_file());
    }
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let bases: Vec<PathBuf> =
        std::iter::once(data_home().join("icons")).chain([home.join(".icons")]).chain(data_dirs().iter().skip(1).map(|d| d.join("icons"))).collect();
    let files = |dir: &Path| [dir.join(format!("{name}.svg")), dir.join(format!("{name}.png"))];
    for theme in THEMES {
        for base in &bases {
            let root = base.join(theme);
            let Ok(index) = std::fs::read_to_string(root.join("index.theme")) else { continue };
            let mut dirs: Vec<(u32, &str)> = index
                .lines()
                .find_map(|l| l.strip_prefix("Directories="))
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|d| !d.is_empty() && !d.contains("symbolic"))
                .map(|d| (if d.contains("scalable") { u32::MAX } else { dir_size(d) }, d))
                .collect();
            dirs.sort_by_key(|(size, _)| std::cmp::Reverse(*size));
            if let Some(found) = dirs.iter().flat_map(|(_, d)| files(&root.join(d))).find(|p| p.is_file()) {
                return Some(found);
            }
        }
    }
    files(Path::new("/usr/share/pixmaps")).into_iter().find(|p| p.is_file())
}

/// The pixel size an icon-theme dir holds: its largest number (`48x48/apps` → 48, `apps@2x/32` → 32).
fn dir_size(dir: &str) -> u32 {
    dir.split(|c: char| !c.is_ascii_digit()).filter_map(|n| n.parse().ok()).max().unwrap_or(0)
}

/// App icons by name and pixel size, rasterised on first use.
// ponytail: rasterised on the frame thread (~2–10 ms per SVG, once per icon and size) and never evicted; move to a
// worker with a disk cache (E1.11) if first frames stutter or sizes multiply.
#[derive(Default)]
pub struct Icons {
    cache: HashMap<(String, u32), Option<Arc<Rendered>>>,
}

impl Icons {
    /// Icon `name` (an `App::icon`) for a `px`-sized square: SVGs are drawn at `px`, PNGs come at their own size for
    /// the GPU to scale. `None`: the theme has no such icon.
    pub fn get(&mut self, name: &str, px: u32) -> Option<Arc<Rendered>> {
        self.cache.entry((name.to_owned(), px)).or_insert_with(|| find_icon(name).and_then(|p| rasterise(&p, px)).map(Arc::new)).clone()
    }
}

/// Premultiplied RGBA of the icon file at `path`.
pub fn rasterise(path: &Path, px: u32) -> Option<Rendered> {
    let bytes = std::fs::read(path).ok()?;
    let rendered = |pixels, width, height| Rendered { pixels, width, height, baseline: height, truncated: false };
    if bytes.starts_with(b"\x89PNG") {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(&bytes));
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = decoder.read_info().ok()?;
        let mut pixels = vec![0; reader.output_buffer_size()?];
        let info = reader.next_frame(&mut pixels).ok()?;
        pixels.truncate(info.buffer_size());
        let rgba: Vec<u8> = match info.color_type {
            png::ColorType::Grayscale => pixels.iter().flat_map(|&v| [v, v, v, 255]).collect(),
            png::ColorType::GrayscaleAlpha => pixels.as_chunks::<2>().0.iter().flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
            png::ColorType::Rgb => pixels.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
            png::ColorType::Rgba => pixels,
            png::ColorType::Indexed => return None,
        };
        let premultiplied =
            rgba.as_chunks::<4>().0.iter().flat_map(|p| [0, 1, 2].map(|i| (u16::from(p[i]) * u16::from(p[3]) / 255) as u8).into_iter().chain([p[3]])).collect();
        return Some(rendered(premultiplied, info.width, info.height));
    }
    let tree = usvg::Tree::from_data(&bytes, &usvg::Options { resources_dir: path.parent().map(Path::to_owned), ..Default::default() }).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(px, px)?;
    let size = tree.size();
    let fit = px as f32 / size.width().max(size.height());
    let (dx, dy) = ((px as f32 - size.width() * fit) / 2.0, (px as f32 - size.height() * fit) / 2.0);
    resvg::render(&tree, tiny_skia::Transform::from_scale(fit, fit).post_translate(dx, dy), &mut pixmap.as_mut());
    Some(rendered(pixmap.take(), px, px))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(text: &str) -> Option<Entry> {
        parse("test", text, "de_DE.UTF-8")
    }

    fn exec(exec: &str) -> Vec<String> {
        App { id: "kate".into(), name: "Kate".into(), exec: unescape(exec), icon: "kate".into(), wm_class: None, no_display: false }.command()
    }

    #[test]
    fn exec_field_codes_and_quoting() {
        assert_eq!(exec("/usr/bin/google-chrome-stable %U"), ["/usr/bin/google-chrome-stable"]);
        assert_eq!(exec("kate -b %F"), ["kate", "-b"]);
        assert_eq!(exec("app %i --name=%c 100%%"), ["app", "--icon", "kate", "--name=Kate", "100%"]);
        assert_eq!(exec("app %c"), ["app", "Kate"]);
        assert_eq!(exec(r#""/opt/My App/run" --flag "a \"b\"" x"#), ["/opt/My App/run", "--flag", "a \"b\"", "x"]);
        assert_eq!(exec(r#"sh -c "echo \\$HOME""#), ["sh", "-c", "echo $HOME"]);
        assert_eq!(exec(r"a\sb  c"), ["a", "b", "c"]);
        assert_eq!(exec(r#"run """#), ["run", ""]);
    }

    #[test]
    fn hidden_nodisplay_and_type() {
        let base = "[Desktop Entry]\nType=Application\nName=Kate\nExec=kate %U\nIcon=kate\nStartupWMClass=kate\n";
        let Some(Entry::App(kate)) = app(base) else { panic!("not parsed") };
        assert_eq!((kate.name.as_str(), kate.icon.as_str(), kate.wm_class.as_deref(), kate.no_display), ("Kate", "kate", Some("kate"), false));
        assert!(matches!(app(&format!("{base}NoDisplay=true\n")), Some(Entry::App(App { no_display: true, .. }))));
        assert_eq!(app(&format!("{base}Hidden=true\n")), Some(Entry::Hidden));
        assert_eq!(app("[Desktop Entry]\nType=Link\nName=x\nExec=x\n"), None);
        assert_eq!(app("[Desktop Entry]\nType=Application\nName=x\n"), None, "no Exec");
        // Keys of other groups (actions) never leak into the main entry.
        let Some(Entry::App(a)) = app("[Desktop Entry]\nType=Application\nName=A\nExec=a\n[Desktop Action new]\nName=New\nExec=a --new\n") else { panic!() };
        assert_eq!((a.name.as_str(), a.exec.as_str()), ("A", "a"));
    }

    #[test]
    fn localised_names_fall_back() {
        assert_eq!(locale_keys("de_DE.UTF-8@euro"), ["de_DE@euro", "de_DE", "de@euro", "de"]);
        assert_eq!(locale_keys("C.UTF-8"), Vec::<String>::new());
        let name = |extra: &str| match app(&format!("[Desktop Entry]\nType=Application\nExec=x\nName=Files\n{extra}")) {
            Some(Entry::App(a)) => a.name,
            other => panic!("{other:?}"),
        };
        assert_eq!(name("Name[de]=Dateien\nName[fr]=Fichiers\n"), "Dateien");
        assert_eq!(name("Name[de_DE]=Dateien (DE)\nName[de]=Dateien\n"), "Dateien (DE)");
        assert_eq!(name("Name[fr]=Fichiers\n"), "Files");
    }

    #[test]
    fn scan_overrides_hides_and_finds() {
        let root = std::env::temp_dir().join(format!("noobsbot-apps-{}", std::process::id()));
        let (user, system) = (root.join("user"), root.join("system"));
        let write = |dir: &Path, file: &str, text: &str| {
            std::fs::create_dir_all(dir.join(file).parent().unwrap()).unwrap();
            std::fs::write(dir.join(file), text).unwrap();
        };
        let entry = |name: &str, exec: &str, extra: &str| format!("[Desktop Entry]\nType=Application\nName={name}\nExec={exec}\n{extra}");
        write(&system, "org.kde.kate.desktop", &entry("Kate", "kate %U", ""));
        write(&system, "gone.desktop", &entry("Gone", "gone", ""));
        write(&user, "gone.desktop", &entry("Gone", "gone", "Hidden=true\n"));
        write(&user, "org.kde.dolphin.desktop", &entry("My Dolphin", "dolphin", ""));
        write(&system, "org.kde.dolphin.desktop", &entry("Dolphin", "dolphin", ""));
        write(&system, "kde/term.desktop", &entry("Term", "/usr/bin/noobsbot", "StartupWMClass=Noobsbot\n"));
        let apps = Apps::scan(&[user, system], "");
        let ids: Vec<&str> = apps.list().iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["org.kde.kate", "org.kde.dolphin", "kde-term"], "sorted by name, gone hidden");
        assert_eq!(apps.find("org.kde.dolphin").map(|a| a.name.as_str()), Some("My Dolphin"));
        assert_eq!(apps.find("org.kde.kate.desktop").map(|a| a.id.as_str()), Some("org.kde.kate"));
        assert_eq!(apps.find("kate").map(|a| a.id.as_str()), Some("org.kde.kate"));
        assert_eq!(apps.find("noobsbot").map(|a| a.id.as_str()), Some("kde-term"), "by StartupWMClass, any case");
        assert_eq!(apps.find("gone"), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn icon_dirs_sort_by_size() {
        assert_eq!(dir_size("48x48/apps"), 48);
        assert_eq!(dir_size("apps@2x/32"), 32);
        assert_eq!(dir_size("apps/scalable"), 0);
    }

    #[test]
    fn rasterises_svg_to_size() {
        let dir = std::env::temp_dir().join(format!("noobsbot-icon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("i.svg");
        std::fs::write(&path, r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="8"><rect width="16" height="8" fill="red"/></svg>"#).unwrap();
        let r = rasterise(&path, 32).unwrap();
        assert_eq!((r.width, r.height), (32, 32));
        // Centred: the wide image fills the middle rows only.
        assert_eq!(r.pixels[(16 * 32 + 16) * 4..][..4], [255, 0, 0, 255]);
        assert_eq!(r.pixels[3], 0);
        let mut icons = Icons::default();
        assert!(icons.get(path.to_str().unwrap(), 32).is_some());
        assert!(icons.get("no-such-icon-anywhere", 32).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
