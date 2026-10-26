//! Keyboard ▸ Input Sources and Modifier Keys: the xkb layouts the system knows, and `[keyboard]`'s comma lists.

use std::sync::LazyLock;

/// A layout or one of its variants, with its name as xkb describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub layout: String,
    pub variant: String,
    pub name: String,
}

/// Every layout and variant in the xkb rules list (read on first use; empty without xkeyboard-config).
pub static ALL: LazyLock<Vec<Source>> =
    LazyLock::new(|| std::fs::read_to_string("/usr/share/X11/xkb/rules/base.lst").map(|t| parse_lst(&t)).unwrap_or_default());

/// The `! layout` and `! variant` sections of `base.lst`: `  us  English (US)`, `  intl  us: English (US, intl.)`.
fn parse_lst(text: &str) -> Vec<Source> {
    let mut section = "";
    let mut all = Vec::new();
    for line in text.lines() {
        if let Some(name) = line.strip_prefix('!') {
            section = name.trim();
            continue;
        }
        let Some((code, rest)) = line.trim().split_once(char::is_whitespace) else { continue };
        let rest = rest.trim();
        let (layout, variant, name) = match section {
            "layout" => (code, "", rest),
            "variant" => match rest.split_once(": ") {
                Some((layout, name)) => (layout, code, name),
                None => continue,
            },
            _ => continue,
        };
        all.push(Source { layout: layout.into(), variant: variant.into(), name: name.into() });
    }
    all
}

/// The chosen sources from `layout = "us,np"` and `variant = "intl,"`.
pub fn chosen(layouts: &str, variants: &str) -> Vec<(String, String)> {
    let mut variants = variants.split(',');
    layouts.split(',').map(str::trim).filter(|l| !l.is_empty()).map(|l| (l.to_owned(), variants.next().unwrap_or("").trim().to_owned())).collect()
}

/// `chosen`'s inverse: the layout and variant lists (variant empty when no source has one).
pub fn lists(sources: &[(String, String)]) -> (String, String) {
    let layouts = sources.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>().join(",");
    let variants =
        if sources.iter().all(|(_, v)| v.is_empty()) { String::new() } else { sources.iter().map(|(_, v)| v.as_str()).collect::<Vec<_>>().join(",") };
    (layouts, variants)
}

/// `sources` with entry `i` swapped with its neighbour `to` (the ↑ / ↓ buttons).
pub fn moved(sources: &[(String, String)], i: usize, to: usize) -> Vec<(String, String)> {
    let mut list = sources.to_vec();
    list.swap(i, to);
    list
}

/// A source's name, or its codes when the rules list doesn't have it.
pub fn name(layout: &str, variant: &str) -> String {
    ALL.iter()
        .find(|s| s.layout == layout && s.variant == variant)
        .map_or_else(|| if variant.is_empty() { layout.to_owned() } else { format!("{layout} ({variant})") }, |s| s.name.clone())
}

/// Caps Lock's dropdown: the option written, the label shown.
pub const CAPS: &[(&str, &str)] = &[("", "Caps Lock"), ("ctrl:nocaps", "Control"), ("caps:escape", "Escape"), ("caps:none", "No Action")];
pub const COMPOSE: &[(&str, &str)] = &[
    ("", "None"),
    ("compose:ralt", "Right Alt"),
    ("compose:rctrl", "Right Ctrl"),
    ("compose:rwin", "Right Super"),
    ("compose:menu", "Menu"),
    ("compose:caps", "Caps Lock"),
];

/// What Caps Lock does (any other choice from the same family replaces it).
pub fn is_caps(option: &str) -> bool {
    option.starts_with("caps:") || ["ctrl:nocaps", "ctrl:swapcaps"].contains(&option)
}

pub fn is_compose(option: &str) -> bool {
    option.starts_with("compose:")
}

/// The option of `family` in `options`, or "" for none.
pub fn current(options: &str, family: fn(&str) -> bool) -> &str {
    options.split(',').map(str::trim).find(|o| family(o)).unwrap_or("")
}

/// `options` with `family`'s option replaced by `new` ("" removes it), the others kept in order.
pub fn with_option(options: &str, family: fn(&str) -> bool, new: &str) -> String {
    let kept = options.split(',').map(str::trim).filter(|o| !o.is_empty() && !family(o));
    kept.chain((!new.is_empty()).then_some(new)).collect::<Vec<_>>().join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_list() {
        let text = "! model\n  pc105           Generic 105-key PC\n\n! layout\n  us              English (US)\n  np              Nepali\n\n! variant\n  intl            us: English (US, intl., with dead keys)\n\n! option\n  grp                  Switching to another layout\n  caps:escape          Make Caps Lock an additional Esc\n";
        let all = parse_lst(text);
        let codes: Vec<_> = all.iter().map(|s| (s.layout.as_str(), s.variant.as_str(), s.name.as_str())).collect();
        assert_eq!(codes, [("us", "", "English (US)"), ("np", "", "Nepali"), ("us", "intl", "English (US, intl., with dead keys)")]);
    }

    #[test]
    fn layout_lists() {
        let sources = chosen("us, np,de", "intl,");
        assert_eq!(sources, [("us".into(), "intl".into()), ("np".into(), String::new()), ("de".into(), String::new())]);
        assert_eq!(lists(&sources), ("us,np,de".into(), "intl,,".into()));
        assert_eq!(lists(&chosen("us,np", "")), ("us,np".into(), String::new()));
        assert!(chosen("", "").is_empty());
    }

    #[test]
    fn reorder_moves_the_variant_along() {
        let sources = chosen("us,np,de", "intl,,");
        assert_eq!(lists(&moved(&sources, 0, 1)), ("np,us,de".into(), ",intl,".into()));
        assert_eq!(lists(&moved(&sources, 2, 1)), ("us,de,np".into(), "intl,,".into()));
        assert_eq!(lists(&moved(&chosen("us,np", ""), 1, 0)), ("np,us".into(), String::new()));
    }
    #[test]
    fn options() {
        let options = "grp:alt_shift_toggle, caps:swapescape";
        assert_eq!(current(options, is_caps), "caps:swapescape");
        assert_eq!(with_option(options, is_caps, "ctrl:nocaps"), "grp:alt_shift_toggle,ctrl:nocaps");
        assert_eq!(with_option(options, is_caps, ""), "grp:alt_shift_toggle");
        assert_eq!(with_option("", is_compose, "compose:ralt"), "compose:ralt");
        assert_eq!(current(options, is_compose), "");
    }
}
