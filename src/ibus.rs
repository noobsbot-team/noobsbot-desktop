//! ibus, the other input-method engine (E6.12, `[keyboard] input_method = "ibus"`): its engines read and switched
//! through the `ibus` CLI and its `preload-engines` key. Shared with the Settings app.
// ponytail: shells out to `ibus` and `gsettings` (~20 ms each) instead of ibus's private bus (`ibus address`); fine
// for a read when a menu opens, never on the frame loop.

use std::process::{Command, Stdio};

pub const SCHEMA: &str = "org.freedesktop.ibus.general";

/// `[keyboard] input_method` with "auto" resolved: fcitx5 if installed, else ibus if installed, else "none".
pub fn engine(choice: &str) -> &'static str {
    match choice {
        "ibus" => "ibus",
        "none" => "none",
        "auto" if on_path("fcitx5") => "fcitx5",
        "auto" if on_path("ibus") => "ibus",
        "auto" => "none",
        _ => "fcitx5",
    }
}

pub fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

pub fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Every installed engine as (name, long name); `None` while ibus-daemon isn't running.
pub fn engines() -> Option<Vec<(String, String)>> {
    run("ibus", &["list-engine"]).map(|text| parse_engines(&text))
}

/// The user's engines in switching order (`preload-engines`).
pub fn preloaded() -> Vec<String> {
    run("gsettings", &["get", SCHEMA, "preload-engines"]).map(|text| parse_list(&text)).unwrap_or_default()
}

/// `ibus list-engine`: `language: Chinese` headers, then `  pinyin - Intelligent Pinyin` per engine.
fn parse_engines(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter(|line| line.starts_with(' '))
        .filter_map(|line| line.trim().split_once(" - "))
        .map(|(name, long)| (name.to_owned(), long.to_owned()))
        .collect()
}

/// A gsettings string list: `['xkb:us::eng', 'pinyin']`, or `@as []` when empty.
fn parse_list(text: &str) -> Vec<String> {
    let inner = text.trim().trim_start_matches("@as").trim().trim_start_matches('[').trim_end_matches(']');
    inner.split(',').map(|item| item.trim().trim_matches('\'').to_owned()).filter(|item| !item.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_list_engine() {
        let text =
            "language: English\n  xkb:us::eng - English (US)\nlanguage: Chinese\n  pinyin - Intelligent Pinyin\n  libpinyin - Intelligent Pinyin - Extra\n";
        assert_eq!(
            parse_engines(text),
            [("xkb:us::eng", "English (US)"), ("pinyin", "Intelligent Pinyin"), ("libpinyin", "Intelligent Pinyin - Extra")]
                .map(|(a, b)| (a.to_owned(), b.to_owned()))
        );
    }

    #[test]
    fn parses_gsettings_lists() {
        assert_eq!(parse_list("['xkb:us::eng', 'm17n:ne:rom']\n"), ["xkb:us::eng", "m17n:ne:rom"]);
        assert!(parse_list("@as []\n").is_empty());
    }

    #[test]
    fn engine_choice() {
        assert_eq!((engine("ibus"), engine("none"), engine("fcitx5"), engine("")), ("ibus", "none", "fcitx5", "fcitx5"));
    }
}
