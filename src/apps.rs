//! The app index (E2.1): `.desktop` entries from the XDG data dirs, kept current with `notify`, and their icons from
//! the icon theme (WhiteSur first), rasterised on first use. The dock uses it today; Spotlight and Launchpad (M4) too.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::Duration,
};

use notify::{RecursiveMode, Watcher};
use resvg::{tiny_skia, usvg};
use smithay::reexports::calloop::channel::Sender;

use crate::{
    services::tray::{self, Image},
    text::{Lru, Rendered},
};

/// An installed application.
#[derive(Debug, Clone, Default, PartialEq)]
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
    /// `Comment`, else `GenericName`, localised: the app drawer's second line.
    pub comment: String,
    /// `GenericName` ("Web Browser"), localised.
    pub generic_name: String,
    /// `Keywords`, localised, for search.
    pub keywords: Vec<String>,
    /// `Categories` (freedesktop: `Development`, `AudioVideo`, …).
    pub categories: Vec<String>,
    /// `Terminal=true`: runs inside a terminal.
    pub terminal: bool,
    /// `Path`: the directory to run in.
    pub working_dir: Option<PathBuf>,
    /// The `.desktop` file it came from (empty when built in code).
    pub path: PathBuf,
}

impl App {
    /// The argv `Exec` runs, with field codes expanded or dropped; `terminal` is the configured one, for `Terminal=true`.
    pub fn command(&self, terminal: &str) -> Vec<String> {
        self.command_with(&[], terminal)
    }

    /// The file name of the program `Exec` runs (`kate`).
    pub fn program(&self) -> String {
        split_exec(&self.exec).first().map(|p| p.rsplit('/').next().unwrap_or(p).to_owned()).unwrap_or_default()
    }

    /// The argv that opens `uris` (dropped on the dock): `%u`/`%U` get the URIs, `%f`/`%F` their local paths.
    pub fn command_with(&self, uris: &[String], terminal: &str) -> Vec<String> {
        let argv = self.exec_argv(uris);
        if self.terminal { in_terminal(argv, installed("xdg-terminal-exec"), terminal) } else { argv }
    }

    fn exec_argv(&self, uris: &[String]) -> Vec<String> {
        let paths: Vec<String> = uris.iter().filter_map(|u| file_path(u)).collect();
        // ponytail: `Path=` through coreutils' env, since the launcher takes an argv; a cwd on `spawn` if env goes away.
        let mut argv: Vec<String> = self.working_dir.iter().flat_map(|d| ["env".to_owned(), format!("--chdir={}", d.display())]).collect();
        for word in split_exec(&self.exec) {
            match word.as_str() {
                "%u" => argv.extend(uris.first().cloned()),
                "%U" => argv.extend(uris.iter().cloned()),
                "%f" => argv.extend(paths.first().cloned()),
                "%F" => argv.extend(paths.iter().cloned()),
                "%d" | "%D" | "%n" | "%N" | "%v" | "%m" | "%k" => {}
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

/// `argv` run inside a terminal: `xdg-terminal-exec` when installed (it knows each terminal's flag), else `terminal -e`.
fn in_terminal(argv: Vec<String>, xdg_terminal_exec: bool, terminal: &str) -> Vec<String> {
    let mut wrapped =
        if xdg_terminal_exec { vec!["xdg-terminal-exec".to_owned()] } else { split_exec(terminal).into_iter().chain(["-e".to_owned()]).collect() };
    wrapped.extend(argv);
    wrapped
}

/// The local path of a `file://` URI, percent-decoded.
pub fn file_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    // Skip a host part (`file://localhost/…`).
    let rest = &rest[rest.find('/')?..];
    let (bytes, mut out) = (rest.as_bytes(), Vec::new());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%').then(|| rest.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())).flatten();
        out.push(hex.unwrap_or(bytes[i]));
        i += if hex.is_some() { 3 } else { 1 };
    }
    String::from_utf8(out).ok()
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
#[expect(clippy::large_enum_variant, reason = "short-lived: one per file while scanning")]
enum Entry {
    App(App),
    /// `Hidden=true`: deleted, and it hides the same id in lower-priority dirs.
    Hidden,
}

/// Parses the `[Desktop Entry]` group of a `.desktop` file. `None`: not a launchable application (or its `TryExec`
/// isn't installed). `desktop` is `$XDG_CURRENT_DESKTOP`, for `OnlyShowIn`/`NotShowIn`.
fn parse(id: &str, text: &str, locale: &str, desktop: &str) -> Option<Entry> {
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
    if fields.get("Type") != Some(&"Application") || fields.get("TryExec").is_some_and(|p| !installed(&unescape(p))) {
        return None;
    }
    let locales = locale_keys(locale);
    let localised = |key: &str| locales.iter().find_map(|l| fields.get(format!("{key}[{l}]").as_str())).or(fields.get(key)).map(|v| unescape(v));
    // `;`-separated lists, the last `;` optional.
    let list = |value: Option<String>| value.unwrap_or_default().split(';').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
    let generic_name = localised("GenericName").unwrap_or_default();
    // Both the translated and the plain keywords: a German user still types "editor".
    let mut keywords: Vec<String> = list(localised("Keywords"));
    for k in list(fields.get("Keywords").map(|k| unescape(k))) {
        if !keywords.contains(&k) {
            keywords.push(k);
        }
    }
    // Meant for another desktop: kept out of launchers, like `NoDisplay`, but still matched to its windows.
    let shows_in = |key: &str| fields.get(key).map(|v| v.split(';').any(|d| !d.is_empty() && desktop.split(':').any(|c| c == d)));
    let elsewhere = shows_in("OnlyShowIn") == Some(false) || shows_in("NotShowIn") == Some(true);
    Some(Entry::App(App {
        id: id.to_owned(),
        name: localised("Name")?,
        exec,
        icon: fields.get("Icon").map(|i| unescape(i)).unwrap_or_default(),
        wm_class: fields.get("StartupWMClass").map(|c| unescape(c)).filter(|c| !c.is_empty()),
        no_display: flag("NoDisplay") || elsewhere,
        comment: localised("Comment").filter(|c| !c.is_empty()).unwrap_or_else(|| generic_name.clone()),
        generic_name,
        keywords,
        categories: list(fields.get("Categories").map(|c| unescape(c))),
        terminal: flag("Terminal"),
        working_dir: fields.get("Path").map(|p| unescape(p)).filter(|p| !p.is_empty()).map(PathBuf::from),
        path: PathBuf::new(),
    }))
}

/// Whether `program` (a path, or a name looked up on `$PATH`) is an executable file.
fn installed(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let runs = |p: PathBuf| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    if program.contains('/') {
        return runs(PathBuf::from(program));
    }
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| runs(dir.join(program))))
}

/// The desktop actions (`Actions=` and their `[Desktop Action …]` groups) of the app with desktop-file id `id`:
/// (name, Exec). Read from its file when asked, for the dock menu.
pub fn actions(id: &str) -> Vec<(String, String)> {
    let file = data_dirs().into_iter().map(|d| d.join("applications").join(format!("{id}.desktop"))).find(|p| p.is_file());
    file.and_then(|f| std::fs::read_to_string(f).ok()).map_or_else(Vec::new, |text| parse_actions(&text, &locale()))
}

fn parse_actions(text: &str, locale: &str) -> Vec<(String, String)> {
    let mut groups: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut group = String::new();
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            group = name.to_owned();
        } else if !line.starts_with('#')
            && let Some((key, value)) = line.split_once('=')
        {
            groups.entry(group.clone()).or_default().entry(key.trim().to_owned()).or_insert_with(|| value.trim().to_owned());
        }
    }
    let listed = groups.get("Desktop Entry").and_then(|g| g.get("Actions")).cloned().unwrap_or_default();
    listed
        .split(';')
        .filter(|a| !a.is_empty())
        .filter_map(|a| {
            let g = groups.get(&format!("Desktop Action {a}"))?;
            let name = locale_keys(locale).iter().find_map(|l| g.get(&format!("Name[{l}]"))).or(g.get("Name"))?;
            Some((unescape(name), unescape(g.get("Exec")?)))
        })
        .collect()
}

fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"))
}

/// The XDG data dirs, highest priority first.
pub fn data_dirs() -> Vec<PathBuf> {
    let system = std::env::var("XDG_DATA_DIRS").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    std::iter::once(data_home()).chain(system.split(':').filter(|d| !d.is_empty()).map(PathBuf::from)).collect()
}

/// Every installed app, by desktop-file id.
#[derive(Debug, Default)]
pub struct Apps {
    apps: Vec<App>,
    /// Lower-cased lookup keys → (how specific the key is, index into `apps`); built once so `find` is one hash probe.
    index: HashMap<String, (u8, usize)>,
}

impl Apps {
    /// Scans `applications/` in every data dir (~10 ms for a few hundred files: run it off the frame thread).
    pub fn load() -> Self {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        Self::scan(&data_dirs().iter().map(|d| d.join("applications")).collect::<Vec<_>>(), &locale(), &desktop)
    }

    /// The first file of an id wins, so `~/.local/share` overrides `/usr/share`, and a `Hidden` copy removes it.
    fn scan(dirs: &[PathBuf], locale: &str, desktop: &str) -> Self {
        let mut seen = std::collections::HashSet::new();
        let mut apps = Vec::new();
        for dir in dirs {
            let mut files = Vec::new();
            walk(dir, &mut files, 0);
            files.sort();
            for path in files {
                let Some(id) = path.strip_prefix(dir).ok().and_then(|p| p.to_str()).and_then(|p| p.strip_suffix(".desktop")).map(|p| p.replace('/', "-"))
                else {
                    continue;
                };
                if !seen.insert(id.clone()) {
                    continue;
                }
                if let Some(Entry::App(app)) = std::fs::read_to_string(&path).ok().and_then(|t| parse(&id, &t, locale, desktop)) {
                    apps.push(App { path, ..app });
                }
            }
        }
        apps.sort_by_cached_key(|a| a.name.to_lowercase());
        let mut index = HashMap::new();
        for (i, a) in apps.iter().enumerate() {
            let program = split_exec(&a.exec).first().map(|p| p.rsplit('/').next().unwrap_or(p).to_owned()).unwrap_or_default();
            let keys = [Some(a.id.as_str()), a.wm_class.as_deref(), a.id.rsplit('.').next(), Some(program.as_str())];
            for (tier, key) in keys.into_iter().enumerate() {
                let Some(key) = key.filter(|k| !k.is_empty()) else { continue };
                let entry = index.entry(key.to_ascii_lowercase()).or_insert((tier as u8, i));
                if tier < usize::from(entry.0) {
                    *entry = (tier as u8, i);
                }
            }
        }
        Self { apps, index }
    }

    /// Every app, by name; launchers skip `no_display` ones.
    pub fn list(&self) -> &[App] {
        &self.apps
    }

    /// The app a key names: a desktop-file id (with or without `.desktop`), a window's app id or `StartupWMClass`, the
    /// last part of a reverse-DNS id either way (`kate` → `org.kde.kate`, `org.example.Files` → `files`), or the
    /// program `Exec` runs. Case-insensitive.
    pub fn find(&self, key: &str) -> Option<&App> {
        let key = key.strip_suffix(".desktop").unwrap_or(key).to_ascii_lowercase();
        // A reverse-DNS key for a plain file (`org.example.Files` → `files`): the key's last part, second.
        let hit = self.index.get(&key).or_else(|| self.index.get(key.rsplit('.').next()?))?;
        Some(&self.apps[hit.1])
    }
}

/// Every `.desktop` file under `dir`, at most `MAX_DEPTH` levels down. Symlinked dirs aren't followed (`file_type`
/// doesn't): a link loop would otherwise recurse until the stack is gone.
fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: u8) {
    const MAX_DEPTH: u8 = 8;
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            if depth < MAX_DEPTH {
                walk(&path, out, depth + 1);
            }
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
        // The user's dir may not exist yet; watching only existing ones would miss the first app installed there.
        let _ = std::fs::create_dir_all(data_home().join("applications"));
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

/// Every file of icon `name` in the first theme that has it, as (size, path): a dir's pixel size for a PNG,
/// `u32::MAX` for an SVG; then `/usr/share/pixmaps`. Symbolic (monochrome) variants are skipped.
// ponytail: follows the fixed THEMES list, not each theme's `Inherits`; read index.theme chains if a theme needs it.
pub fn icon_files(name: &str) -> Vec<(u32, PathBuf)> {
    if name.is_empty() {
        return Vec::new();
    }
    if Path::new(name).is_absolute() {
        return Some(PathBuf::from(name)).filter(|p| p.is_file()).map(|p| (u32::MAX, p)).into_iter().collect();
    }
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let bases: Vec<PathBuf> =
        std::iter::once(data_home().join("icons")).chain([home.join(".icons")]).chain(data_dirs().iter().skip(1).map(|d| d.join("icons"))).collect();
    let found = theme_icon_files(name, &bases);
    if !found.is_empty() {
        return found;
    }
    icon_dir_files(name, Path::new("/usr/share/pixmaps"), 0).into_iter().filter(|(_, p)| p.is_file()).collect()
}

/// `name.svg` and `name.png` in `dir`, as (size, path).
fn icon_dir_files(name: &str, dir: &Path, size: u32) -> [(u32, PathBuf); 2] {
    [(u32::MAX, dir.join(format!("{name}.svg"))), (size, dir.join(format!("{name}.png")))]
}

/// [`icon_files`]' theme search over the icon `bases`, highest priority first. A theme's `index.theme` can sit in
/// any one base and lists the dirs for all of them: programs installed to /usr/local put their icons in
/// /usr/local/share/icons/hicolor, which has none of its own.
fn theme_icon_files(name: &str, bases: &[PathBuf]) -> Vec<(u32, PathBuf)> {
    for theme in THEMES {
        let Some(index) = bases.iter().find_map(|base| std::fs::read_to_string(base.join(theme).join("index.theme")).ok()) else { continue };
        let dirs: Vec<&str> = index
            .lines()
            .find_map(|l| l.strip_prefix("Directories="))
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|d| !d.is_empty() && !d.contains("symbolic"))
            .collect();
        for base in bases {
            let root = base.join(theme);
            let found: Vec<(u32, PathBuf)> = dirs.iter().flat_map(|d| icon_dir_files(name, &root.join(d), dir_size(d))).filter(|(_, p)| p.is_file()).collect();
            if !found.is_empty() {
                return found;
            }
        }
    }
    Vec::new()
}

/// The file to draw a `px`-sized icon from: a bitmap of exactly `px`, else an SVG, else the smallest bigger bitmap
/// (`rasterise` shrinks it), else the largest there is.
pub fn pick(files: &[(u32, PathBuf)], px: u32) -> Option<&Path> {
    let best = files.iter().filter(|(size, _)| *size >= px).min_by_key(|(size, _)| (*size != px, *size != u32::MAX, *size));
    best.or_else(|| files.iter().max_by_key(|(size, _)| *size)).map(|(_, p)| p.as_path())
}

/// The file of icon `name`: its SVG, else its largest bitmap.
pub fn find_icon(name: &str) -> Option<PathBuf> {
    pick(&icon_files(name), u32::MAX).map(Path::to_owned)
}

/// The pixel size an icon-theme dir holds: its largest number (`48x48/apps` → 48, `apps@2x/32` → 32).
fn dir_size(dir: &str) -> u32 {
    dir.split(|c: char| !c.is_ascii_digit()).filter_map(|n| n.parse().ok()).max().unwrap_or(0)
}

/// Icons kept: Launchpad, the drawer and the dock each at their own size, ~10 MB at 2× for 50 apps.
// ponytail: a working set past these caps evicts and re-rasterises every frame; raise them before a 4K Launchpad of
// 300 apps shows up in a profile.
const MAX_ENTRIES: usize = 512;
const MAX_BYTES: usize = 32 << 20;
/// Tray icons kept (image set × size).
const MAX_TRAY: usize = 32;

/// A missing icon, cached so the theme isn't searched again.
fn missing() -> Rendered {
    Rendered { pixels: Vec::new(), width: 0, height: 0, baseline: 0, truncated: false }
}

/// What the `icon-raster` thread draws.
enum Job {
    Theme(String, u32),
    Tray(Arc<Vec<Image>>, u32),
}

/// What it drew, back on the frame thread through `Icons::receive`.
pub enum Rasterised {
    Theme { name: String, px: u32, rendered: Option<Rendered> },
    Tray { images: Arc<Vec<Image>>, px: u32, rendered: Rendered },
}

impl Job {
    /// `files` remembers each name's theme files across jobs (the index.theme reads and stats).
    fn run(self, files: &mut HashMap<String, Vec<(u32, PathBuf)>>) -> Rasterised {
        match self {
            Job::Theme(name, px) => {
                // A miss isn't remembered: an install brings the icon along, and `Icons::clear` asks again.
                let found = match files.get(&name) {
                    Some(found) => found,
                    None => {
                        let found = icon_files(&name);
                        if found.is_empty() {
                            return Rasterised::Theme { name, px, rendered: None };
                        }
                        files.entry(name.clone()).or_insert(found)
                    }
                };
                let rendered = pick(found, px).and_then(|p| rasterise(p, px));
                Rasterised::Theme { name, px, rendered }
            }
            Job::Tray(images, px) => Rasterised::Tray { rendered: tray::render(&images, px), images, px },
        }
    }
}

/// App icons by name and pixel size, and tray icons by image set, rasterised on the `icon-raster` thread (C6, D5):
/// an icon asked for shows at its nearest other size, or not at all, until the thread has drawn it and `receive`
/// asked for a frame.
pub struct Icons {
    cache: Lru<(String, u32)>,
    /// Sent to the thread and not back yet.
    pending: HashSet<(String, u32)>,
    /// Tray icons by their image set (the `Arc` kept so its address stays unique) and size, oldest first.
    tray: Vec<(Arc<Vec<Image>>, u32, Arc<Rendered>)>,
    tray_pending: Vec<(Arc<Vec<Image>>, u32)>,
    /// `None`: no thread (tests), drawn on the spot.
    jobs: Option<mpsc::Sender<Job>>,
}

impl Default for Icons {
    fn default() -> Self {
        Self { cache: Lru::new(MAX_ENTRIES, MAX_BYTES), pending: HashSet::new(), tray: Vec::new(), tray_pending: Vec::new(), jobs: None }
    }
}

impl Icons {
    /// Starts the thread; what it draws comes back through `tx`, for `receive`.
    pub fn new(tx: Sender<Rasterised>) -> Self {
        let (jobs, rx) = mpsc::channel::<Job>();
        let spawned = std::thread::Builder::new().name("icon-raster".into()).spawn(move || {
            let mut files = HashMap::new();
            while let Ok(job) = rx.recv() {
                if tx.send(job.run(&mut files)).is_err() {
                    return;
                }
            }
        });
        let jobs = match spawned {
            Ok(_) => Some(jobs),
            Err(err) => {
                tracing::warn!("icons: {err}, drawing on the frame thread");
                None
            }
        };
        Self { jobs, ..Self::default() }
    }

    /// Forgets every drawn icon (the desktop files changed) but keeps the thread.
    pub fn clear(&mut self) {
        let jobs = self.jobs.take();
        *self = Self { jobs, ..Self::default() };
    }

    /// Icon `name` (an `App::icon`) for a `px`-sized square. `None`: the theme has no such icon, or it isn't drawn
    /// yet and no other size of it is.
    pub fn get(&mut self, name: &str, px: u32) -> Option<Arc<Rendered>> {
        let key = (name.to_owned(), px);
        if let Some(rendered) = self.cache.get(&key) {
            return Some(rendered).filter(|r| r.width > 0);
        }
        let Some(jobs) = &self.jobs else {
            self.receive(Job::Theme(key.0, px).run(&mut HashMap::new()));
            return self.cache.get(&(name.to_owned(), px)).filter(|r| r.width > 0);
        };
        if self.pending.insert(key.clone()) && jobs.send(Job::Theme(key.0.clone(), px)).is_err() {
            // The thread is gone: draw here from now on.
            self.jobs = None;
            self.pending.clear();
            return self.get(name, px);
        }
        // Until it lands: the same icon at the nearest size there is (the GPU scales it).
        self.cache.iter().filter(|((n, _), r)| n == name && r.width > 0).min_by_key(|((_, p), _)| p.abs_diff(px)).map(|(_, r)| r.clone())
    }

    /// A tray item's icon (`services::tray`) in a `px` square: blank until the thread has drawn it.
    pub fn tray(&mut self, images: &Arc<Vec<Image>>, px: u32) -> Arc<Rendered> {
        let same = |i: &Arc<Vec<Image>>, p: u32| Arc::ptr_eq(i, images) && p == px;
        if let Some((_, _, rendered)) = self.tray.iter().find(|(i, p, _)| same(i, *p)) {
            return rendered.clone();
        }
        match &self.jobs {
            Some(jobs) => {
                if !self.tray_pending.iter().any(|(i, p)| same(i, *p)) && jobs.send(Job::Tray(images.clone(), px)).is_ok() {
                    self.tray_pending.push((images.clone(), px));
                }
                Arc::new(Rendered { pixels: vec![0; (px * px * 4) as usize], width: px, height: px, baseline: px, truncated: false })
            }
            None => {
                self.receive(Job::Tray(images.clone(), px).run(&mut HashMap::new()));
                self.tray(images, px)
            }
        }
    }

    /// Keeps what the thread drew.
    pub fn receive(&mut self, done: Rasterised) {
        match done {
            Rasterised::Theme { name, px, rendered } => {
                self.pending.remove(&(name.clone(), px));
                self.cache.insert((name, px), Arc::new(rendered.unwrap_or_else(missing)));
            }
            Rasterised::Tray { images, px, rendered } => {
                self.tray_pending.retain(|(i, p)| !(Arc::ptr_eq(i, &images) && *p == px));
                if self.tray.len() >= MAX_TRAY {
                    self.tray.remove(0);
                }
                self.tray.push((images, px, Arc::new(rendered)));
            }
        }
    }
}

/// usvg with `<image href="file">` turned off: an icon file never pulls other files off the disk (B18).
pub fn svg_options() -> usvg::Options<'static> {
    let resolver = usvg::ImageHrefResolver { resolve_data: usvg::ImageHrefResolver::default_data_resolver(), resolve_string: Box::new(|_, _| None) };
    usvg::Options { image_href_resolver: resolver, ..Default::default() }
}

/// Premultiplied RGBA of the icon file at `path`: an SVG drawn at `px`, a PNG at its own size, or shrunk to `px`
/// here (bicubic) when it is bigger, not by the GPU's linear filter (D4).
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
        if info.width <= px && info.height <= px {
            return Some(rendered(premultiplied, info.width, info.height));
        }
        let source = tiny_skia::Pixmap::from_vec(premultiplied, tiny_skia::IntSize::from_wh(info.width, info.height)?)?;
        let mut pixmap = tiny_skia::Pixmap::new(px, px)?;
        let fit = px as f32 / source.width().max(source.height()) as f32;
        let (dx, dy) = ((px as f32 - source.width() as f32 * fit) / 2.0, (px as f32 - source.height() as f32 * fit) / 2.0);
        let paint = tiny_skia::PixmapPaint { quality: tiny_skia::FilterQuality::Bicubic, ..Default::default() };
        pixmap.draw_pixmap(0, 0, source.as_ref(), &paint, tiny_skia::Transform::from_row(fit, 0.0, 0.0, fit, dx, dy), None);
        return Some(rendered(pixmap.take(), px, px));
    }
    let tree = usvg::Tree::from_data(&bytes, &svg_options()).ok()?;
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
        parse("test", text, "de_DE.UTF-8", "noobsbot-desktop:Shared")
    }

    fn exec(exec: &str) -> Vec<String> {
        App { id: "kate".into(), name: "Kate".into(), exec: unescape(exec), icon: "kate".into(), wm_class: None, no_display: false, ..Default::default() }
            .command("")
    }

    #[test]
    fn dropped_files_fill_field_codes() {
        let kate = |exec: &str, uris: &[&str]| {
            let uris: Vec<String> = uris.iter().map(|u| u.to_string()).collect();
            App { id: "kate".into(), name: "Kate".into(), exec: exec.into(), icon: String::new(), wm_class: None, no_display: false, ..Default::default() }
                .command_with(&uris, "")
        };
        let two = ["file:///home/a/My%20File.txt", "file://localhost/tmp/b"];
        let Some(Entry::App(cd)) = app("[Desktop Entry]\nType=Application\nName=A\nExec=a %u\nPath=/opt/a\n") else { panic!() };
        assert_eq!(cd.command(""), ["env", "--chdir=/opt/a", "a"], "Path= runs it there");
        assert_eq!(cd.program(), "a");
        assert_eq!(kate("kate -b %F", &two), ["kate", "-b", "/home/a/My File.txt", "/tmp/b"]);
        assert_eq!(kate("kate %f", &two), ["kate", "/home/a/My File.txt"]);
        assert_eq!(kate("helium %U", &two), ["helium", two[0], two[1]]);
        assert_eq!(kate("app %u x", &[]), ["app", "x"]);
        assert_eq!(file_path("https://x/y"), None);
    }

    #[test]
    fn terminal_apps_run_in_a_terminal() {
        let Some(Entry::App(htop)) = app("[Desktop Entry]\nType=Application\nName=htop\nExec=htop -t\nTerminal=true\n") else { panic!() };
        assert!(htop.terminal);
        let argv = htop.exec_argv(&[]);
        assert_eq!(in_terminal(argv.clone(), true, "foot"), ["xdg-terminal-exec", "htop", "-t"]);
        assert_eq!(in_terminal(argv, false, "foot --app-id x"), ["foot", "--app-id", "x", "-e", "htop", "-t"], "no xdg-terminal-exec: the configured one");
        let Some(Entry::App(kate)) = app("[Desktop Entry]\nType=Application\nName=Kate\nExec=kate\nTerminal=false\n") else { panic!() };
        assert_eq!(kate.command("foot"), ["kate"]);
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
        // Meant for another desktop: out of launchers, still an app (its windows get matched).
        let shown = |line: &str| matches!(app(&format!("{base}{line}\n")), Some(Entry::App(App { no_display: false, .. })));
        assert!(shown("OnlyShowIn=Other;Shared;"));
        assert!(!shown("OnlyShowIn=Other;"));
        assert!(shown("NotShowIn=Other;"));
        assert!(!shown("NotShowIn=Other;noobsbot-desktop;"));
        assert!(matches!(app(&format!("{base}NotShowIn=Other;noobsbot-desktop;\n")), Some(Entry::App(_))));
        assert!(shown("TryExec=sh"), "on PATH");
        assert_eq!(app(&format!("{base}TryExec=/nowhere/such-program\n")), None, "not installed");
        // Keys of other groups (actions) never leak into the main entry.
        let Some(Entry::App(a)) = app("[Desktop Entry]\nType=Application\nName=A\nExec=a\n[Desktop Action new]\nName=New\nExec=a --new\n") else { panic!() };
        assert_eq!((a.name.as_str(), a.exec.as_str()), ("A", "a"));
    }

    #[test]
    fn comment_keywords_and_categories() {
        let base =
            "[Desktop Entry]\nType=Application\nName=Kate\nExec=kate\nGenericName=Text Editor\nKeywords=text;editor;\nCategories=Qt;Utility;TextEditor;\n";
        let Some(Entry::App(kate)) = app(&format!("{base}Comment=Edit text\nComment[de]=Text bearbeiten\nKeywords[de]=Text;Bearbeiter\n")) else { panic!() };
        assert_eq!((kate.comment.as_str(), kate.generic_name.as_str()), ("Text bearbeiten", "Text Editor"));
        assert_eq!(kate.keywords, ["Text", "Bearbeiter", "text", "editor"], "translated first, then the plain ones");
        assert_eq!(kate.categories, ["Qt", "Utility", "TextEditor"]);
        let Some(Entry::App(bare)) = app(base) else { panic!() };
        assert_eq!(bare.comment, "Text Editor", "no Comment: the generic name");
        assert_eq!(bare.keywords, ["text", "editor"]);
    }

    #[test]
    fn desktop_actions_in_listed_order() {
        let text = "[Desktop Entry]\nName=Chrome\nExec=chrome\nActions=new-private-window;new-window;gone;\n\n[Desktop Action new-window]\nName=New Window\nName[de]=Neues Fenster\nExec=chrome --new\n[Desktop Action new-private-window]\nName=New Incognito Window\nExec=chrome --incognito\n";
        let names = |locale| parse_actions(text, locale).into_iter().map(|(n, _)| n).collect::<Vec<_>>();
        assert_eq!(names("C"), ["New Incognito Window", "New Window"]);
        assert_eq!(names("de_DE.UTF-8"), ["New Incognito Window", "Neues Fenster"]);
        assert_eq!(parse_actions(text, "C")[1].1, "chrome --new");
        assert!(parse_actions("[Desktop Entry]\nExec=a\n", "C").is_empty());
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
        write(&system, "files.desktop", &entry("Files", "files", ""));
        write(&system, "a-wrapper.desktop", &entry("A wrapper", "kate --wrapped", ""));
        let apps = Apps::scan(&[user, system], "", "");
        let ids: Vec<&str> = apps.list().iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["a-wrapper", "files", "org.kde.kate", "org.kde.dolphin", "kde-term"], "sorted by name, gone hidden");
        assert_eq!(apps.find("KATE").map(|a| a.id.as_str()), Some("org.kde.kate"), "an id's last segment beats a program, whatever the sort order");
        assert_eq!(apps.index.len(), 8, "one key per distinct id, wm class, id tail and program");
        assert_eq!(apps.find("org.kde.dolphin").map(|a| a.name.as_str()), Some("My Dolphin"));
        assert_eq!(apps.find("org.kde.kate.desktop").map(|a| a.id.as_str()), Some("org.kde.kate"));
        assert_eq!(apps.find("kate").map(|a| a.id.as_str()), Some("org.kde.kate"));
        assert_eq!(apps.find("noobsbot").map(|a| a.id.as_str()), Some("kde-term"), "by StartupWMClass, any case");
        assert_eq!(apps.find("gone"), None);
        // A window's app id and the pinned name land on one dock item: the same app either way round.
        let id = |key: &str| apps.find(key).map(|a| a.id.as_str());
        assert_eq!(id("org.example.Files"), Some("files"), "a reverse-DNS app id for a plain desktop file");
        assert_eq!(id("org.example.Files"), id("files"));
        assert_eq!(id("io.github.Noobsbot"), id("Noobsbot"), "by the last part against StartupWMClass");
        assert_eq!(id("org.kde.kate"), id("Kate"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn icon_dirs_sort_by_size() {
        assert_eq!(dir_size("48x48/apps"), 48);
        assert_eq!(dir_size("apps@2x/32"), 32);
        assert_eq!(dir_size("apps/scalable"), 0);
    }

    #[test]
    fn picks_the_exact_size_then_svg_then_the_smallest_bigger() {
        let files: Vec<(u32, PathBuf)> = [(16, "16.png"), (48, "48.png"), (512, "512.png"), (u32::MAX, "s.svg")].map(|(s, p)| (s, PathBuf::from(p))).into();
        let pick_at = |px| pick(&files, px).and_then(|p| p.to_str()).unwrap();
        assert_eq!(pick_at(48), "48.png", "exact");
        assert_eq!(pick_at(64), "s.svg", "no exact bitmap: the SVG");
        assert_eq!(pick_at(u32::MAX), "s.svg", "find_icon: scalable first");
        let bitmaps = &files[..3];
        assert_eq!(pick(bitmaps, 64).unwrap().to_str(), Some("512.png"), "the smallest bigger one, shrunk later");
        assert_eq!(pick(bitmaps, 1024).unwrap().to_str(), Some("512.png"), "nothing bigger: the largest");
        assert_eq!(pick(&[], 32), None);
    }

    #[test]
    fn icon_cache_evicts_and_shows_the_nearest_size_meanwhile() {
        let mut icons = Icons::default();
        let big = Rendered { pixels: vec![0; MAX_BYTES / 2 + 1], width: 1, height: 1, baseline: 1, truncated: false };
        icons.receive(Rasterised::Theme { name: "a".into(), px: 32, rendered: Some(big) });
        icons.receive(Rasterised::Theme {
            name: "b".into(),
            px: 32,
            rendered: Some(Rendered { pixels: vec![0; 4], width: 1, height: 1, baseline: 1, truncated: false }),
        });
        icons.receive(Rasterised::Theme {
            name: "a".into(),
            px: 64,
            rendered: Some(Rendered { pixels: vec![0; MAX_BYTES / 2 + 1], width: 2, height: 2, baseline: 2, truncated: false }),
        });
        assert!(icons.get("a", 32).is_none() && icons.get("b", 32).is_some(), "the big one made room for the other big one");
        // A pending size shows the nearest one there is; a missing icon stays None and isn't looked up again.
        let (tx, rx) = smithay::reexports::calloop::channel::channel();
        let mut icons = Icons::new(tx);
        icons.receive(Rasterised::Theme {
            name: "a".into(),
            px: 64,
            rendered: Some(Rendered { pixels: vec![0; 16], width: 2, height: 2, baseline: 2, truncated: false }),
        });
        icons.receive(Rasterised::Theme {
            name: "a".into(),
            px: 16,
            rendered: Some(Rendered { pixels: vec![0; 4], width: 1, height: 1, baseline: 1, truncated: false }),
        });
        assert_eq!(icons.get("a", 48).map(|r| r.width), Some(2), "nearest size while 48 is pending");
        assert!(icons.pending.contains(&("a".to_owned(), 48)));
        icons.receive(Rasterised::Theme { name: "a".into(), px: 48, rendered: None });
        assert!(icons.get("a", 48).is_none() && icons.pending.is_empty());
        let done = rx.recv().unwrap();
        assert!(matches!(done, Rasterised::Theme { name, px: 48, rendered: None } if name == "a"), "the thread looked a nameless icon up once");
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

    #[test]
    fn finds_icons_in_a_base_without_index_theme() {
        let dir = std::env::temp_dir().join(format!("noobsbot-icon-bases-{}", std::process::id()));
        let (local, usr) = (dir.join("local/icons"), dir.join("usr/icons"));
        for d in [local.join("hicolor/scalable/apps"), usr.join("hicolor/48x48/apps")] {
            std::fs::create_dir_all(d).unwrap();
        }
        // Like Debian: only /usr/share/icons/hicolor has the index; /usr/local/share/icons/hicolor has the icon.
        std::fs::write(usr.join("hicolor/index.theme"), "[Icon Theme]\nDirectories=48x48/apps,scalable/apps\n").unwrap();
        std::fs::write(local.join("hicolor/scalable/apps/term.svg"), "<svg/>").unwrap();
        std::fs::write(usr.join("hicolor/48x48/apps/other.png"), "").unwrap();
        let bases = [local.clone(), usr.clone()];
        assert_eq!(theme_icon_files("term", &bases), [(u32::MAX, local.join("hicolor/scalable/apps/term.svg"))]);
        assert_eq!(theme_icon_files("other", &bases), [(48, usr.join("hicolor/48x48/apps/other.png"))]);
        assert!(theme_icon_files("none", &bases).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
