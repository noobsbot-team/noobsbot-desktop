//! `$XDG_RUNTIME_DIR/noobsbot-desktop/displays.json` (E6.2): the connected displays as the desktop sees them. The
//! desktop rewrites it whenever they change; System Settings watches it and writes `[[display.outputs]]` in
//! `config.toml` back. Shared by both crates (`#[path]` in settings), so the two ends always agree.

use std::{path::PathBuf, time::Duration};

use serde::{Deserialize, Serialize};

/// How long "Keep these display settings?" waits before the desktop puts the old ones back.
pub const KEEP_FOR: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Display {
    /// The connector (`eDP-1`, `HDMI-A-1`; `winit` when nested): the key of its `[[display.outputs]]` entry.
    pub name: String,
    /// From the EDID; empty when unknown.
    pub make: String,
    pub model: String,
    pub width_mm: u32,
    pub height_mm: u32,
    /// A laptop's own panel (eDP, LVDS, DSI).
    pub built_in: bool,
    /// Every mode it offers, as the display lists them.
    pub modes: Vec<Mode>,
    /// The nested window: only scale and rotation apply.
    pub nested: bool,
    pub enabled: bool,
    /// The mode it is lit with (or would be, when off).
    pub mode: Mode,
    pub scale: f64,
    /// The scale `0` (auto) picks for it.
    pub auto_scale: f64,
    /// Degrees clockwise.
    pub rotation: u16,
    /// Logical px: where it is and how big, after scale and rotation.
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub primary: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Mode {
    pub width: i32,
    pub height: i32,
    /// mHz.
    pub refresh: i32,
    pub preferred: bool,
}

/// `$XDG_RUNTIME_DIR/noobsbot-desktop/displays.json`; `None` without a runtime dir.
pub fn path() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
    dir.is_absolute().then(|| dir.join("noobsbot-desktop").join("displays.json"))
}

/// `$XDG_RUNTIME_DIR/noobsbot-desktop/display.previous`: the `[display]` section from before a display change that
/// is not kept yet. System Settings writes it before the change and deletes it on "Keep Changes"; the desktop puts
/// it back into `config.toml` `KEEP_FOR` after the change (or at its next start), so a bad mode never sticks even
/// when Settings is gone by then.
pub fn previous_path() -> Option<PathBuf> {
    Some(path()?.with_file_name("display.previous"))
}

/// A laptop's own panel, by its connector name.
pub fn built_in(name: &str) -> bool {
    ["eDP", "LVDS", "DSI"].iter().any(|p| name.starts_with(p))
}
