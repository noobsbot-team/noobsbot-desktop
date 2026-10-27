//! App theme generator (E2.9): `noobsbot-desktop theme --write` turns the Part D tokens the shell draws with into a
//! Kvantum theme, a Qt colour scheme and the kdeglobals keys (plus gsettings on request), so Qt and GTK apps
//! match the desktop in light and dark. Files the owner also edits (kdeglobals, kvantum.kvconfig, gtk.css) are merged: only
//! our keys change, every other line stays byte-for-byte.

use std::{
    fmt::Write,
    fs, io,
    path::{Path, PathBuf},
    sync::{OnceLock, mpsc},
};

use crate::{
    config::{self, Accent, Appearance, Mouse},
    render::{self, Material},
};

const USAGE: &str = "usage: noobsbot-desktop theme --write [--gsettings]";

type Rgb = [u8; 3];
/// INI groups in file order: name → keys and values.
type Groups = Vec<(&'static str, Vec<(&'static str, String)>)>;

/// Kvantum folder and colour scheme name.
fn name(dark: bool) -> &'static str {
    if dark { "NoobsbotGlassDark" } else { "NoobsbotGlass" }
}

fn icons(dark: bool) -> &'static str {
    if dark { "WhiteSur-dark" } else { "WhiteSur" }
}

/// `org.gnome.desktop.interface` keys and values for GTK apps.
fn gsettings(dark: bool) -> [[&'static str; 2]; 6] {
    [
        ["color-scheme", if dark { "prefer-dark" } else { "default" }],
        ["gtk-theme", if dark { "WhiteSur-Dark" } else { "WhiteSur-Light" }],
        ["icon-theme", icons(dark)],
        ["font-name", "Inter 10"],
        ["monospace-font-name", "JetBrains Mono 10"],
        ["overlay-scrolling", "true"],
    ]
}

const CSS_BEGIN: &str = "/* noobsbot-desktop theme: begin */";
const CSS_END: &str = "/* noobsbot-desktop theme: end */";

/// GTK's accent colours (libadwaita's named colours, which GTK 3 themes read too) for `gtk.css`.
fn gtk_css(accent: Accent, dark: bool) -> String {
    let a = hex(render::accent(accent, dark));
    format!("{CSS_BEGIN}\n@define-color accent_color {a};\n@define-color accent_bg_color {a};\n@define-color accent_fg_color #FFFFFF;\n{CSS_END}\n")
}

/// `text` with our block (between the markers) replaced, or appended; everything else stays byte-for-byte.
fn css_merge(text: &str, ours: &str) -> String {
    match text.find(CSS_BEGIN).zip(text.find(CSS_END)).filter(|(b, e)| b < e) {
        Some((begin, end)) => {
            let end = text[end + CSS_END.len()..].strip_prefix('\n').map_or(text.len(), |rest| text.len() - rest.len());
            format!("{}{ours}{}", &text[..begin], &text[end..])
        }
        None if text.is_empty() || text.ends_with('\n') => format!("{text}{ours}"),
        None => format!("{text}\n{ours}"),
    }
}

/// The text size and pointer size keys of `org.gnome.desktop.interface` (E8.4).
fn sizes(appearance: &Appearance) -> [[String; 2]; 2] {
    [
        ["text-scaling-factor".into(), format!("{:.2}", appearance.text_scale())],
        ["cursor-size".into(), crate::backend::cursor::size(appearance.pointer_size).to_string()],
    ]
}

/// The `[mouse]` keys GTK reads: schema, key, value (E8.2).
fn mouse_keys(mouse: &Mouse) -> [[String; 3]; 2] {
    [
        ["org.gnome.desktop.peripherals.mouse".into(), "double-click".into(), mouse.double_click().as_millis().to_string()],
        ["org.gnome.desktop.interface".into(), "gtk-enable-primary-paste".into(), mouse.middle_click_paste.to_string()],
    ]
}

/// The Qt font size in points: 10 times the text scale.
fn qt_points(appearance: &Appearance) -> u32 {
    (10.0 * appearance.text_scale()).round() as u32
}

/// `ink` (0 or 255) at `alpha` over `bg`, flattened: Qt colour schemes and Kvantum want solid colours.
fn over(ink: u8, alpha: f32, bg: Rgb) -> Rgb {
    mix([ink; 3], alpha, bg)
}

/// `fg` at `alpha` over `bg`.
fn mix(fg: Rgb, alpha: f32, bg: Rgb) -> Rgb {
    std::array::from_fn(|i| (f32::from(fg[i]) * alpha + f32::from(bg[i]) * (1.0 - alpha)).round() as u8)
}

fn hex([r, g, b]: Rgb) -> String {
    format!("#{r:02X}{g:02X}{b:02X}")
}

fn kde([r, g, b]: Rgb) -> String {
    format!("{r},{g},{b}")
}

/// D2 label ink and its primary / secondary alphas.
fn ink(dark: bool) -> (u8, f32, f32) {
    if dark { (255, 0.85, 0.55) } else { (0, 0.85, 0.50) }
}

/// The `[Colors:*]` groups of the Qt colour scheme: window = `panel` glass, views = `card`, menus = `popover`.
fn colour_groups(dark: bool, accent: Accent) -> Groups {
    let tint = |m: Material| m.spec(dark).tint;
    let a = render::accent(accent, dark);
    let (ink, label, secondary) = ink(dark);
    let fill = over(ink, if dark { 0.08 } else { 0.05 }, tint(Material::Card));
    let group = |bg: Rgb, alt: Rgb, ink: u8| {
        vec![
            ("BackgroundNormal", kde(bg)),
            ("BackgroundAlternate", kde(alt)),
            ("ForegroundNormal", kde(over(ink, label, bg))),
            ("ForegroundInactive", kde(over(ink, secondary, bg))),
            ("ForegroundActive", kde(a)),
            ("ForegroundLink", kde(a)),
            ("ForegroundVisited", kde(render::accent(Accent::Purple, dark))),
            ("ForegroundNegative", kde(render::accent(Accent::Red, dark))),
            ("ForegroundNeutral", kde(render::accent(Accent::Orange, dark))),
            ("ForegroundPositive", kde(render::accent(Accent::Green, dark))),
            ("DecorationFocus", kde(a)),
            ("DecorationHover", kde(a)),
        ]
    };
    vec![
        ("Colors:Window", group(tint(Material::Panel), tint(Material::Panel), ink)),
        ("Colors:View", group(tint(Material::Card), fill, ink)),
        ("Colors:Button", group(tint(Material::Popover), tint(Material::Popover), ink)),
        ("Colors:Selection", group(a, a, 255)),
        ("Colors:Tooltip", group(tint(Material::Popover), tint(Material::Popover), ink)),
        ("Colors:Header", group(tint(Material::Bar), tint(Material::Bar), ink)),
        ("Colors:Complementary", group(Material::Hud.spec(true).tint, Material::Hud.spec(true).tint, 255)),
    ]
}

/// The `.colors` file.
fn scheme(dark: bool, accent: Accent) -> Groups {
    let mut groups: Groups =
        vec![("General", vec![("ColorScheme", name(dark).into()), ("Name", format!("Noobsbot Glass{}", if dark { " Dark" } else { "" }))])];
    groups.extend(colour_groups(dark, accent));
    groups.push(("KDE", vec![("contrast", "4".into())]));
    groups
}

/// Our Kvantum keys: D3 opacity (window = `panel`, menus = `popover`), translucency on, no blur, Part D colours.
fn kvantum(appearance: &Appearance, dark: bool) -> Groups {
    let reduce = |m: Material| ((1.0 - m.spec(dark).opacity * appearance.glass_opacity) * 100.0).round().clamp(0.0, 90.0).to_string();
    let tint = |m: Material| m.spec(dark).tint;
    let a = render::accent(appearance.accent, dark);
    let (ink, label, secondary) = ink(dark);
    let text = hex(over(ink, label, tint(Material::Card)));
    vec![
        (
            "%General",
            vec![
                ("comment", "Noobsbot Glass, generated by noobsbot-desktop theme --write".into()),
                ("composite", "true".into()),
                ("translucent_windows", "true".into()),
                ("reduce_window_opacity", reduce(Material::Panel)),
                ("reduce_menu_opacity", reduce(Material::Popover)),
                ("blurring", "false".into()),
                ("popup_blurring", "false".into()),
                // Overlay scrollbars, as the desktop's own.
                ("transient_scrollbar", "true".into()),
            ],
        ),
        (
            "GeneralColors",
            vec![
                ("window.color", hex(tint(Material::Panel))),
                ("base.color", hex(tint(Material::Card))),
                ("alt.base.color", hex(over(ink, if dark { 0.08 } else { 0.05 }, tint(Material::Card)))),
                ("button.color", hex(tint(Material::Popover))),
                ("highlight.color", hex(a)),
                ("inactive.highlight.color", hex(mix(a, 0.2, tint(Material::Card)))),
                ("text.color", text.clone()),
                ("window.text.color", text.clone()),
                ("button.text.color", text.clone()),
                ("tooltip.text.color", text),
                ("disabled.text.color", hex(over(ink, secondary, tint(Material::Card)))),
                ("highlight.text.color", "#FFFFFF".into()),
                ("link.color", hex(a)),
                ("link.visited.color", hex(render::accent(Accent::Purple, dark))),
            ],
        ),
    ]
}

fn header(line: &str) -> Option<&str> {
    line.trim().strip_prefix('[')?.strip_suffix(']')
}

fn key(line: &str) -> Option<&str> {
    let line = line.trim_start();
    if line.starts_with(['#', ';']) { None } else { line.split_once('=').map(|(k, _)| k.trim()) }
}

fn lookup<'a>(groups: &'a Groups, group: &str, key: &str) -> Option<&'a str> {
    groups.iter().filter(|(g, _)| *g == group).flat_map(|(_, keys)| keys).find(|(k, _)| *k == key).map(|(_, v)| v.as_str())
}

/// A value from INI `text`.
fn get<'a>(text: &'a str, group: &str, wanted: &str) -> Option<&'a str> {
    let mut current = "";
    for line in text.lines() {
        if let Some(h) = header(line) {
            current = h;
        } else if current == group && key(line) == Some(wanted) {
            return line.split_once('=').map(|(_, v)| v.trim());
        }
    }
    None
}

/// Merges `ours` into INI `text`: our keys replace theirs in place (missing ones go under the group's header), groups
/// `owned` says are ours are replaced whole, and every other line is kept byte-for-byte.
fn merge(text: &str, ours: &Groups, owned: impl Fn(&str) -> bool) -> String {
    let mut present = Vec::new();
    let mut group = "";
    for line in text.lines() {
        if let Some(h) = header(line) {
            group = h;
        } else if let Some(k) = key(line) {
            present.push((group, k));
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut seen = Vec::new();
    let (mut group, mut skip) = ("", false);
    for line in text.split_inclusive('\n') {
        if let Some(h) = header(line) {
            (group, skip) = (h, owned(h));
            if skip {
                continue;
            }
            out.push_str(line);
            if seen.contains(&h) {
                continue;
            }
            seen.push(h);
            for (k, v) in ours.iter().filter(|(g, _)| *g == h).flat_map(|(_, keys)| keys).filter(|(k, _)| !present.contains(&(h, *k))) {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                let _ = writeln!(out, "{k}={v}");
            }
        } else if skip {
        } else if let Some((k, v)) = key(line).and_then(|k| Some(k).zip(lookup(ours, group, k))) {
            let _ = write!(out, "{k}={v}{}", if line.ends_with('\n') { "\n" } else { "" });
        } else {
            out.push_str(line);
        }
    }
    for (g, keys) in ours.iter().filter(|(g, _)| owned(g) || !seen.contains(g)) {
        if !out.is_empty() && !out.ends_with("\n\n") {
            out.push_str(if out.ends_with('\n') { "\n" } else { "\n\n" });
        }
        let _ = writeln!(out, "[{g}]");
        for (k, v) in keys {
            let _ = writeln!(out, "{k}={v}");
        }
    }
    out
}

/// `groups` as a fresh INI file.
fn ini(groups: &Groups) -> String {
    merge("", groups, |_| true)
}

/// WhiteSur's SVG with its window, base and highlight colours swapped for ours, by colour string.
/// ponytail: plain string swap; a colour WhiteSur also uses for something else changes there too.
fn recolour(svg: &str, theirs: &str, ours: &Groups) -> String {
    let mut svg = svg.to_owned();
    for k in ["window.color", "base.color", "highlight.color"] {
        let (Some(from), Some(to)) = (get(theirs, "GeneralColors", k), lookup(ours, "GeneralColors", k)) else { continue };
        if let Ok(re) = regex_lite::Regex::new(&format!("(?i){}", regex_lite::escape(from))) {
            svg = re.replace_all(&svg, regex_lite::NoExpand(to)).into_owned();
        }
    }
    svg
}

/// WhiteSur's installed Kvantum theme (kvconfig, svg), if any.
/// WhiteSur's kvconfig and SVG: the user's copy (Kvantum Manager installs to `config_home/Kvantum`) or the package's.
fn whitesur(dark: bool, config_home: &Path) -> Option<(String, String)> {
    let name = if dark { "WhiteSurDark" } else { "WhiteSur" };
    [config_home.join("Kvantum"), PathBuf::from("/usr/share/Kvantum")].into_iter().find_map(|root| {
        // Both variants live in the one folder.
        let dir = root.join("WhiteSur");
        Some((fs::read_to_string(dir.join(format!("{name}.kvconfig"))).ok()?, fs::read_to_string(dir.join(format!("{name}.svg"))).ok()?))
    })
}

/// The file's text, empty if it doesn't exist yet. Follows a symlink so a dotfiles link stays a link.
fn read(path: &Path) -> io::Result<(PathBuf, String)> {
    let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    match fs::read_to_string(&path) {
        Ok(text) => Ok((path, text)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok((path, String::new())),
        Err(err) => Err(err),
    }
}

/// Writes the light and dark themes and selects `appearance`'s mode. Returns the files written.
pub fn write(appearance: &Appearance, config_home: &Path, data_home: &Path) -> io::Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    let mut save = |path: PathBuf, text: String| config::save_atomic(&path, &text).map(|()| written.push(path));
    for dark in [false, true] {
        let name = name(dark);
        let dir = config_home.join("Kvantum").join(name);
        let ours = kvantum(appearance, dark);
        // Without WhiteSur the kvconfig alone is a full theme on Kvantum's built-in SVG.
        let kvconfig = match whitesur(dark, config_home) {
            Some((theirs, svg)) => {
                save(dir.join(format!("{name}.svg")), recolour(&svg, &theirs, &ours))?;
                merge(&theirs, &ours, |_| false)
            }
            // The built-in SVG draws a dark toolbar: without its interior, Kvantum paints it in our window colour.
            // ponytail: selections stay the SVG's grey (only an SVG colours them); WhiteSur, installed by install.sh,
            // takes the branch above.
            None => ini(&[ours, vec![("Toolbar", vec![("interior", "false".into()), ("frame", "false".into())])]].concat()),
        };
        save(dir.join(format!("{name}.kvconfig")), kvconfig)?;
        save(data_home.join("color-schemes").join(format!("{name}.colors")), ini(&scheme(dark, appearance.accent)))?;
    }

    let dark = render::is_dark(appearance);
    let (path, text) = read(&config_home.join("Kvantum").join("kvantum.kvconfig"))?;
    save(path, merge(&text, &vec![("General", vec![("theme", name(dark).into())])], |_| false))?;

    let mut ours: Groups = vec![
        (
            "General",
            vec![
                ("ColorScheme", name(dark).into()),
                ("font", format!("Inter,{}", qt_points(appearance))),
                ("fixed", format!("JetBrains Mono,{}", qt_points(appearance))),
            ],
        ),
        ("Icons", vec![("Theme", icons(dark).into())]),
        // KF6 apps (Dolphin) pick their palette here; without it they fall back to the bundled Breeze Light: dark text.
        ("UiSettings", vec![("ColorScheme", name(dark).into())]),
    ];
    ours.extend(colour_groups(dark, appearance.accent));
    let (path, text) = read(&config_home.join("kdeglobals"))?;
    save(path, merge(&text, &ours, |g| g.starts_with("Colors:")))?;

    let css = gtk_css(appearance.accent, dark);
    for gtk in ["gtk-3.0", "gtk-4.0"] {
        let (path, text) = read(&config_home.join(gtk).join("gtk.css"))?;
        save(path, css_merge(&text, &css))?;
    }
    Ok(written)
}

fn homes() -> (PathBuf, PathBuf) {
    (config::xdg_dir("XDG_CONFIG_HOME", ".config"), config::xdg_dir("XDG_DATA_HOME", ".local/share"))
}

/// `gsettings set org.gnome.desktop.interface key value`.
fn gsettings_command([key, value]: [&str; 2]) -> std::process::Command {
    let mut command = std::process::Command::new("gsettings");
    command.args(["set", "org.gnome.desktop.interface", key, value]);
    command
}

/// Rewrites the files for `appearance` on a worker thread, so the frame never waits; the newest request wins.
/// `gsettings`: also set the GTK colour scheme, so GTK4/libadwaita apps follow (a real session only, never nested:
/// that is the host's setting). The other keys stay a CLI step (`theme --write --gsettings`).
pub fn write_in_background(appearance: Appearance, gsettings: bool) {
    static WORKER: OnceLock<mpsc::Sender<(Appearance, bool)>> = OnceLock::new();
    let tx = WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<(Appearance, bool)>();
        let spawned = std::thread::Builder::new().name("theme-write".into()).spawn(move || {
            while let Ok(first) = rx.recv() {
                let (appearance, gsettings) = rx.try_iter().last().unwrap_or(first);
                let (config_home, data_home) = homes();
                if let Err(err) = write(&appearance, &config_home, &data_home) {
                    tracing::warn!("app theme: {err}");
                }
                if gsettings && !gsettings_command(self::gsettings(render::is_dark(&appearance))[0]).status().is_ok_and(|s| s.success()) {
                    tracing::warn!("gsettings color-scheme: failed");
                }
            }
        });
        if let Err(err) = spawned {
            tracing::warn!("app theme off: {err}");
        }
        tx
    });
    let _ = tx.send((appearance, gsettings));
}

/// Sets the GTK text scale and pointer size on a thread (a real session only, never nested), so the frame never waits.
pub fn sizes_in_background(appearance: &Appearance) {
    let sizes = sizes(appearance);
    std::thread::spawn(move || {
        for [key, value] in &sizes {
            if !gsettings_command([key, value]).status().is_ok_and(|s| s.success()) {
                tracing::warn!("gsettings {key}: failed");
            }
        }
    });
}

/// Sets the GTK mouse keys on a thread (a real session only, never nested), so the frame never waits.
pub fn mouse_in_background(mouse: &Mouse) {
    let keys = mouse_keys(mouse);
    std::thread::spawn(move || {
        for [schema, key, value] in &keys {
            if !std::process::Command::new("gsettings").args(["set", schema, key, value]).status().is_ok_and(|s| s.success()) {
                tracing::warn!("gsettings {key}: failed");
            }
        }
    });
}

/// `noobsbot-desktop theme …`: the exit code.
pub fn cli(args: impl Iterator<Item = String>) -> i32 {
    let (mut write_files, mut run_gsettings) = (false, false);
    for arg in args {
        match arg.as_str() {
            "--write" => write_files = true,
            "--gsettings" => run_gsettings = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            other => {
                eprintln!("unknown argument: {other}\n{USAGE}");
                return 2;
            }
        }
    }
    if !write_files {
        eprintln!("{USAGE}");
        return 2;
    }
    let appearance = config::load(&config::path()).appearance;
    let (config_home, data_home) = homes();
    match write(&appearance, &config_home, &data_home) {
        Ok(paths) => paths.iter().for_each(|p| println!("wrote {}", p.display())),
        Err(err) => {
            eprintln!("theme: {err}");
            return 1;
        }
    }
    if !run_gsettings {
        println!("GTK (pass --gsettings to apply):");
    }
    let sizes = sizes(&appearance);
    for [key, value] in gsettings(render::is_dark(&appearance)).into_iter().chain(sizes.iter().map(|[k, v]| [k.as_str(), v.as_str()])) {
        println!("gsettings set org.gnome.desktop.interface {key} '{value}'");
        if run_gsettings && !gsettings_command([key, value]).status().is_ok_and(|s| s.success()) {
            eprintln!("gsettings {key}: failed");
            return 1;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_colour_scheme_command_is_what_the_worker_runs() {
        // Built only: never run here, the host's GTK setting is not ours to touch.
        for (dark, value) in [(true, "prefer-dark"), (false, "default")] {
            let command = gsettings_command(gsettings(dark)[0]);
            assert_eq!(command.get_program(), "gsettings");
            let args: Vec<_> = command.get_args().collect();
            assert_eq!(args, ["set", "org.gnome.desktop.interface", "color-scheme", value]);
        }
    }

    #[test]
    fn mouse_keys_go_to_gtk() {
        let mouse = Mouse { double_click_ms: 650, middle_click_paste: false, ..Default::default() };
        assert_eq!(mouse_keys(&mouse)[0][1..], ["double-click".to_string(), "650".into()]);
        assert_eq!(mouse_keys(&mouse)[1][1..], ["gtk-enable-primary-paste".to_string(), "false".into()]);
    }

    #[test]
    fn text_and_pointer_sizes_follow_the_config() {
        let appearance = Appearance { text_scale: 1.26, pointer_size: 32, ..Appearance::default() };
        assert_eq!(qt_points(&appearance), 13);
        assert_eq!(sizes(&appearance), [["text-scaling-factor".to_string(), "1.26".into()], ["cursor-size".into(), "32".into()]]);
        // Out of range clamps.
        assert_eq!(qt_points(&Appearance { text_scale: 3.0, ..Appearance::default() }), 15);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("noobsbot-desktop-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn kdeglobals_merge_keeps_the_rest() {
        let theirs = "# owner's\n[General]\nTerminalApplication=noobsbot\nfont=Comic Sans,12\n\n[Colors:View]\nBackgroundNormal=1,2,3\nOld=x\n\n[KDE]\nSingleClick=false";
        let ours: Groups = vec![
            ("General", vec![("ColorScheme", "NoobsbotGlass".into()), ("font", "Inter,10".into())]),
            ("Icons", vec![("Theme", "WhiteSur".into())]),
            ("Colors:View", vec![("BackgroundNormal", "255,255,255".into())]),
        ];
        let merged = merge(theirs, &ours, |g| g.starts_with("Colors:"));
        assert_eq!(
            merged,
            "# owner's\n[General]\nColorScheme=NoobsbotGlass\nTerminalApplication=noobsbot\nfont=Inter,10\n\n[KDE]\nSingleClick=false\n\n[Icons]\nTheme=WhiteSur\n\n[Colors:View]\nBackgroundNormal=255,255,255\n"
        );
        assert_eq!(merge(&merged, &ours, |g| g.starts_with("Colors:")), merged, "merging twice changes nothing");
        assert_eq!(get(&merged, "General", "TerminalApplication"), Some("noobsbot"));
    }

    #[test]
    fn writes_accent_light_and_dark() {
        let dir = temp_dir("theme");
        let (config_home, data_home) = (dir.join("config"), dir.join("data"));
        fs::create_dir_all(&config_home).unwrap();
        fs::write(config_home.join("kdeglobals"), "[General]\nTerminalApplication=noobsbot\n").unwrap();
        let appearance = Appearance { accent: Accent::Green, mode: config::Mode::Dark, ..Default::default() };
        write(&appearance, &config_home, &data_home).unwrap();

        let colors = |n: &str| fs::read_to_string(data_home.join("color-schemes").join(format!("{n}.colors"))).unwrap();
        assert!(colors("NoobsbotGlass").contains("=52,199,89"));
        assert!(colors("NoobsbotGlassDark").contains("=48,209,88"));
        let kv = |n: &str| fs::read_to_string(config_home.join("Kvantum").join(n).join(format!("{n}.kvconfig"))).unwrap();
        assert!(kv("NoobsbotGlass").contains("highlight.color=#34C759") && kv("NoobsbotGlass").contains("translucent_windows=true"));
        assert!(kv("NoobsbotGlassDark").contains("highlight.color=#30D158") && kv("NoobsbotGlassDark").contains("reduce_window_opacity=15"));

        let kdeglobals = fs::read_to_string(config_home.join("kdeglobals")).unwrap();
        assert_eq!(get(&kdeglobals, "General", "TerminalApplication"), Some("noobsbot"));
        assert_eq!(get(&kdeglobals, "General", "ColorScheme"), Some("NoobsbotGlassDark"));
        assert_eq!(get(&kdeglobals, "UiSettings", "ColorScheme"), Some("NoobsbotGlassDark"));
        assert_eq!(get(&kdeglobals, "Colors:Selection", "BackgroundNormal"), Some("48,209,88"));
        let css = fs::read_to_string(config_home.join("gtk-4.0").join("gtk.css")).unwrap();
        assert!(css.contains("@define-color accent_color #30D158;"));
        let selected = fs::read_to_string(config_home.join("Kvantum").join("kvantum.kvconfig")).unwrap();
        assert_eq!(get(&selected, "General", "theme"), Some("NoobsbotGlassDark"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn gtk_css_keeps_the_owners_rules() {
        let ours = gtk_css(Accent::Green, true);
        assert!(ours.contains("@define-color accent_bg_color #30D158;"));
        let theirs = "window { margin: 0; }";
        let merged = css_merge(theirs, &ours);
        assert_eq!(merged, format!("{theirs}\n{ours}"));
        // Replaced in place, not added twice.
        let again = css_merge(&format!("{merged}.x {{}}\n"), &gtk_css(Accent::Red, false));
        assert_eq!(again, format!("{theirs}\n{}.x {{}}\n", gtk_css(Accent::Red, false)));
    }

    #[test]
    fn recolours_whitesur_svg() {
        let ours = kvantum(&Appearance::default(), false);
        let svg = recolour("<rect fill=\"#3C84F7\"/><rect fill=\"#3c84f7\"/>", "[GeneralColors]\nhighlight.color=#3c84f7\n", &ours);
        assert_eq!(svg, "<rect fill=\"#007AFF\"/><rect fill=\"#007AFF\"/>");
    }
}
