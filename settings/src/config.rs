//! `config.toml` (C11) as the Settings app sees it: the user's file with every comment kept, over the shipped
//! defaults. A change is written atomically and the desktop's live reload applies it.

use std::{fs, io, io::Write, path::Path, path::PathBuf};

use toml_edit::{DocumentMut, Item, Value};

/// The commented defaults the desktop ships; also what a missing file starts from.
pub const DEFAULT_TOML: &str = include_str!("../../data/config.toml");

pub struct Config {
    path: PathBuf,
    doc: DocumentMut,
    defaults: DocumentMut,
}

/// `$XDG_CONFIG_HOME/noobsbot-desktop/config.toml`, else `~/.config/…`, the same rule as the desktop.
pub fn path() -> PathBuf {
    let home = || PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config");
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(home);
    let path = base.join("noobsbot-desktop").join("config.toml");
    // Through a dotfiles symlink, so the rename replaces the real file and the desktop's watch sees it.
    fs::canonicalize(&path).unwrap_or(path)
}

impl Config {
    /// The file at `path`; a missing or unparsable one reads as the defaults (a broken file is never overwritten
    /// until the user changes something, and then only from the defaults plus that change).
    pub fn load(path: PathBuf) -> (Config, Option<String>) {
        let defaults: DocumentMut = DEFAULT_TOML.parse().expect("shipped config parses");
        let (doc, error) = match fs::read_to_string(&path) {
            Ok(text) => match text.parse::<DocumentMut>() {
                Ok(doc) => (doc, None),
                Err(err) => (defaults.clone(), Some(format!("{}: {err}", path.display()))),
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => (defaults.clone(), None),
            Err(err) => (defaults.clone(), Some(format!("{}: {err}", path.display()))),
        };
        (Config { path, doc, defaults }, error)
    }

    /// `table.key` (or a top-level `key`): the user's value, else the default.
    pub fn get(&self, key: &str) -> Option<&Value> {
        lookup(&self.doc, key).or_else(|| lookup(&self.defaults, key))
    }

    pub fn bool(&self, key: &str) -> bool {
        self.get(key).and_then(Value::as_bool).unwrap_or(false)
    }

    pub fn str(&self, key: &str) -> &str {
        self.get(key).and_then(Value::as_str).unwrap_or("")
    }

    pub fn num(&self, key: &str) -> f64 {
        self.get(key).and_then(|v| v.as_float().or(v.as_integer().map(|i| i as f64))).unwrap_or(0.0)
    }

    /// Every shortcut: the defaults with the user's `[bindings]` merged over them, as the desktop reads them.
    pub fn bindings(&self) -> Vec<(String, String)> {
        let mut all = std::collections::BTreeMap::new();
        for doc in [&self.defaults, &self.doc] {
            if let Some(table) = doc.get("bindings").and_then(Item::as_table_like) {
                all.extend(table.iter().filter_map(|(k, v)| Some((k.to_owned(), v.as_str()?.to_owned()))));
            }
        }
        all.into_iter().collect()
    }

    /// Sets `key` and saves. The comment and position of an existing line stay.
    pub fn set(&mut self, key: &str, value: impl Into<Value>) -> io::Result<()> {
        let (table, name) = key.rsplit_once('.').map_or((None, key), |(t, k)| (Some(t), k));
        let parent = match table {
            Some(table) => {
                let item = self.doc.entry(table).or_insert(toml_edit::table());
                item.as_table_like_mut().ok_or_else(|| io::Error::other(format!("[{table}] is not a table")))?
            }
            None => self.doc.as_table_mut() as &mut dyn toml_edit::TableLike,
        };
        match parent.get_mut(name).and_then(Item::as_value_mut) {
            // Keeps the old value's comments and spacing.
            Some(old) => {
                let decor = old.decor().clone();
                *old = value.into();
                *old.decor_mut() = decor;
            }
            None => {
                parent.insert(name, Item::Value(value.into()));
            }
        }
        save_atomic(&self.path, &self.doc.to_string())
    }

    /// The `[[display.outputs]]` entry for the display `name`, if the file has one.
    pub fn output(&self, name: &str) -> Option<&toml_edit::Table> {
        self.doc.get("display")?.get("outputs")?.as_array_of_tables()?.iter().find(|t| t.get("name").and_then(Item::as_str) == Some(name))
    }

    /// Sets `(display, key, value)` in each display's `[[display.outputs]]` entry (made when missing) and saves once.
    pub fn set_outputs(&mut self, changes: Vec<(String, &str, Value)>) -> io::Result<()> {
        let display = self.doc.entry("display").or_insert(toml_edit::table());
        let display = display.as_table_mut().ok_or_else(|| io::Error::other("[display] is not a table"))?;
        let outputs = display.entry("outputs").or_insert(Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
        let outputs = outputs.as_array_of_tables_mut().ok_or_else(|| io::Error::other("display.outputs is not [[display.outputs]]"))?;
        for (name, key, value) in changes {
            let at = outputs.iter().position(|t| t.get("name").and_then(Item::as_str) == Some(&name));
            let entry = match at {
                Some(i) => outputs.get_mut(i).expect("found above"),
                None => {
                    let mut table = toml_edit::Table::new();
                    table.insert("name", toml_edit::value(name));
                    outputs.push(table);
                    outputs.get_mut(outputs.len() - 1).expect("just pushed")
                }
            };
            entry.insert(key, Item::Value(value));
        }
        save_atomic(&self.path, &self.doc.to_string())
    }

    /// The whole file as it stands, for `restore`.
    pub fn text(&self) -> String {
        self.doc.to_string()
    }

    /// Writes back a `text` taken earlier (the display settings' "Revert").
    pub fn restore(&mut self, text: String) -> io::Result<()> {
        self.doc = text.parse().map_err(io::Error::other)?;
        save_atomic(&self.path, &text)
    }
}

/// A `()` each time the file at `path` is written, by us or elsewhere (`config.toml`: the Control Center's Dark
/// toggle, an editor; `displays.json`: the desktop): inotify on its folder through `notify`, as the desktop's own
/// `config::watch`; no polling. Bursts coalesce in the one-slot channel: whatever is queued is read after the last
/// write.
#[expect(clippy::ptr_arg, reason = "Subscription::run_with hands its data over as &PathBuf")]
pub fn changes(path: &PathBuf) -> impl iced::futures::Stream<Item = ()> + use<> {
    let path = path.clone();
    iced::stream::channel(1, async move |tx: iced::futures::channel::mpsc::Sender<()>| {
        let watcher = (|| {
            let dir = path.parent()?.to_owned();
            let name = path.file_name()?.to_owned();
            let mut tx = tx.clone();
            let _ = fs::create_dir_all(&dir);
            let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if event.is_ok_and(|e| written(&e, &name)) {
                    let _ = tx.try_send(());
                }
            })
            .inspect_err(|err| eprintln!("config watch off: {err}"))
            .ok()?;
            notify::Watcher::watch(&mut watcher, &dir, notify::RecursiveMode::NonRecursive)
                .inspect_err(|err| eprintln!("config watch off: {}: {err}", dir.display()))
                .ok()?;
            Some(watcher)
        })();
        // The watcher lives as long as the subscription.
        std::future::pending::<()>().await;
        drop(watcher);
    })
}

/// A write, rename or delete of the file `name`; reads (ours included) don't count, or every reload would loop.
fn written(event: &notify::Event, name: &std::ffi::OsStr) -> bool {
    use notify::{
        EventKind,
        event::{AccessKind, AccessMode},
    };
    let write = matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_))
        || matches!(event.kind, EventKind::Access(AccessKind::Close(AccessMode::Write)));
    write && event.paths.iter().any(|p| p.file_name() == Some(name))
}

fn lookup<'a>(doc: &'a DocumentMut, key: &str) -> Option<&'a Value> {
    let (table, name) = key.rsplit_once('.').map_or((None, key), |(t, k)| (Some(t), k));
    let parent = match table {
        Some(table) => doc.get(table)?.as_table_like()?,
        None => doc.as_table(),
    };
    parent.get(name)?.as_value()
}

/// Temp file next to `path`, synced, renamed over it: the desktop only ever reads the old file or the new one.
/// The same steps as the desktop's `config::save_atomic`.
fn save_atomic(path: &Path, contents: &str) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let written = fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(contents.as_bytes())?;
        f.sync_all()
    });
    if let Err(err) = written.and_then(|()| fs::rename(&tmp, path)) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    let _ = fs::File::open(dir).and_then(|d| d.sync_all());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str, text: Option<&str>) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("noobsbot-settings-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        if let Some(text) = text {
            fs::write(&path, text).unwrap();
        }
        path
    }

    #[test]
    fn defaults_under_user_values() {
        let path = temp("read", Some("[appearance]\nmode = \"dark\"\n"));
        let (config, error) = Config::load(path);
        assert!(error.is_none());
        assert_eq!(config.str("appearance.mode"), "dark");
        assert_eq!(config.str("appearance.accent"), "blue");
        assert_eq!(config.num("dock.icon_size"), 84.0);
        assert_eq!(config.str("terminal"), "noobsbot");
        assert!(config.bool("touchpad.tap_to_click"));
    }

    #[test]
    fn set_keeps_comments_and_creates_tables() {
        let path = temp("set", Some("# mine\n[appearance]\n# the mode\nmode = \"light\" # trailing\n"));
        let (mut config, _) = Config::load(path.clone());
        config.set("appearance.mode", "dark").unwrap();
        config.set("dock.zoom", false).unwrap();
        config.set("terminal", "kitty").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("# mine\n"), "{text}");
        assert!(text.contains("# the mode\nmode = \"dark\" # trailing\n"), "{text}");
        let doc: DocumentMut = text.parse().unwrap();
        assert_eq!(doc["dock"]["zoom"].as_bool(), Some(false));
        assert_eq!(doc["terminal"].as_str(), Some("kitty"));
        let (again, _) = Config::load(path);
        assert_eq!(again.str("appearance.mode"), "dark");
    }

    #[test]
    fn missing_file_starts_from_defaults() {
        let path = temp("missing", None);
        let (mut config, error) = Config::load(path.clone());
        assert!(error.is_none());
        config.set("clock.hour24", true).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Noobsbot Desktop settings."));
        assert!(text.contains("hour24 = true"));
    }

    #[test]
    fn only_writes_of_the_file_count() {
        use notify::{
            Event, EventKind,
            event::{AccessKind, AccessMode, CreateKind, ModifyKind, RenameMode},
        };
        let at = |kind, file: &str| Event::new(kind).add_path(PathBuf::from("/c/noobsbot-desktop").join(file));
        let name = std::ffi::OsStr::new("config.toml");
        assert!(written(&at(EventKind::Modify(ModifyKind::Name(RenameMode::To)), "config.toml"), name));
        assert!(written(&at(EventKind::Create(CreateKind::File), "config.toml"), name));
        assert!(written(&at(EventKind::Access(AccessKind::Close(AccessMode::Write)), "config.toml"), name));
        assert!(!written(&at(EventKind::Access(AccessKind::Close(AccessMode::Read)), "config.toml"), name));
        assert!(!written(&at(EventKind::Access(AccessKind::Open(AccessMode::Any)), "config.toml"), name));
        assert!(!written(&at(EventKind::Create(CreateKind::File), ".config.toml.42.tmp"), name));
    }

    #[test]
    fn display_outputs_by_name() {
        let path = temp("outputs", Some(DEFAULT_TOML));
        let (mut config, _) = Config::load(path.clone());
        let before = config.text();
        config
            .set_outputs(vec![("eDP-1".into(), "scale", 1.25.into()), ("HDMI-A-1".into(), "position", Value::Array([1536i64, 0].into_iter().collect()))])
            .unwrap();
        config.set_outputs(vec![("eDP-1".into(), "rotation", 90i64.into())]).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("# One display's own settings"), "comments kept: {text}");
        let doc: DocumentMut = text.parse().unwrap();
        let outputs = doc["display"]["outputs"].as_array_of_tables().unwrap();
        assert_eq!(outputs.len(), 2, "{text}");
        assert_eq!(config.output("eDP-1").unwrap()["scale"].as_float(), Some(1.25));
        assert_eq!(config.output("eDP-1").unwrap()["rotation"].as_integer(), Some(90));
        assert_eq!(config.output("HDMI-A-1").unwrap()["position"].as_array().map(|a| a.len()), Some(2));
        assert!(config.output("DP-1").is_none());
        // Revert: the file as it was.
        config.restore(before.clone()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
        assert!(config.output("eDP-1").is_none());
    }

    #[test]
    fn bindings_merge() {
        let path = temp("bindings", Some("[bindings]\n\"Super+Q\" = \"none\"\n\"F5\" = \"spotlight\"\n"));
        let (config, _) = Config::load(path);
        let bindings = config.bindings();
        let get = |k: &str| bindings.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        assert_eq!(get("Super+Q"), Some("none"));
        assert_eq!(get("F5"), Some("spotlight"));
        assert_eq!(get("Super+Return"), Some("terminal"));
    }
}
