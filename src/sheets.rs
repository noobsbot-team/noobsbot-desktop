//! Sheets and remembered frames (E8.5). A sheet is a modal dialog hung under its parent's title bar: it stacks directly
//! above the parent, moves with it, takes the parent's focus clicks and is dragged by dragging the parent. An app's
//! last window leaves its frame in `$XDG_STATE_HOME/noobsbot-desktop/window-frames`; its next first window opens there.
//! The geometry is in wm.rs.

use smithay::{
    desktop::Window,
    utils::{Logical, Point, Size},
    wayland::{
        compositor::with_states,
        shell::xdg::{SurfaceCachedState, XdgToplevelSurfaceData},
    },
};

use crate::{
    state::State,
    wm::{self, Rect},
};

/// The frame a new window opens at, from the app's last one.
struct Recalled(Rect);

fn frames_path() -> std::path::PathBuf {
    crate::config::xdg_dir("XDG_STATE_HOME", ".local/state").join("noobsbot-desktop/window-frames")
}

impl State {
    /// The window `window` is a sheet of: it has a parent and is modal (xdg-dialog), or fixed-size (older toolkits
    /// that don't speak xdg-dialog).
    pub fn sheet_parent(&self, window: &Window) -> Option<Window> {
        let toplevel = window.toplevel()?;
        let parent = toplevel.parent()?;
        let sheet = with_states(toplevel.wl_surface(), |s| {
            let modal = s.data_map.get::<XdgToplevelSurfaceData>().is_some_and(|d| d.lock().is_ok_and(|d| d.modal));
            let mut cached = s.cached_state.get::<SurfaceCachedState>();
            let c = cached.current();
            modal || (c.min_size == c.max_size && c.min_size.w > 0 && c.min_size.h > 0)
        });
        sheet.then(|| self.window_for(&parent).cloned()).flatten()
    }

    /// The frontmost sheet on `parent`.
    pub fn sheet_of(&self, parent: &Window) -> Option<Window> {
        self.space.elements().rev().find(|w| self.sheet_parent(w).as_ref() == Some(parent)).cloned()
    }

    /// Puts every sheet directly above its parent (E1.2 stacking); part of `restack`.
    pub fn stack_sheets(&mut self) {
        let order: Vec<Window> = self.space.elements().cloned().collect();
        let sorted = wm::sheets_above(&order, |w| self.sheet_parent(w));
        if sorted != order {
            for window in &sorted {
                self.space.raise_element(window, false);
            }
        }
    }

    /// `parent` moved by `delta`: its sheets go with it.
    pub fn carry_sheets(&mut self, parent: &Window, delta: Point<i32, Logical>) {
        let sheets: Vec<Window> = self.space.elements().filter(|w| self.sheet_parent(w).as_ref() == Some(parent)).cloned().collect();
        for sheet in sheets {
            if let Some(loc) = self.space.element_location(&sheet) {
                self.space.map_element(sheet, loc + delta, false);
            }
        }
    }

    /// Where a new window of frame `size` goes instead of the usual placement: under its parent's title bar for a
    /// sheet, else its app's remembered frame.
    // ponytail: a sheet keeps its place when the parent is resized; re-centre it in the resize path if that shows.
    pub fn attached_loc(&self, window: &Window, size: Size<i32, Logical>) -> Option<Point<i32, Logical>> {
        if let Some(parent) = self.sheet_parent(window) {
            let frame = self.frame(&parent)?;
            return Some(wm::sheet_loc(frame, crate::decorations::bar_height(&parent), size));
        }
        Some(window.user_data().get::<Recalled>()?.0.loc)
    }

    /// Before the first configure of an app's first window: sizes it to the app's remembered frame if that still fits
    /// its display. Not for dialogs, or when a rule places or sizes the window.
    pub fn recall_frame(&mut self, window: &Window) {
        let Some(toplevel) = window.toplevel() else { return };
        let ruled = crate::state::ruled(window).unwrap_or_default();
        if toplevel.parent().is_some() || ruled.size.is_some() || ruled.position.is_some() || ruled.fullscreen == Some(true) {
            return;
        }
        let Some(app) = self.app_id(window) else { return };
        if self.other_app_windows(&app, window) > 0 {
            return;
        }
        let saved = std::fs::read_to_string(frames_path()).unwrap_or_default();
        let Some((_, display, frame)) = wm::parse_frames(&saved).into_iter().find(|(a, ..)| *a == app) else { return };
        let Some(output) = self.space.outputs().find(|o| o.name() == display).cloned() else { return };
        let area = self.output_area(&output);
        let Some(frame) = area.and_then(|area| wm::recall(frame, area)) else { return };
        let size = self.content_size(window, frame.size);
        toplevel.with_pending_state(|s| s.size = Some(size));
        window.user_data().insert_if_missing(|| Recalled(frame));
    }

    /// An app's last window is closing: remember its frame (the untiled one for a tiled, zoomed or full-screen window).
    pub fn remember_frame(&mut self, window: &Window) {
        if window.toplevel().is_none_or(|t| t.parent().is_some()) {
            return;
        }
        let Some(app) = self.app_id(window) else { return };
        if self.other_app_windows(&app, window) > 0 {
            return;
        }
        let Some(mut frame) = self.restore_frame(window).or_else(|| self.frame(window)) else { return };
        frame.loc.y -= self.window_row(window) * crate::spaces::STRIDE;
        let outputs = self.space.outputs().filter_map(|o| Some((self.space.output_geometry(o)?, o.name())));
        let Some(display) = crate::displays::mostly_on(frame, outputs) else { return };
        if frame.size.w <= 0 || frame.size.h <= 0 {
            return;
        }
        let path = frames_path();
        let mut frames = wm::parse_frames(&std::fs::read_to_string(&path).unwrap_or_default());
        frames.retain(|(a, ..)| *a != app);
        frames.push((app, display, frame));
        let saved = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&path, wm::format_frames(&frames)));
        if let Err(err) = saved {
            tracing::warn!("window frames: {err}");
        }
    }

    /// How many other top-level windows (shown or minimised) app `app` has besides `except`.
    fn other_app_windows(&self, app: &str, except: &Window) -> usize {
        let hidden = self.hidden.iter().map(|(w, _)| w);
        self.space
            .elements()
            .chain(hidden)
            .filter(|w| *w != except && w.toplevel().is_some_and(|t| t.parent().is_none()) && self.app_id(w).as_deref() == Some(app))
            .count()
    }
}
