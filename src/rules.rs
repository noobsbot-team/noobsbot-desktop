//! Window rules (E1.9): `[[rules]]` entries in config.toml, matched by Wayland `app_id` and an optional title pattern.
//! wm.rs calls [`compile`] once per (re)load and [`resolve`] when a window appears and again when its title changes.

use regex_lite::Regex;
use serde::Deserialize;

/// Every key a rule understands; config.rs warns about the rest.
pub const RULE_KEYS: &[&str] = &["app_id", "title", "space", "float_above", "size", "position", "fullscreen", "decorations", "layer", "opacity"];

/// One `[[rules]]` entry as written in config.toml.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Rule {
    /// Exact Wayland `app_id`.
    pub app_id: Option<String>,
    /// Regular expression searched anywhere in the title (anchor it with `^…$` for an exact match).
    pub title: Option<String>,
    #[serde(flatten)]
    pub actions: Actions,
}

/// What a rule does. Unset fields leave the window's normal behaviour alone.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Actions {
    /// `"all"` or a Space number (1 = first).
    pub space: Option<Space>,
    /// Always on top of normal windows (picture-in-picture).
    pub float_above: Option<bool>,
    /// `[width, height]`: logical px, or `"40%"` of the usable area.
    pub size: Option<[Length; 2]>,
    /// `[x, y]` of the top-left corner inside the usable area: logical px, or a percentage.
    pub position: Option<[Length; 2]>,
    pub fullscreen: Option<bool>,
    /// `false` = no title bar or traffic lights.
    pub decorations: Option<bool>,
    pub layer: Option<Layer>,
    /// 0.0 … 1.0, multiplied into the window's alpha.
    pub opacity: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Space {
    All(AllSpaces),
    Number(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AllSpaces {
    All,
}

/// `normal` (the default stacking) or `overlay`: above fullscreen apps, below the dock and menu bar (the quake terminal).
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    Normal,
    Overlay,
}

/// A size or position component.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(try_from = "RawLength")]
pub enum Length {
    Px(i32),
    /// A fraction of the usable area, 0.0 … 1.0.
    Fraction(f32),
}

impl Length {
    /// Resolves against the usable area's extent on the same axis.
    pub fn to_px(self, extent: i32) -> i32 {
        match self {
            Length::Px(px) => px,
            Length::Fraction(f) => (extent as f32 * f).round() as i32,
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawLength {
    Px(i32),
    Text(String),
}

impl TryFrom<RawLength> for Length {
    type Error = String;
    fn try_from(raw: RawLength) -> Result<Self, String> {
        match raw {
            RawLength::Px(px) => Ok(Length::Px(px)),
            RawLength::Text(text) => match text.strip_suffix('%').and_then(|n| n.trim().parse::<f32>().ok()) {
                Some(pct) if (0.0..=100.0).contains(&pct) => Ok(Length::Fraction(pct / 100.0)),
                _ => Err(format!("{text:?} is not a pixel count or a percentage like \"40%\"")),
            },
        }
    }
}

/// A rule with its title pattern compiled.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    app_id: Option<String>,
    title: Option<Regex>,
    actions: Actions,
}

/// Compiles every rule once. A rule with an invalid pattern, or with nothing to match on, is skipped with a warning.
pub fn compile(rules: &[Rule]) -> Vec<CompiledRule> {
    rules
        .iter()
        .enumerate()
        .filter_map(|(i, rule)| {
            if rule.app_id.is_none() && rule.title.is_none() {
                tracing::warn!("window rule {}: needs app_id or title; ignored", i + 1);
                return None;
            }
            let title = match rule.title.as_deref().map(Regex::new).transpose() {
                Ok(title) => title,
                Err(err) => {
                    tracing::warn!("window rule {}: bad title pattern: {err}; ignored", i + 1);
                    return None;
                }
            };
            let mut actions = rule.actions.clone();
            actions.opacity = actions.opacity.map(|o| o.clamp(0.0, 1.0));
            Some(CompiledRule { app_id: rule.app_id.clone(), title, actions })
        })
        .collect()
}

/// The effective settings for a window: every matching rule in file order, later rules overriding earlier ones field by field.
pub fn resolve(rules: &[CompiledRule], app_id: &str, title: &str) -> Actions {
    let mut out = Actions::default();
    for rule in rules {
        if rule.app_id.as_deref().is_some_and(|id| id != app_id) || rule.title.as_ref().is_some_and(|re| !re.is_match(title)) {
            continue;
        }
        let a = &rule.actions;
        out.space = a.space.or(out.space);
        out.float_above = a.float_above.or(out.float_above);
        out.size = a.size.or(out.size);
        out.position = a.position.or(out.position);
        out.fullscreen = a.fullscreen.or(out.fullscreen);
        out.decorations = a.decorations.or(out.decorations);
        out.layer = a.layer.or(out.layer);
        out.opacity = a.opacity.or(out.opacity);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(text: &str) -> Vec<CompiledRule> {
        #[derive(Deserialize)]
        struct File {
            rules: Vec<Rule>,
        }
        compile(&toml::from_str::<File>(text).unwrap().rules)
    }

    #[test]
    fn matching() {
        let r = rules(
            r#"
            [[rules]]
            app_id = "noobsbot"
            space = "all"
            size = ["100%", "40%"]
            position = [0, 0]
            layer = "overlay"
            decorations = false

            [[rules]]
            app_id = "brave-browser"
            title = "^Picture in picture$"
            float_above = true

            [[rules]]
            title = "(unclosed"
            opacity = 0.5

            [[rules]]
            title = "Private"
            space = 2
            opacity = 7.0
            "#,
        );
        assert_eq!(r.len(), 3, "the invalid pattern is ignored");

        let quake = resolve(&r, "noobsbot", "zsh");
        assert_eq!(quake.layer, Some(Layer::Overlay));
        assert_eq!(quake.space, Some(Space::All(AllSpaces::All)));
        assert_eq!(quake.size.map(|[w, h]| (w.to_px(1920), h.to_px(1000))), Some((1920, 400)));
        assert_eq!(quake.float_above, None);

        assert_eq!(resolve(&r, "brave-browser", "Picture in picture").float_above, Some(true));
        assert_eq!(resolve(&r, "brave-browser", "Picture in picture - YouTube").float_above, None);
        assert_eq!(resolve(&r, "noobsbot-other", "zsh"), Actions::default(), "app_id is exact");

        let private = resolve(&r, "brave-browser", "New Private Tab");
        assert_eq!((private.space, private.opacity), (Some(Space::Number(2)), Some(1.0)));
    }

    #[test]
    fn bad_length_is_an_error() {
        assert!(toml::from_str::<Rule>(r#"size = ["wide", 10]"#).is_err());
    }
}
