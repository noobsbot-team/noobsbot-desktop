//! Desktop & Dock ▸ Default Apps: `mimeapps.list`'s `[Default Applications]`, which `xdg-open` (and the
//! `default-app` keys) follow. Only that section's keys for the chosen kind change; every other line stays.

use std::{io, path::PathBuf};

const SECTION: &str = "[Default Applications]";

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

fn env_dir(variable: &str, home_relative: &str) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(home_relative))
}

/// `~/.config/mimeapps.list`, where a choice is written.
pub fn user_list() -> PathBuf {
    env_dir("XDG_CONFIG_HOME", ".config").join("mimeapps.list")
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
            std::iter::once(env_dir("XDG_DATA_HOME", ".local/share"))
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
    fn adds_the_section_when_missing() {
        assert_eq!(with_default("", &["inode/directory"], "files.desktop"), "[Default Applications]\ninode/directory=files.desktop;\n");
        assert_eq!(
            with_default("[Added Associations]\nimage/png=a.desktop;\n", &["image/png"], "b.desktop"),
            "[Added Associations]\nimage/png=a.desktop;\n\n[Default Applications]\nimage/png=b.desktop;\n"
        );
    }
}
