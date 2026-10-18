//! The dock (E3.1): the owner's Plank dock (Transparent-Tight theme) rebuilt in the shell. Pinned apps from
//! `[dock] items`, then running apps; a dot under each running one; click to launch, focus or minimise; Plank's
//! parabolic zoom under the pointer; intelligent hide. Layout is a pure function so drawing and clicks share it.

use std::sync::atomic::{AtomicU64, Ordering};

use smithay::{
    desktop::Window,
    reexports::{wayland_protocols::xdg::shell::server::xdg_toplevel, wayland_server::Resource},
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Size},
    wayland::{compositor::with_states, shell::xdg::XdgToplevelSurfaceData},
};

use crate::{
    apps::App,
    config::{Dock, DockHide},
    state::State,
    wm::Rect,
};

/// Icons never shrink below this to fit the screen (D13).
pub const MIN_ICON: i32 = 32;
/// Plank's theme units are tenths of the icon size: ItemPadding 0.5 → gap 0.05 × icon, BottomPadding 2 → 0.2 × icon.
const ITEM_PAD: f64 = 0.05;
const BOTTOM: f64 = 0.2;
/// TopPadding -12: the bar stands behind the lower half of the icons only; the top half rises above it (E3.1 design).
const BAR_COVER: f64 = 0.5;
/// How far the zoom reaches either side of the pointer, in icon steps (Plank-style parabola).
const ZOOM_REACH: f64 = 2.5;

/// Where everything is, output-relative.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// One icon square per item, zoomed.
    pub icons: Vec<Rectangle<f64, Logical>>,
    /// Each item's click column: its icon plus the gaps beside it.
    pub columns: Vec<(f64, f64)>,
    /// The Transparent-Tight background bar.
    pub bar: Rect,
    /// The unzoomed dock: where the pointer zooms and clicks.
    pub zone: Rect,
    /// The unzoomed icon size after fitting.
    pub icon: f64,
}

/// The icon size for `n` items on a `width`-wide screen: `icon`, shrunk until they fit (D13), never below MIN_ICON.
pub fn fit(n: usize, width: i32, icon: i32) -> f64 {
    let fits = f64::from(width) / (n.max(1) as f64 * (1.0 + ITEM_PAD));
    f64::from(icon).min(fits.floor()).max(f64::from(MIN_ICON))
}

/// What windows keep clear of at the bottom of the screen: the dock at its configured size.
pub fn reserve(dock: &Dock) -> i32 {
    (f64::from(dock.icon_size) * (1.0 + BOTTOM)).round() as i32
}

/// The dock of `n` items on an `output`-sized screen, zoomed to `zoom` (1.5 = 150%) around `pointer` when the pointer is
/// over it. The item under the pointer stays under it while the others grow, so the dock widens away from it.
pub fn layout(n: usize, output: Size<i32, Logical>, icon: i32, zoom: f64, pointer: Option<Point<f64, Logical>>) -> Layout {
    let s = fit(n, output.w, icon);
    let step = s * (1.0 + ITEM_PAD);
    let bottom = (s * BOTTOM).round();
    let (w, h) = (f64::from(output.w), f64::from(output.h));
    let left = ((w - step * n as f64) / 2.0).round();
    let zone = Rectangle::new((left as i32, (h - s - bottom) as i32).into(), ((step * n as f64).round() as i32, (s + bottom) as i32).into());
    let pointer = pointer.filter(|p| n > 0 && zoom > 1.0 && zone.to_f64().contains(*p));
    let scale = |i: usize| {
        let Some(p) = pointer else { return 1.0 };
        let d = (p.x - (left + step * (i as f64 + 0.5))).abs() / (ZOOM_REACH * step);
        1.0 + (zoom - 1.0) * (1.0 - d * d).max(0.0)
    };
    let widths: Vec<f64> = (0..n).map(|i| step * scale(i)).collect();
    // The first column's left edge: the pointer keeps its fraction of the item it is over.
    let start = match pointer {
        Some(p) => {
            let k = (((p.x - left) / step).floor() as usize).min(n - 1);
            let fraction = (p.x - left - step * k as f64) / step;
            p.x - fraction * widths[k] - widths[..k].iter().sum::<f64>()
        }
        None => left,
    };
    let mut x = start;
    let (mut icons, mut columns) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for (i, width) in widths.iter().enumerate() {
        let size = s * scale(i);
        icons.push(Rectangle::new((x + (width - size) / 2.0, h - bottom - size).into(), (size, size).into()));
        columns.push((x, x + width));
        x += width;
    }
    let bar_top = (h - bottom - s * BAR_COVER).round() as i32;
    let bar = Rectangle::new((start.round() as i32, bar_top).into(), ((x - start).round() as i32, output.h - bar_top).into());
    Layout { icons, columns, bar, zone, icon: s }
}

/// The item a press at `p` hits: inside the unzoomed dock, by column.
pub fn hit(layout: &Layout, p: Point<f64, Logical>) -> Option<usize> {
    if !layout.zone.to_f64().contains(p) {
        return None;
    }
    layout.columns.iter().position(|(l, r)| p.x >= *l && p.x < *r)
}

/// One dock icon.
#[derive(Debug, Clone)]
pub struct Item {
    /// The desktop-file id, or the window's app id when no entry matches.
    pub key: String,
    pub app: Option<App>,
    /// Shown and minimised, topmost shown first.
    pub windows: Vec<Window>,
}

impl Item {
    pub fn icon(&self) -> &str {
        self.app.as_ref().map_or(&self.key, |a| &a.icon)
    }
}

/// The order windows first showed up in the dock, so running apps keep their place when focus restacks windows.
struct Seen(u64);
static SEEN: AtomicU64 = AtomicU64::new(0);

fn seen(window: &Window) -> u64 {
    window.user_data().get_or_insert(|| Seen(SEEN.fetch_add(1, Ordering::Relaxed))).0
}

/// Linux input codes of the mouse buttons the dock answers.
const BTN_LEFT: u32 = 0x110;
const BTN_MIDDLE: u32 = 0x112;

impl State {
    /// The app a window belongs to: its app id, else (noobsbot and others set none) its process name.
    pub fn app_key(&self, window: &Window) -> Option<String> {
        let toplevel = window.toplevel()?;
        let id = with_states(toplevel.wl_surface(), |s| s.data_map.get::<XdgToplevelSurfaceData>().and_then(|d| d.lock().ok()?.app_id.clone()));
        id.filter(|id| !id.is_empty()).or_else(|| {
            let pid = toplevel.wl_surface().client()?.get_credentials(&self.display_handle).ok()?.pid;
            Some(std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?.trim().to_owned())
        })
    }

    /// Pinned apps that are installed, in config order, then running apps in the order they started.
    pub fn dock_items(&self) -> Vec<Item> {
        let mut items: Vec<Item> = Vec::new();
        for name in &self.config.dock.items {
            if let Some(app) = self.apps.find(name).filter(|a| !items.iter().any(|i| i.key == a.id)) {
                items.push(Item { key: app.id.clone(), app: Some(app.clone()), windows: Vec::new() });
            }
        }
        let mut windows: Vec<Window> = self.space.elements().rev().chain(self.hidden.iter().map(|(w, _)| w)).cloned().collect();
        windows.sort_by_key(seen);
        for window in windows {
            let Some(raw) = self.app_key(&window) else { continue };
            let app = self.apps.find(&raw);
            let key = app.map_or(raw, |a| a.id.clone());
            match items.iter_mut().find(|i| i.key == key) {
                Some(item) => item.windows.push(window),
                None => items.push(Item { key, app: app.cloned(), windows: vec![window] }),
            }
        }
        // Topmost shown window first within each app.
        let stack: Vec<&Window> = self.space.elements().rev().collect();
        for item in &mut items {
            item.windows.sort_by_key(|w| stack.iter().position(|s| *s == w).unwrap_or(usize::MAX));
        }
        items
    }

    /// The dock's layout for `n` items, zoomed around the pointer (or `--show dock-zoom`'s fake one).
    pub fn dock_layout(&self, n: usize) -> Option<Layout> {
        let size = self.space.outputs().next().and_then(|o| self.space.output_geometry(o))?.size;
        let dock = &self.config.dock;
        let zoom = if dock.zoom { f64::from(dock.zoom_percent) / 100.0 } else { 1.0 };
        let pointer = self.dock_pointer.or_else(|| self.seat.get_pointer().map(|p| self.to_output(p.current_location())));
        Some(layout(n, size, i32::from(dock.icon_size), zoom, pointer))
    }

    /// Shown, unless a fullscreen window has focus, or (intelligent hide) the focused window overlaps it and the pointer
    /// hasn't called it up from the bottom edge.
    pub fn dock_visible(&self, layout: &Layout) -> bool {
        let focused = self.focused_window();
        let state = |s| focused.as_ref().and_then(|w| w.toplevel()).is_some_and(|t| t.current_state().states.contains(s));
        if state(xdg_toplevel::State::Fullscreen) {
            return false;
        }
        let origin = self.to_output((0.0, 0.0).into()).to_i32_round::<i32>();
        let overlaps = focused.and_then(|w| self.frame(&w)).is_some_and(|f| Rectangle::new(f.loc + origin, f.size).overlaps(layout.zone));
        self.config.dock.hide == DockHide::Never || self.dock_reveal || !overlaps
    }

    /// A cheap first check for every pointer motion: is `location` low enough to be on the dock at all?
    fn near_dock(&self, location: Point<f64, Logical>) -> bool {
        let height = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map_or(0, |g| g.size.h);
        location.y >= f64::from(height - reserve(&self.config.dock))
    }

    /// Whether the dock takes the pointer at `location` (output-relative).
    pub fn dock_covers(&self, location: Point<f64, Logical>) -> bool {
        if !self.near_dock(location) {
            return false;
        }
        let items = self.dock_items();
        self.dock_layout(items.len()).is_some_and(|l| !items.is_empty() && self.dock_visible(&l) && l.zone.to_f64().contains(location))
    }

    /// A press at `location`; `true` if the dock took it. With a popup open the shell closes it first.
    pub fn dock_press(&mut self, location: Point<f64, Logical>, button: u32) -> bool {
        if self.popup.is_some() || self.alert.is_some() {
            return false;
        }
        let items = self.dock_items();
        let Some(layout) = self.dock_layout(items.len()).filter(|l| self.dock_visible(l)) else { return false };
        if !layout.zone.to_f64().contains(location) {
            return false;
        }
        let Some(item) = hit(&layout, location).and_then(|i| items.into_iter().nth(i)) else { return true };
        match button {
            BTN_LEFT => self.dock_activate(item),
            BTN_MIDDLE => self.launch(item.app.as_ref()),
            // ponytail: the right-click menu (windows · Keep in Dock · New Window · Close All), drag to reorder or
            // unpin, drops, badges and the Plank importer are the rest of E3.1, not built yet.
            _ => {}
        }
        true
    }

    /// Left click: launches a closed app, minimises the focused one (Plank), else brings the app's windows forward
    /// (minimised ones too) and focuses its topmost.
    fn dock_activate(&mut self, item: Item) {
        if item.windows.is_empty() {
            return self.launch(item.app.as_ref());
        }
        let shown: Vec<Window> = item.windows.iter().filter(|w| self.space.elements().any(|s| s == *w)).cloned().collect();
        if self.focused_window().is_some_and(|f| shown.contains(&f)) {
            return self.hide_windows(shown);
        }
        let restored: Vec<(Window, Point<i32, Logical>)> = self.hidden.iter().filter(|(w, _)| item.windows.contains(w)).cloned().collect();
        self.hidden.retain(|(w, _)| !item.windows.contains(w));
        for (window, loc) in &restored {
            self.set_visible_loc(window, *loc);
        }
        // Bottom first, so their order is kept and the topmost ends on top with focus.
        for window in shown.iter().rev().skip(1) {
            self.space.raise_element(window, false);
        }
        let top = shown.first().or(restored.last().map(|(w, _)| w)).cloned();
        self.focus_window(top, SERIAL_COUNTER.next_serial());
        self.schedule_redraw();
    }

    /// Starts `app` from its `Exec` line.
    // ponytail: no launch bounce (Plank's 600 ms bounce needs the animation module); Terminal=true apps start without
    // a terminal.
    fn launch(&mut self, app: Option<&App>) {
        let Some(app) = app else { return };
        tracing::info!(app = app.id, "dock: launch");
        self.spawn(&app.command());
    }

    /// A wheel step over an icon cycles through that app's windows. `true`: over the dock, so apps don't get it.
    pub fn dock_scroll(&mut self, location: Point<f64, Logical>, step: bool) -> bool {
        if !self.dock_covers(location) {
            return false;
        }
        let items = self.dock_items();
        let Some(item) = self.dock_layout(items.len()).and_then(|l| hit(&l, location)).and_then(|i| items.into_iter().nth(i)) else { return true };
        let shown: Vec<Window> = item.windows.iter().filter(|w| self.space.elements().any(|s| s == *w)).cloned().collect();
        // The app's bottom window comes to the top: repeated steps go round all of them.
        if let Some(next) = shown.last().filter(|_| step).cloned() {
            self.focus_window(Some(next), SERIAL_COUNTER.next_serial());
            self.schedule_redraw();
        }
        true
    }

    /// The pointer moved: redraw while it zooms the dock; touching the bottom edge calls up a hidden dock, leaving it
    /// lets it hide again.
    pub fn dock_motion(&mut self, from: Point<f64, Logical>, to: Point<f64, Logical>) {
        if !self.dock_reveal && !self.near_dock(from) && !self.near_dock(to) {
            return;
        }
        let n = self.dock_items().len();
        let (Some(layout), Some(size)) = (self.dock_layout(n), self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map(|g| g.size)) else {
            return;
        };
        let zone = layout.zone.to_f64();
        let edge = to.y >= f64::from(size.h) - 1.0;
        let reveal = if edge { true } else { self.dock_reveal && zone.contains(to) };
        if reveal != self.dock_reveal || (self.config.dock.zoom && (zone.contains(from) || zone.contains(to))) {
            self.dock_reveal = reveal;
            self.schedule_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: (i32, i32) = (1920, 1200);

    #[test]
    fn icons_shrink_to_fit() {
        assert_eq!(fit(8, 1920, 84), 84.0);
        // 30 apps on 1280: 1280 / (30 × 1.05) = 40.6.
        assert_eq!(fit(30, 1280, 84), 40.0);
        assert_eq!(fit(100, 1280, 84), f64::from(MIN_ICON));
        assert_eq!(fit(0, 1280, 84), 84.0);
    }

    #[test]
    fn unzoomed_dock_is_centred_and_tight() {
        let l = layout(8, SCREEN.into(), 84, 1.5, None);
        // 8 steps of 84 × 1.05 = 705.6 px, centred; 84 px icons on a 17 px bottom band.
        assert_eq!(l.zone, Rectangle::new((607, 1099).into(), (706, 101).into()));
        assert_eq!(l.icons[0].size.w, 84.0);
        assert_eq!(l.icons[0].loc.y, 1200.0 - 17.0 - 84.0);
        assert!((l.icons[1].loc.x - l.icons[0].loc.x - 88.2).abs() < 1e-9, "4.2 px gaps");
        // The bar covers the icons' lower half and reaches the screen bottom, end to end.
        assert_eq!((l.bar.loc.y, l.bar.loc.y + l.bar.size.h), (1141, 1200));
        assert_eq!((l.bar.loc.x, l.bar.size.w), (607, 706));
    }

    #[test]
    fn zoom_peaks_under_the_pointer_and_keeps_it_there() {
        let flat = layout(8, SCREEN.into(), 84, 1.5, None);
        let centre = flat.icons[3].loc + Point::from((42.0, 42.0));
        let l = layout(8, SCREEN.into(), 84, 1.5, Some(centre));
        assert!((l.icons[3].size.w - 126.0).abs() < 1e-9, "150% under the pointer");
        assert!(l.icons[2].size.w < 126.0 && l.icons[2].size.w > l.icons[1].size.w && l.icons[1].size.w > l.icons[0].size.w);
        assert_eq!(l.icons[7].size.w, 84.0, "out of reach: unzoomed");
        // The hovered icon is still centred under the pointer, and the zoomed icon stands on the same baseline.
        assert!((l.icons[3].loc.x + 63.0 - centre.x).abs() < 1e-9);
        assert_eq!(l.icons[3].loc.y + 126.0, flat.icons[3].loc.y + 84.0);
        assert!(l.bar.size.w > flat.bar.size.w);
        assert_eq!(hit(&l, centre), Some(3));
        // Off the dock or with zoom off: flat.
        assert_eq!(layout(8, SCREEN.into(), 84, 1.5, Some((100.0, 100.0).into())), flat);
        assert_eq!(layout(8, SCREEN.into(), 84, 1.0, Some(centre)), flat);
    }

    #[test]
    fn hits_follow_columns_inside_the_zone() {
        let l = layout(3, SCREEN.into(), 84, 1.0, None);
        let x = |i: usize| (l.columns[i].0 + l.columns[i].1) / 2.0;
        assert_eq!(hit(&l, (x(0), 1150.0).into()), Some(0));
        assert_eq!(hit(&l, (x(2), 1199.0).into()), Some(2));
        assert_eq!(hit(&l, (x(1), 1000.0).into()), None, "above the dock");
        assert_eq!(hit(&l, (10.0, 1150.0).into()), None, "beside it");
        // Every pointer position along a zoomed dock hits the item it zooms.
        for px in (l.zone.loc.x..l.zone.loc.x + l.zone.size.w).step_by(7) {
            let p = (f64::from(px), 1150.0).into();
            let z = layout(3, SCREEN.into(), 84, 1.5, Some(p));
            let i = hit(&z, p).expect("inside");
            assert!(z.icons.iter().all(|r| r.size.w <= z.icons[i].size.w + 1e-9), "at {px}");
        }
    }

    #[test]
    fn reserve_matches_the_zone() {
        assert_eq!(reserve(&Dock::default()), layout(8, SCREEN.into(), 84, 1.0, None).zone.size.h);
    }
}
