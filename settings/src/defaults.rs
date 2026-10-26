//! Desktop & Dock ▸ Default Apps: `mimeapps.list`'s `[Default Applications]`, which `xdg-open` (and the
//! `default-app` keys) follow. Only that section's keys for the chosen kind change; every other line stays.

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
};

use crate::autostart::{self, Installed};

const SECTION: &str = "[Default Applications]";

/// Each row: its label and the MIME types a choice is written for (the first one shows the current default).
pub const KINDS: &[(&str, &[&str])] = &[
    ("Web browser", &["x-scheme-handler/https", "x-scheme-handler/http", "text/html", "application/xhtml+xml"]),
    ("Mail", &["x-scheme-handler/mailto", "message/rfc822"]),
    ("Files", &["inode/directory"]),
    ("Text editor", &["text/plain", "text/markdown", "application/x-shellscript"]),
    ("Images", &["image/png", "image/jpeg", "image/gif", "image/webp", "image/bmp", "image/tiff", "image/svg+xml"]),
    ("Video", &["video/mp4", "video/x-matroska", "video/webm", "video/mpeg", "video/quicktime", "video/x-msvideo"]),
    ("Music", &["audio/mpeg", "audio/flac", "audio/ogg", "audio/x-vorbis+ogg", "audio/x-wav", "audio/mp4", "audio/aac"]),
    ("PDF", &["application/pdf"]),
];

/// The apps whose `MimeType` lists any of `mimes`.
pub fn handlers(dirs: &[PathBuf], desktop: &str, mimes: &[&str]) -> Vec<Installed> {
    autostart::installed_in(dirs, desktop, |fields: &HashMap<&str, &str>| {
        fields.get("MimeType").is_some_and(|types| types.split(';').any(|t| mimes.contains(&t.trim())))
    })
}

/// The desktop-file id written to `mimeapps.list`: the file name.
pub fn id(app: &Installed) -> String {
    app.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Each kind's current default (a desktop-file id) and the apps that can be it; then the terminal emulators.
pub fn load() -> (Vec<(Option<String>, Vec<Installed>)>, Vec<Installed>) {
    let (dirs, desktop) = (autostart::app_dirs(), autostart::desktop());
    let kinds = KINDS.iter().map(|(_, mimes)| (current(mimes[0]), handlers(&dirs, &desktop, mimes))).collect();
    let terminals = autostart::installed_in(&dirs, &desktop, |f| f.get("Categories").is_some_and(|c| c.split(';').any(|c| c == "TerminalEmulator")));
    (kinds, terminals)
}

/// The command an app's `Exec` runs, its field codes (`%U`, …) dropped: what the `terminal` key holds.
pub fn command(app: &Path) -> Option<String> {
    let text = std::fs::read_to_string(app).ok()?;
    let exec = autostart::fields(&text).get("Exec")?.split_whitespace().filter(|w| !w.starts_with('%')).collect::<Vec<_>>().join(" ");
    (!exec.is_empty()).then_some(exec)
}

/// The default for `mime` in one `mimeapps.list`: the first desktop-file id listed.
pub fn default_in(text: &str, mime: &str) -> Option<String> {
    let mut inside = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            inside = line == SECTION;
        } else if inside
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == mime
        {
            return value.split(';').map(str::trim).find(|id| !id.is_empty()).map(String::from);
        }
    }
    None
}

/// `text` with `id` the default for every one of `mimes`: their old lines in the section go, the new ones follow its
/// header (added at the end when there is none).
pub fn with_default(text: &str, mimes: &[&str], id: &str) -> String {
    let new: String = mimes.iter().map(|m| format!("{m}={id};\n")).collect();
    let mut out = String::new();
    let (mut inside, mut found) = (false, false);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            inside = trimmed == SECTION;
        } else if inside && trimmed.split_once('=').is_some_and(|(key, _)| mimes.contains(&key.trim())) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
        if inside && !found {
            found = true;
            out.push_str(&new);
        }
    }
    if !found {
        if !out.is_empty() && !out.ends_with("\n\n") {
            out.push('\n');
        }
        out.push_str(SECTION);
        out.push('\n');
        out.push_str(&new);
    }
    out
}

/// `~/.config/mimeapps.list`, where a choice is written.
pub fn user_list() -> PathBuf {
    autostart::env_dir("XDG_CONFIG_HOME", ".config").join("mimeapps.list")
}

/// Every `mimeapps.list` read for the current default, most important first: the user's, `$XDG_CONFIG_DIRS`',
/// then the data dirs' `applications/` (`/usr/share/applications/mimeapps.list`).
/// ponytail: no `$desktop-mimeapps.list` and no `mimeinfo.cache`; read them if a default shows wrong.
fn lists() -> Vec<PathBuf> {
    let split = |var: &str, fallback: &str| std::env::var(var).ok().filter(|d| !d.is_empty()).unwrap_or_else(|| fallback.into());
    let config = split("XDG_CONFIG_DIRS", "/etc/xdg");
    let data = split("XDG_DATA_DIRS", "/usr/local/share:/usr/share");
    std::iter::once(user_list())
        .chain(config.split(':').filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).join("mimeapps.list")))
        .chain(
            std::iter::once(autostart::env_dir("XDG_DATA_HOME", ".local/share"))
                .chain(data.split(':').filter(|d| !d.is_empty()).map(PathBuf::from))
                .map(|d| d.join("applications/mimeapps.list")),
        )
        .collect()
}

/// The default for `mime`, from the first list that names one.
pub fn current(mime: &str) -> Option<String> {
    lists().into_iter().find_map(|p| std::fs::read_to_string(p).ok().and_then(|text| default_in(&text, mime)))
}

/// Makes `id` (`firefox.desktop`) the user's default for `mimes`.
pub fn set(mimes: &[&str], id: &str) -> io::Result<()> {
    let path = user_list();
    // Through a dotfiles symlink, like config.toml.
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    let text = match std::fs::read_to_string(&path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        read => read?,
    };
    crate::config::save_atomic(&path, &with_default(&text, mimes, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_default_section_only() {
        let text = "[Added Associations]\ntext/html=a.desktop;\n\n[Default Applications]\n# browser\ntext/html= ;b.desktop;c.desktop;\n";
        assert_eq!(default_in(text, "text/html").as_deref(), Some("b.desktop"));
        assert_eq!(default_in(text, "text/plain"), None);
    }

    #[test]
    fn writes_only_the_chosen_keys() {
        let text = "# mine\n[Added Associations]\ntext/html=a.desktop;\n\n[Default Applications]\ntext/html=old.desktop;\n# keep\ntext/plain=kate.desktop;\nx-scheme-handler/http=old.desktop;\n\n[Removed Associations]\nimage/png=x.desktop;\n";
        let out = with_default(text, &["text/html", "x-scheme-handler/http"], "new.desktop");
        assert_eq!(
            out,
            "# mine\n[Added Associations]\ntext/html=a.desktop;\n\n[Default Applications]\ntext/html=new.desktop;\nx-scheme-handler/http=new.desktop;\n# keep\ntext/plain=kate.desktop;\n\n[Removed Associations]\nimage/png=x.desktop;\n"
        );
        assert_eq!(with_default(&out, &["text/html", "x-scheme-handler/http"], "new.desktop"), out);
    }

    #[test]
    fn handlers_of_a_kind() {
        let dir = std::env::temp_dir().join(format!("noobsbot-defaults-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let app = |file: &str, name: &str, extra: &str| {
            std::fs::write(dir.join(file), format!("[Desktop Entry]\nType=Application\nName={name}\nExec={name} %U\n{extra}")).unwrap()
        };
        app("web.desktop", "Web", "MimeType=text/html;x-scheme-handler/https;\n");
        app("edit.desktop", "Edit", "MimeType=text/plain;\n");
        app("hidden.desktop", "Hidden", "MimeType=text/html;\nNoDisplay=true\n");
        app("term.desktop", "Term", "Categories=System;TerminalEmulator;\n");
        let names = |mimes| handlers(std::slice::from_ref(&dir), "noobsbot-desktop", mimes).into_iter().map(|a| a.name).collect::<Vec<_>>();
        assert_eq!(names(KINDS[0].1), ["Web"]);
        assert_eq!(names(KINDS[3].1), ["Edit"]);
        assert!(names(KINDS[2].1).is_empty());
        assert_eq!(command(&dir.join("term.desktop")).as_deref(), Some("Term"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn adds_the_section_when_missing() {
        assert_eq!(with_default("", &["inode/directory"], "files.desktop"), "[Default Applications]\ninode/directory=files.desktop;\n");
        assert_eq!(
            with_default("[Added Associations]\nimage/png=a.desktop;\n", &["image/png"], "b.desktop"),
            "[Added Associations]\nimage/png=a.desktop;\n\n[Default Applications]\nimage/png=b.desktop;\n"
        );
    }
}
