//! General ▸ Login Items: the XDG autostart entries (`~/.config/autostart`, then `/etc/xdg/autostart`), which the
//! session starts at login (systemd's autostart generator). Off is `Hidden=true` in the user's own copy.

use std::{
    collections::{BTreeMap, HashMap},
    io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// The file name, the same in every dir.
    pub file: String,
    pub name: String,
    pub on: bool,
    /// Only in the user's dir: − deletes it.
    pub removable: bool,
}

/// An installed app, for "+".
#[derive(Clone, Debug, PartialEq)]
pub struct Installed {
    pub name: String,
    pub path: PathBuf,
}

impl std::fmt::Display for Installed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

fn env_dir(variable: &str, home_relative: &str) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(home_relative))
}

/// `~/.config/autostart`.
pub fn user_dir() -> PathBuf {
    env_dir("XDG_CONFIG_HOME", ".config").join("autostart")
}

/// The system's autostart dirs, most important first.
pub fn system_dirs() -> Vec<PathBuf> {
    let dirs = std::env::var("XDG_CONFIG_DIRS").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "/etc/xdg".into());
    dirs.split(':').filter(|d| !d.is_empty()).map(|d| Path::new(d).join("autostart")).collect()
}

fn desktop() -> String {
    std::env::var("XDG_CURRENT_DESKTOP").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "noobsbot-desktop".into())
}

/// The `[Desktop Entry]` keys (the desktop's `apps::parse`).
fn fields(text: &str) -> HashMap<&str, &str> {
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
    fields
}

/// `OnlyShowIn`/`NotShowIn` allow `desktop` (`$XDG_CURRENT_DESKTOP`, `:`-separated).
fn shows_here(fields: &HashMap<&str, &str>, desktop: &str) -> bool {
    let listed = |key: &str| fields.get(key).map(|v| v.split(';').any(|d| !d.is_empty() && desktop.split(':').any(|c| c == d)));
    listed("OnlyShowIn") != Some(false) && listed("NotShowIn") != Some(true)
}

/// `*.desktop` files by name; a file in a later dir replaces the same name from an earlier one.
fn files<'a>(dirs: impl Iterator<Item = &'a Path>) -> BTreeMap<String, PathBuf> {
    let mut files = BTreeMap::new();
    for dir in dirs {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".desktop") {
                files.insert(name, entry.path());
            }
        }
    }
    files
}

/// The login items: the user's files over the system's, minus those meant for other desktops, by name.
pub fn list(user: &Path, system: &[PathBuf], desktop: &str) -> Vec<Item> {
    let mut items: Vec<Item> = files(system.iter().rev().map(PathBuf::as_path).chain([user]))
        .into_iter()
        .filter_map(|(file, path)| {
            let text = std::fs::read_to_string(&path).ok()?;
            let fields = fields(&text);
            shows_here(&fields, desktop).then(|| Item {
                name: fields.get("Name").map_or_else(|| file.trim_end_matches(".desktop").to_owned(), |n| (*n).to_owned()),
                on: fields.get("Hidden") != Some(&"true"),
                removable: !system.iter().any(|d| d.join(&file).exists()),
                file,
            })
        })
        .collect();
    items.sort_by_key(|i| i.name.to_lowercase());
    items
}

/// The entry with `Hidden=true` added (`hidden`) or taken out.
fn with_hidden(text: &str, hidden: bool) -> String {
    let mut out = String::new();
    for line in text.lines().filter(|l| !l.trim_start().starts_with("Hidden=")) {
        out.push_str(line);
        out.push('\n');
        if hidden && line.trim() == "[Desktop Entry]" {
            out.push_str("Hidden=true\n");
        }
    }
    out
}

/// Turns an item on or off in the user's copy; on, a copy that now matches the system's file goes.
pub fn set_on(user: &Path, system: &[PathBuf], file: &str, on: bool) -> io::Result<()> {
    let own = user.join(file);
    let theirs = system.iter().map(|d| d.join(file)).find(|p| p.exists());
    let text = std::fs::read_to_string(if own.exists() { &own } else { theirs.as_ref().ok_or(io::ErrorKind::NotFound)? })?;
    let text = with_hidden(&text, !on);
    if on
        && let Some(theirs) = theirs
        && std::fs::read_to_string(theirs).is_ok_and(|t| with_hidden(&t, false) == text)
    {
        return std::fs::remove_file(own).or_else(|e| if e.kind() == io::ErrorKind::NotFound { Ok(()) } else { Err(e) });
    }
    std::fs::create_dir_all(user)?;
    std::fs::write(own, text)
}

/// "+": the app's own file, copied in.
pub fn add(user: &Path, app: &Path) -> io::Result<()> {
    std::fs::create_dir_all(user)?;
    std::fs::copy(app, user.join(app.file_name().ok_or(io::ErrorKind::InvalidInput)?)).map(drop)
}

/// "−": the user's file.
pub fn remove(user: &Path, file: &str) -> io::Result<()> {
    std::fs::remove_file(user.join(file))
}

/// The apps a launcher shows, by name, from `applications/` in the data dirs.
/// ponytail: top-level files only, no subdirectory ids; walk them if an app is missing.
fn installed_in(dirs: &[PathBuf], desktop: &str) -> Vec<Installed> {
    let mut apps: Vec<Installed> = files(dirs.iter().rev().map(PathBuf::as_path))
        .into_values()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let fields = fields(&text);
            let flag = |key: &str| fields.get(key) == Some(&"true");
            let shown = fields.get("Type") == Some(&"Application") && !flag("NoDisplay") && !flag("Hidden") && shows_here(&fields, desktop);
            Some(Installed { name: (*fields.get("Name").filter(|_| shown)?).to_owned(), path })
        })
        .collect();
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

/// The items and the installed apps, for the page.
pub fn load() -> (Vec<Item>, Vec<Installed>) {
    let desktop = desktop();
    let system = std::env::var("XDG_DATA_DIRS").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    let data: Vec<PathBuf> =
        std::iter::once(env_dir("XDG_DATA_HOME", ".local/share")).chain(system.split(':').filter(|d| !d.is_empty()).map(PathBuf::from)).collect();
    let apps: Vec<PathBuf> = data.iter().map(|d| d.join("applications")).collect();
    (list(&user_dir(), &system_dirs(), &desktop), installed_in(&apps, &desktop))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_items() {
        let root = std::env::temp_dir().join(format!("noobsbot-autostart-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (user, system, apps) = (root.join("user"), vec![root.join("system")], root.join("apps"));
        for dir in [&system[0], &apps] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let write = |path: PathBuf, text: &str| std::fs::write(path, text).unwrap();
        write(system[0].join("alpha.desktop"), "[Desktop Entry]\nType=Application\nName=Alpha\nExec=alpha\n");
        write(system[0].join("other.desktop"), "[Desktop Entry]\nName=Other\nOnlyShowIn=Other;\n");
        write(apps.join("beta.desktop"), "[Desktop Entry]\nType=Application\nName=Beta\nExec=beta\n");
        write(apps.join("hidden.desktop"), "[Desktop Entry]\nType=Application\nName=Hidden\nNoDisplay=true\n");
        let desktop = "noobsbot-desktop";
        let alpha = |on| Item { file: "alpha.desktop".into(), name: "Alpha".into(), on, removable: false };
        assert_eq!(list(&user, &system, desktop), [alpha(true)]);

        // Off: a user copy with Hidden=true; on again: the copy goes.
        set_on(&user, &system, "alpha.desktop", false).unwrap();
        assert!(std::fs::read_to_string(user.join("alpha.desktop")).unwrap().starts_with("[Desktop Entry]\nHidden=true\n"));
        assert_eq!(list(&user, &system, desktop), [alpha(false)]);
        set_on(&user, &system, "alpha.desktop", true).unwrap();
        assert!(!user.join("alpha.desktop").exists());

        let installed = installed_in(std::slice::from_ref(&apps), desktop);
        assert_eq!(installed.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["Beta"]);
        add(&user, &installed[0].path).unwrap();
        let beta = Item { file: "beta.desktop".into(), name: "Beta".into(), on: true, removable: true };
        assert_eq!(list(&user, &system, desktop), [alpha(true), beta.clone()]);
        set_on(&user, &system, "beta.desktop", false).unwrap();
        assert!(!list(&user, &system, desktop)[1].on);
        remove(&user, "beta.desktop").unwrap();
        assert_eq!(list(&user, &system, desktop), [alpha(true)]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
