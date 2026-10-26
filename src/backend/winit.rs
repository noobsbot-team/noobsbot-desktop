//! Development backend: the desktop runs as a window on the current session (`--nested`).

use std::time::{Duration, Instant};

use smithay::{
    backend::{
        egl::EGLDevice,
        renderer::{ImportDma, damage::OutputDamageTracker, gles::GlesRenderer},
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    desktop::{layer_map_for_output, utils::OutputPresentationFeedback},
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            EventLoop, channel,
            timer::{TimeoutAction, Timer},
        },
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::Transform,
    wayland::{compositor::SurfaceData, presentation::Refresh},
};

use super::{CLEAR, Device, Views};
use crate::{
    frame::FrameClock,
    render::{self, DockIcon, DockView, LaunchpadFrame, LockContent, SceneElement, View, Wallpaper},
    shell::Popup,
    state::State,
};

const REFRESH_MHZ: i32 = 60_000;

pub struct Winit {
    pub backend: WinitGraphicsBackend<GlesRenderer>,
    output: Output,
    damage_tracker: OutputDamageTracker,
    pub clock: FrameClock,
}

impl Winit {
    /// The window's output.
    pub fn output(&self) -> &Output {
        &self.output
    }
}

pub fn init(event_loop: &mut EventLoop<State>, state: &mut State) -> Result<(), Box<dyn std::error::Error>> {
    let (mut backend, winit) = winit::init::<GlesRenderer>()?;
    backend.window().set_title("Noobsbot Desktop");

    let formats = backend.renderer().dmabuf_formats();
    let node = EGLDevice::device_for_display(backend.renderer().egl_context().display()).and_then(|device| device.try_get_render_node());
    super::dmabuf_global(state, formats, node.ok().flatten());

    let mode = Mode { size: backend.window_size(), refresh: REFRESH_MHZ };
    let output =
        Output::new("winit".into(), PhysicalProperties { size: (0, 0).into(), subpixel: Subpixel::Unknown, make: "Noobsbot".into(), model: "Nested".into() });
    let _global = output.create_global::<State>(&state.display_handle);
    // winit's GL surface is upside down relative to Wayland; Flipped180 corrects it.
    output.change_current_state(Some(mode), Some(Transform::Flipped180), None, Some((0, 0).into()));
    output.set_preferred(mode);
    state.space.map_output(&output, (0, 0));
    let damage_tracker = OutputDamageTracker::from_output(&output);

    state.backend = Some(Views::new(backend.renderer(), wallpaper(event_loop, state)?)?);
    let clock = FrameClock::new(Duration::from_nanos(1_000_000_000_000 / REFRESH_MHZ as u64));
    state.device = Some(Device::Winit(Box::new(Winit { backend, output, damage_tracker, clock })));

    event_loop.handle().insert_source(winit, |event, _, state| match event {
        WinitEvent::Resized { size, .. } => {
            if let Some(Device::Winit(winit)) = &state.device {
                let mode = Mode { size, refresh: REFRESH_MHZ };
                winit.output.change_current_state(Some(mode), None, None, None);
                winit.output.set_preferred(mode);
            }
            // Lays the panels out again and rewrites displays.json with the new size.
            state.apply_displays();
        }
        WinitEvent::Input(event) => state.process_input_event(event),
        WinitEvent::Redraw => state.schedule_redraw(),
        WinitEvent::CloseRequested => state.loop_signal.stop(),
        _ => {}
    })?;

    Ok(())
}

/// The wallpaper, set from the config; pictures decode on a worker thread and upload when they arrive.
pub fn wallpaper(event_loop: &mut EventLoop<State>, state: &State) -> Result<Wallpaper, Box<dyn std::error::Error>> {
    let (tx, rx) = channel::channel();
    event_loop.handle().insert_source(rx, |event, _, state| {
        if let (channel::Event::Msg(decoded), Some(views), Some(renderer)) = (event, state.backend.as_mut(), state.device.as_mut().and_then(Device::renderer)) {
            views.wallpaper.receive(renderer, decoded);
            state.schedule_redraw();
        }
    })?;
    // The slideshow (E8.4) checks for its next picture twice a minute.
    // ponytail: polled, so a change lands up to 30 s late; a timer re-armed per interval if that ever matters.
    let poll = Duration::from_secs(30);
    event_loop.handle().insert_source(Timer::from_duration(poll), move |_, _, state| {
        if state.backend.as_mut().is_some_and(|views| views.wallpaper.tick()) {
            state.schedule_redraw();
        }
        TimeoutAction::ToDuration(poll)
    })?;
    let mut wallpaper = Wallpaper::new(tx);
    wallpaper.set(&state.config.appearance);
    Ok(wallpaper)
}

/// A client's lock surface, if any, at its display's top left.
fn lock_surface_elements(renderer: &mut GlesRenderer, surface: Option<&smithay::wayland::session_lock::LockSurface>, scale: f64) -> Vec<SceneElement> {
    let Some(surface) = surface else { return Vec::new() };
    smithay::backend::renderer::element::surface::render_elements_from_surface_tree(
        renderer,
        surface.wl_surface(),
        (0, 0),
        scale,
        1.0,
        smithay::backend::renderer::element::Kind::Unspecified,
    )
    .into_iter()
    .map(SceneElement::Surface)
    .collect()
}

impl State {
    /// Every element of `output`'s frame, front to back, and whether an animation needs another frame. Both backends
    /// draw with it. Zoom and colour filters (E8.6) take the frame, then Night Light goes in front of everything
    /// (capture.rs leaves both out: `zoom.plain` and no tint).
    pub fn frame_elements(&mut self, renderer: &mut GlesRenderer, output: &Output) -> Option<(Vec<SceneElement>, bool)> {
        if let Some(views) = self.backend.as_mut() {
            views.shaders.contrast = self.config.accessibility.increase_contrast;
        }
        let (elements, animating) = self.scene_elements(renderer, output)?;
        let geo = self.space.output_geometry(output)?;
        let scale = output.current_scale().fractional_scale();
        let zooming = self.zoom.advance(self.frame_time, self.config.appearance.reduce_motion);
        let pointer = self.seat.get_pointer().map(|p| p.current_location()).filter(|p| geo.to_f64().contains(*p)).map(|p| p - geo.loc.to_f64());
        // The focused text field's caret and the focused window, if Zoom follows them and they are on this output.
        let relative = |r: smithay::utils::Rectangle<i32, smithay::utils::Logical>| {
            r.intersection(geo).map(|_| smithay::utils::Rectangle::new(r.loc - geo.loc, r.size).to_f64())
        };
        let follow = self.config.accessibility.zoom_follows_focus;
        let caret = self.text_cursor().filter(|_| follow).and_then(relative);
        let window = self.focused_window().filter(|_| follow).and_then(|w| self.space.element_geometry(&w)).and_then(relative);
        let views = self.backend.as_ref()?;
        let mut elements =
            self.zoom.apply(renderer, &views.shaders, elements, &output.name(), geo.size.to_f64(), pointer, caret, window, &self.config.accessibility, scale);
        let size = geo.size.to_physical_precise_round(scale);
        let (tint, fading) = self.night_light.element(&self.config.night_light, self.frame_time, size);
        elements.splice(0..0, self.osk_elements(renderer, output));
        // Displays ▸ Identify, in front of all but the keyboard.
        let title = self.identify.is_some().then(|| crate::decorations::display_name(&output.name(), &output.physical_properties().model));
        let (dark, glass) = (render::is_dark(&self.config.appearance), self.config.appearance.glass_opacity);
        if let Some(views) = self.backend.as_mut() {
            let named = views.identify.elements(renderer, &views.shaders, &mut self.text, &output.name(), title.as_deref(), geo.size, dark, glass, scale);
            elements.splice(0..0, named);
        }
        elements.splice(0..0, tint.map(SceneElement::Tint));
        // The bell's flash (E8.6), in front of everything.
        let flash = self.bell_flash();
        if let (Some(alpha), Some(views)) = (flash, self.backend.as_ref()) {
            elements.insert(
                0,
                SceneElement::Shader(views.shaders.glass_shape(
                    smithay::utils::Rectangle::from_size(geo.size),
                    [1.0, 1.0, 1.0, alpha],
                    0.0,
                    false,
                    false,
                    scale,
                )),
            );
        }
        Some((elements, animating || fading || flash.is_some() || zooming))
    }

    /// The on-screen keyboard, in front of everything (the lock screen too), on the display with the focused text
    /// field or the pointer (`osk_output`).
    fn osk_elements(&mut self, renderer: &mut GlesRenderer, output: &Output) -> Vec<SceneElement> {
        let Some(geo) = self.osk_panel().filter(|_| self.osk_output().as_ref() == Some(output)).and_then(|_| self.space.output_geometry(output)) else {
            return Vec::new();
        };
        let labels = self.osk_labels();
        let lit = self.osk_lit();
        let appearance = &self.config.appearance;
        let (dark, scale) = (render::is_dark(appearance), output.current_scale().fractional_scale());
        let accent = render::accent(appearance.accent, dark);
        let Some(views) = self.backend.as_mut() else { return Vec::new() };
        views.osk.elements(renderer, &views.shaders, &mut self.text, &mut self.symbols, geo.size, labels, &lit, dark, accent, appearance.glass_opacity, scale)
    }

    /// The app switcher on `output`, when it shows there (the display it opened on).
    fn switcher_elements(&mut self, renderer: &mut GlesRenderer, output: &Output) -> Vec<SceneElement> {
        let Some(open) = self.switcher.open.as_ref().filter(|o| o.visible && o.output.as_ref() == Some(output)) else { return Vec::new() };
        let Some(geo) = self.space.output_geometry(output) else { return Vec::new() };
        let icon_names: Vec<Option<String>> = open.apps.iter().map(|a| a.windows.first().and_then(|w| self.app_icon(w))).collect();
        let names: Vec<String> = open.apps.iter().map(|a| a.name.clone()).collect();
        let (selected, scale) = (open.selected, output.current_scale().fractional_scale());
        let (dark, opacity) = (render::is_dark(&self.config.appearance), self.config.appearance.glass_opacity);
        let Some(views) = self.backend.as_mut() else { return Vec::new() };
        views.switcher.elements(renderer, &views.shaders, &mut self.text, &mut self.icons, &names, &icon_names, selected, geo.size, dark, opacity, scale)
    }

    /// `output`'s menu bar (none while `covered`, by Launchpad or Mission Control) and, when the open popup shows on
    /// this display (`State::popup_output`), it and its submenu; each front to back.
    fn bar_elements(&mut self, renderer: &mut GlesRenderer, output: &Output, covered: bool) -> (Vec<SceneElement>, Vec<SceneElement>) {
        // A popup that hangs from no bar item is on the first display, and so is its fade after it closes.
        if self.popup.is_some_and(|p| p.item().is_none()) {
            self.bar_output = None;
        }
        let here = self.popup_output().as_ref() == Some(output);
        let first = self.space.outputs().next() == Some(output);
        let Some(geo) = self.space.output_geometry(output) else { return Default::default() };
        let (geo_origin, size, width) = (geo.loc, geo.size, geo.size.w);
        let hidden = self.bar_hidden();
        let content = self.bar_content(output);
        let status = matches!(self.popup, Some(Popup::Control | Popup::Module(..))).then(|| self.control_status());
        let center = self.center_layout();
        let batteries = self.batteries();
        let calendar = match self.popup {
            Some(Popup::Notifications { year, month }) => Some(self.calendar_view(year, month, self.config.clock.week_start.into())),
            _ => None,
        };
        let dock_menu =
            self.dock_ui.menu.as_ref().filter(|_| self.popup == Some(Popup::Menu(crate::shell::Menu::Dock))).map(|(_, at)| (self.dock_menu_rows(), *at));
        let input_menu = self.input_menu_rows();
        let overflow_menu = self.overflow_menu_rows();
        let battery_menu = self.battery_menu_rows();
        self.refresh_recent();
        let update_ready = self.update_ready();
        let desktop_menu = (self.popup == Some(Popup::Menu(crate::shell::Menu::Desktop)))
            .then(|| self.desktop_menu_rows())
            .and_then(|rows| Some((self.desktop_menu_anchor(&rows)?, rows)));
        let drawer_menu = (self.popup == Some(Popup::Menu(crate::shell::Menu::Drawer)))
            .then(|| self.drawer_menu_rows())
            .and_then(|rows| Some((self.drawer_menu_anchor(&rows)?, rows)));
        let window_menu = (self.popup == Some(Popup::Menu(crate::shell::Menu::Window)))
            .then(|| self.window_menu_rows())
            .and_then(|rows| Some((self.window_menu_anchor()?, rows)));
        let open_sub = self.open_submenu();
        let cursor = self.menu_cursor.filter(|(p, sub, _)| Some(*p) == self.popup && (open_sub.is_some() || !sub));
        let (now, scale) = (self.frame_time, output.current_scale().fractional_scale());
        let appearance = &self.config.appearance;
        let (dark, reduce, hour24) = (render::is_dark(appearance), appearance.reduce_motion, self.config.clock.format.hour24);
        let accent = render::accent(appearance.accent, dark);
        let Some(Views { shaders, menu_bar, bars, notifications, menu, submenu, control, entrances, .. }) = self.backend.as_mut() else {
            return Default::default();
        };
        let menu_bar = if first { menu_bar } else { bars.entry(output.name()).or_default() };
        // The bar is laid out before its menus, so they line up with this frame's items (it is drawn behind them).
        let active = self.popup.and_then(Popup::item).filter(|_| here);
        let strip = match content.as_ref().filter(|_| !covered && hidden < 1.0) {
            Some(content) => {
                let strip = menu_bar.elements(
                    renderer,
                    shaders,
                    &mut self.text,
                    &mut self.symbols,
                    &mut self.icons,
                    content,
                    active,
                    width,
                    dark,
                    appearance.glass_opacity,
                    scale,
                );
                render::menubar::slid(renderer, strip, hidden, scale)
            }
            None => {
                menu_bar.hide();
                Vec::new()
            }
        };
        if !here {
            return (strip, Vec::new());
        }
        let mut popups = Vec::new();
        match self.popup {
            Some(Popup::Notifications { year, month }) => {
                let week_start = self.config.clock.week_start.into();
                let (dots, up_next) = calendar.clone().unwrap_or_default();
                let shown = render::calendar::Month { year, month, week_start, today: chrono::Local::now().date_naive(), dots };
                if let Some(center) = &center {
                    let view = View::Center {
                        center,
                        month: &shown,
                        up_next: &up_next,
                        batteries: &batteries,
                        dnd: self.config.notifications.dnd,
                        weather: self.weather.as_ref(),
                        widgets: &crate::services::widgets::shown(&self.widgets, &self.config.widgets, false),
                    };
                    popups.extend(notifications.elements(
                        renderer,
                        shaders,
                        &mut self.text,
                        &mut self.symbols,
                        view,
                        &self.notifications,
                        hour24,
                        dark,
                        accent,
                        appearance.glass_opacity,
                        scale,
                    ));
                }
            }
            Some(popup @ Popup::Menu(kind)) => {
                let app = content.as_ref().and_then(|c| c.app.as_deref()).map(|id| menu_bar.name(id)).unwrap_or_default();
                let tray = self.tray_menu.as_ref().map_or(&[][..], |(_, _, rows)| rows);
                let (rows, anchor) = match (kind, dock_menu) {
                    (crate::shell::Menu::Dock, Some((rows, at))) => (rows, crate::shell::Anchor::Above(at)),
                    (crate::shell::Menu::Desktop, _) => {
                        desktop_menu.clone().map(|(anchor, rows)| (rows, anchor)).unwrap_or((Vec::new(), crate::shell::Anchor::Below(0)))
                    }
                    (crate::shell::Menu::Drawer, _) => {
                        drawer_menu.clone().map(|(anchor, rows)| (rows, anchor)).unwrap_or((Vec::new(), crate::shell::Anchor::Below(0)))
                    }
                    (crate::shell::Menu::Window, _) => {
                        window_menu.clone().map(|(anchor, rows)| (rows, anchor)).unwrap_or((Vec::new(), crate::shell::Anchor::Below(0)))
                    }
                    _ => (
                        match kind {
                            crate::shell::Menu::Input => input_menu.clone(),
                            crate::shell::Menu::Overflow => overflow_menu.clone(),
                            crate::shell::Menu::Battery(_) => battery_menu.clone(),
                            _ => crate::shell::menu_rows(kind, &app, tray, update_ready, self.recent_menu.as_deref().unwrap_or_default()),
                        },
                        // A tray item opened from "»" hangs from it.
                        crate::shell::Anchor::Below(
                            popup
                                .item()
                                .and_then(|i| menu_bar.pill(i))
                                .or_else(|| menu_bar.pill(crate::render::menubar::BarItem::Overflow))
                                .map_or(0, |r| r.loc.x),
                        ),
                    ),
                };
                let hover = self.seat.get_pointer().map(|p| p.current_location()).unwrap_or_default() - geo_origin.to_f64();
                // The keyboard's row is lit instead of the pointer's, which goes nowhere.
                let (hover, lit, sub_lit) = match cursor {
                    Some((_, false, i)) => ((f64::MIN, f64::MIN).into(), Some(i), None),
                    Some((_, true, j)) => ((f64::MIN, f64::MIN).into(), open_sub, Some(j)),
                    None => (hover, open_sub, None),
                };
                let main = menu.elements(renderer, shaders, &mut self.text, &rows, anchor, size, hover, lit, dark, accent, appearance.glass_opacity, scale);
                // The submenu beside its row, in front of the menu.
                let beside = open_sub.and_then(|i| Some((rows.get(i)?.as_ref()?.sub.clone(), menu.cell(i)?, menu.rect()?)));
                if let Some((sub, row, parent)) = beside {
                    let anchor = crate::shell::Anchor::Beside(
                        smithay::utils::Rectangle::new((parent.loc.x, row.loc.y).into(), (parent.size.w, row.size.h).into()),
                        false,
                    );
                    popups.extend(submenu.elements(
                        renderer,
                        shaders,
                        &mut self.text,
                        &sub,
                        anchor,
                        size,
                        hover,
                        sub_lit,
                        dark,
                        accent,
                        appearance.glass_opacity,
                        scale,
                    ));
                }
                popups.extend(main);
            }
            Some(Popup::Control | Popup::Module(..)) => {
                if let Some(status) = &status {
                    popups.extend(control.elements(
                        renderer,
                        shaders,
                        &mut self.text,
                        &mut self.symbols,
                        &mut self.icons,
                        status,
                        size,
                        dark,
                        accent,
                        appearance.glass_opacity,
                        scale,
                    ));
                }
            }
            None => {}
        }
        render::motion::appear(entrances.popup(), renderer, &mut popups, 0, scale, now);
        let replaced = entrances.replaced;
        entrances.fade_popup.track(renderer, &mut popups, 0, replaced, true, reduce, scale, now, &mut self.anim_until);
        (strip, popups)
    }

    /// `output`'s dock, when it has one (`[dock] displays`), front to back: the window previews, the dock, the glows of
    /// a hidden one, then the previewed window over a dim of the others; and whether it animates. The hover, previews
    /// and a dragged icon show on `dock_output`'s alone.
    fn dock_elements(&mut self, renderer: &mut GlesRenderer, output: &Output) -> (Vec<SceneElement>, bool) {
        if !self.dock_outputs().contains(output) {
            return (Vec::new(), false);
        }
        let here = self.dock_output().as_ref() == Some(output);
        let dock_items = self.dock_items_on(Some(output));
        let dock_frame = self.dock_frame(output, &dock_items);
        let peek = self.dock_peek().filter(|_| here);
        let peek_hover = self.dock_ui.peek.as_ref().and_then(|p| p.hover);
        let held_at = self.dock_ui.dragged().filter(|_| here).and(self.seat.get_pointer()).map(|p| self.to_dock(self.to_output(p.current_location())));
        let focused_item = self.focused_window().and_then(|w| dock_items.iter().position(|i| i.windows.contains(&w)));
        let output_rect = self.space.output_geometry(output).unwrap_or_default();
        let scale = output.current_scale().fractional_scale();
        let appearance = &self.config.appearance;
        let dark = render::is_dark(appearance);
        let accent = render::accent(appearance.accent, dark);
        let zoom = if self.config.dock.zoom { f64::from(self.config.dock.zoom_percent) / 100.0 } else { 1.0 };
        let Some(Views { shaders, docks, tiles, .. }) = self.backend.as_mut() else { return (Vec::new(), false) };
        let dock = docks.entry(output.name()).or_default();
        let mut bar = Vec::new();
        // Its window previews in front of it, the window hovered there (over a dim of the others) behind it.
        let (peek_front, peek_behind) =
            dock.peek(renderer, shaders, &mut self.text, &self.space, peek.as_ref(), peek_hover, output_rect, dark, accent, appearance.glass_opacity, scale);
        bar.extend(peek_front);
        if let Some(layout) = &dock_frame.layout {
            let mut icons: Vec<DockIcon> = dock_items
                .iter()
                .zip(&dock_frame.shades)
                .enumerate()
                .map(|(i, (item, shade))| {
                    let windows = if item.is_tile() { 0 } else { item.windows.len() };
                    DockIcon { name: item.icon(), windows, focused: focused_item == Some(i), shade: *shade }
                })
                .collect();
            let px = (layout.icon * zoom * scale).ceil() as u32;
            // The dragged icon follows the pointer: one more icon, without a column (so without dots).
            let mut layout = layout.clone();
            // Minimised windows' tiles: the window's last frame, the app's icon small in the corner.
            tiles.retain(|g| self.hidden.iter().any(|(w, _)| *w == g.window));
            let mut thumbs = Vec::new();
            for (i, item) in dock_items.iter().enumerate().filter(|(_, i)| i.is_tile()) {
                let Some(ghost) = tiles.iter().find(|g| item.windows.contains(&g.window)) else { continue };
                let (thumb, badge) = crate::shell::dock::tile_rects(layout.icons[i], ghost.frame().size);
                layout.icons[i] = badge;
                let alpha = f64::from(dock_frame.shades[i].alpha);
                thumbs.push(ghost.drawn(renderer, crate::anim::Look { rect: thumb, alpha }, scale));
            }
            let held = self.dock_ui.dragged().and_then(|key| dock_items.iter().find(|i| i.key == key));
            if let Some((item, at)) = held.zip(held_at) {
                let s = layout.icon;
                layout.icons.push(smithay::utils::Rectangle::new((at.x - s / 2.0, at.y - s / 2.0).into(), (s, s).into()));
                let shade = crate::shell::dock_fx::Shade { alpha: 1.0, lighten: 0.0, darken: 0.0 };
                icons.push(DockIcon { name: item.icon(), windows: 0, focused: false, shade });
            }
            let badges: Vec<_> = dock_frame.badges.iter().map(|(i, entry)| (layout.icons[*i].to_i32_round(), *entry)).collect();
            bar.extend(dock.tooltip(renderer, shaders, &mut self.text, &mut self.symbols, &dock_frame.tips, dark, scale));
            bar.extend(dock.badges(renderer, shaders, &mut self.text, &mut self.symbols, &badges, accent, scale));
            let mut elements = dock.elements(renderer, shaders, &mut self.icons, &icons, &layout, px, scale);
            // In front of the bar (the last element), behind badges and dots.
            let back = elements.pop();
            bar.extend(elements.into_iter().chain(thumbs).chain(back));
            bar.extend(DockView::poofs(shaders, &dock_frame.poofs, layout.icon, scale));
        } else {
            bar.extend(dock.tooltip(renderer, shaders, &mut self.text, &mut self.symbols, &[], dark, scale));
        }
        bar.extend(DockView::glows(shaders, &dock_frame.glows, scale));
        bar.extend(peek_behind);
        (bar, dock_frame.animating)
    }

    /// The alert, on `output` when it shows there (`focus_output`).
    fn alert_elements(&mut self, renderer: &mut GlesRenderer, output: &Output) -> Vec<SceneElement> {
        let shown = self.alert.is_some() && self.focus_output().as_ref() == Some(output);
        let size = self.space.output_geometry(output).map_or_else(Default::default, |g| g.size);
        let (scale, reopen) = (output.current_scale().fractional_scale(), self.config.windows.reopen_at_login);
        let appearance = &self.config.appearance;
        let (dark, glass) = (render::is_dark(appearance), appearance.glass_opacity);
        let accent = render::accent(appearance.accent, dark);
        let Some(views) = self.backend.as_mut() else { return Vec::new() };
        match &self.alert {
            Some(alert) if shown => {
                views.alert.elements(renderer, &views.shaders, &mut self.text, &mut self.symbols, alert, size, dark, accent, glass, scale, reopen)
            }
            Some(_) => Vec::new(),
            None => {
                views.alert.hide();
                Vec::new()
            }
        }
    }

    /// Launchpad, when it shows on `output` (it then covers that display's menu bar and windows).
    fn launchpad_elements(&mut self, renderer: &mut GlesRenderer, output: &Output) -> Option<Vec<SceneElement>> {
        let l = self.launchpad.as_ref().filter(|l| self.overlay_output(l.output.as_ref()).as_ref() == Some(output))?;
        let grid = self.launchpad_grid()?;
        let size = self.space.output_geometry(output)?.size;
        let now = self.frame_time;
        let tiles = crate::shell::launchpad::tiles(self.apps.list(), &l.order, &l.field.text);
        let (lifted, offsets) = (l.lifted(&grid, now), l.offsets(now));
        let open = l.folder.as_ref().and_then(|f| match tiles.get(f.index) {
            Some(crate::shell::launchpad::Tile::Folder(_, apps)) => Some((crate::shell::launchpad::folder_grid(&grid, apps.len()), f)),
            _ => None,
        });
        let frame = LaunchpadFrame {
            tiles: &tiles,
            grid: &grid,
            output: size,
            position: l.position(now),
            page: l.page,
            shown: l.shown.at(now),
            field: &l.field,
            lifted,
            offsets: &offsets,
            onto: l.press.as_ref().and_then(|p| p.onto(now)),
            folder: open.as_ref().map(|((panel, inner), f)| (*panel, inner, *f, f.shown.at(now))),
        };
        let appearance = &self.config.appearance;
        let (dark, glass, scale) = (render::is_dark(appearance), appearance.glass_opacity, output.current_scale().fractional_scale());
        let accent = render::accent(appearance.accent, dark);
        let views = self.backend.as_mut()?;
        Some(views.launchpad.elements(renderer, &views.shaders, &mut self.text, &mut self.symbols, &mut self.icons, &frame, dark, accent, glass, scale))
    }

    /// The app drawer on `output` when it shows there, with its entrance; once closed, its fade-out on the display
    /// it was on.
    fn drawer_elements(&mut self, renderer: &mut GlesRenderer, output: &Output) -> Vec<SceneElement> {
        let open = self.drawer.is_some().then(|| self.drawer_output());
        let frame = self.drawer_frame();
        let appearance = &self.config.appearance;
        let (dark, glass, scale, reduce) =
            (render::is_dark(appearance), appearance.glass_opacity, output.current_scale().fractional_scale(), appearance.reduce_motion);
        let accent = render::accent(appearance.accent, dark);
        let Some(views) = self.backend.as_mut() else { return Vec::new() };
        match open {
            Some(on) if on.as_ref() != Some(output) => return Vec::new(),
            Some(_) => views.drawer.output = Some(output.name()),
            None if views.drawer.output != Some(output.name()) => return Vec::new(),
            None => {}
        }
        let mut elements = match &frame {
            Some(frame) => {
                views.drawer.elements(renderer, &views.shaders, &mut self.text, &mut self.symbols, &mut self.icons, frame, dark, accent, glass, scale)
            }
            None => Vec::new(),
        };
        views.drawer.motion(renderer, &mut elements, 0, frame.is_some(), reduce, scale, self.frame_time, &mut self.anim_until);
        elements
    }

    /// The screenshot overlay and its toolbar, on `output` when it is up there.
    fn shot_elements(&mut self, renderer: &mut GlesRenderer, output: &Output) -> Vec<SceneElement> {
        let here = self.overlay_output(self.screenshots.output.as_ref()).as_ref() == Some(output);
        let Some(shot) = self.screenshots.shot.clone().filter(|_| here) else { return Vec::new() };
        let flags = (self.can_record(), self.screenshots.recording.is_some(), self.screenshots.delay, self.screenshots.no_thumbnail);
        let size = self.shot_size();
        let appearance = &self.config.appearance;
        let (dark, glass, scale) = (render::is_dark(appearance), appearance.glass_opacity, output.current_scale().fractional_scale());
        let accent = render::accent(appearance.accent, dark);
        let Some(views) = self.backend.as_mut() else { return Vec::new() };
        views.screenshot.elements(renderer, &views.shaders, &mut self.text, &mut self.symbols, &shot, flags, size, dark, accent, glass, scale)
    }

    /// The screenshot thumbnail, on `output` when it shows there (`thumbnail_output`).
    fn thumbnail_elements(&mut self, renderer: &mut GlesRenderer, output: &Output) -> Vec<SceneElement> {
        let frame = self.thumbnail_frame();
        let here = self.thumbnail_output().as_ref() == Some(output);
        let (dark, scale) = (render::is_dark(&self.config.appearance), output.current_scale().fractional_scale());
        let Some(views) = self.backend.as_mut() else { return Vec::new() };
        match &frame {
            Some((rect, thumb, alpha)) if here => views.screenshot.thumbnail(renderer, &views.shaders, thumb, *rect, *alpha, dark, scale),
            Some(_) => Vec::new(),
            None => {
                views.screenshot.hide_thumbnail();
                Vec::new()
            }
        }
    }

    /// Spotlight, on `output` when it shows there.
    fn spotlight_elements(&mut self, renderer: &mut GlesRenderer, output: &Output) -> Vec<SceneElement> {
        let Some(open) = self.spotlight.clone().filter(|s| self.overlay_output(s.output.as_ref()).as_ref() == Some(output)) else { return Vec::new() };
        let hits = self.spotlight_results();
        let Some(layout) = self.spotlight_layout(hits.len()) else { return Vec::new() };
        let selected = open.selected.min(layout.rows.len().saturating_sub(1));
        let appearance = &self.config.appearance;
        let (dark, glass, scale) = (render::is_dark(appearance), appearance.glass_opacity, output.current_scale().fractional_scale());
        let accent = render::accent(appearance.accent, dark);
        let Some(views) = self.backend.as_mut() else { return Vec::new() };
        views.spotlight.elements(
            renderer,
            &views.shaders,
            &mut self.text,
            &mut self.symbols,
            &mut self.icons,
            &hits,
            selected,
            &open.field,
            &layout,
            dark,
            accent,
            glass,
            scale,
        )
    }

    /// The hot-corner ripple (E5.4) on `output`, if its corner is there: a faint glass disc with a light rim, growing
    /// out of the corner and fading.
    fn ripple_elements(&mut self, output: &Output) -> Vec<SceneElement> {
        let Some((rect, alpha)) = self.ripple_frame(output) else { return Vec::new() };
        let (dark, glass, scale) = (render::is_dark(&self.config.appearance), self.config.appearance.glass_opacity, output.current_scale().fractional_scale());
        let Some(views) = self.backend.as_ref() else { return Vec::new() };
        let spec = render::Material::Hud.spec(dark);
        let [r, g, b] = spec.tint.map(|c| f32::from(c) / 255.0);
        let radius = f64::from(rect.size.w) / 2.0;
        let rim = if dark { [1.0, 1.0, 1.0, 0.5 * alpha] } else { [0.0, 0.0, 0.0, 0.22 * alpha] };
        let fill = [r, g, b, spec.opacity * glass * alpha * 0.8];
        vec![
            SceneElement::Shader(views.shaders.ring(rect, radius, 1, rim, scale)),
            SceneElement::Shader(views.shaders.glass_shape(rect, fill, radius, false, dark, scale)),
        ]
    }

    // ponytail: banners, the OSD, Force Quit, the emoji picker and the desktop icons are on the first output only,
    // since their caches and hit tests are for one output; every display gets its own menu bar, dock and bar popups,
    // and the alert, Spotlight, Launchpad, the app drawer and the screenshot overlay and thumbnail where they show.
    fn scene_elements(&mut self, renderer: &mut GlesRenderer, output: &Output) -> Option<(Vec<SceneElement>, bool)> {
        let switcher_elements = self.switcher_elements(renderer, output);
        if self.space.outputs().next() != Some(output) {
            let geo = self.space.output_geometry(output)?;
            let scale = output.current_scale().fractional_scale();
            let dark = render::is_dark(&self.config.appearance);
            let (locked, idle_shade, animating) = (self.lock.is_some(), self.idle_shade(), self.animating());
            // Its own Mission Control, Space slide and indicator (E8.7).
            let overview = self.mission_frame(output);
            let slide = if overview.is_none() { self.slide_frame(output) } else { None };
            let indicator = self.space_indicator(output);
            let animating = animating || overview.as_ref().is_some_and(|o| o.animating) || slide.is_some();
            // Launchpad when it shows here, covering the menu bar and the windows.
            let pad = self.launchpad_elements(renderer, output);
            // Its own menu bar and the popups opened from it.
            let (strip, popups) = self.bar_elements(renderer, output, locked || pad.is_some());
            let drawer = self.drawer_elements(renderer, output);
            let mut ripple = self.alert_elements(renderer, output);
            ripple.extend(self.shot_elements(renderer, output));
            ripple.extend(self.thumbnail_elements(renderer, output));
            ripple.extend(self.spotlight_elements(renderer, output));
            ripple.extend(self.ripple_elements(output));
            let animating = animating || !ripple.is_empty();
            // Its own dock, in front of the windows.
            let (dock, dock_animating) = if locked { Default::default() } else { self.dock_elements(renderer, output) };
            let animating = animating || dock_animating;
            let views = self.backend.as_mut()?;
            // Locked (E6.4): nothing of the apps on this output either, only the wallpaper under our screen, or a
            // client's lock surface for this display.
            let mut elements = if let Some(lock) = &self.lock {
                let mut elements = lock_surface_elements(renderer, lock.surface_on(output), scale);
                elements.extend(views.wallpaper.element(&views.shaders, geo.size, scale, dark));
                elements
            } else {
                // Its popups and menu bar, and ghosts of closing and minimising windows, in front of the windows.
                let now = self.frame_time;
                views.ghosts.retain(|g| !g.anim.done(now));
                let mut front = ripple;
                front.extend(popups);
                front.extend(drawer);
                front.extend(strip);
                front.extend(dock);
                let pad_here = pad.is_some();
                front.extend(pad.into_iter().flatten());
                front.extend(views.ghosts.iter().filter(|g| g.output == *output).map(|g| g.element(renderer, &views.shaders, now, scale)));
                let appearance = &self.config.appearance;
                let mission = views.missions.entry(output.name()).or_default();
                if let Some((count, current)) = indicator {
                    front.extend(mission.indicator(&views.shaders, count, current, geo.size.w, dark, appearance.glass_opacity, scale));
                }
                let windows = match (&overview, &slide) {
                    (Some(overview), _) => {
                        let accent = render::accent(appearance.accent, dark);
                        let (glass, text) = (appearance.glass_opacity, &mut self.text);
                        Some(mission.windows(renderer, &views.shaders, text, &self.space, overview, geo, dark, accent, glass, scale))
                    }
                    (None, Some(slide)) => Some(render::sliding(renderer, &views.shaders, &mut self.text, &self.space, slide, scale, dark)),
                    (None, None) if pad_here => Some(Vec::new()),
                    (None, None) => None,
                };
                render::scene(
                    renderer,
                    &views.shaders,
                    &mut views.wallpaper,
                    &mut self.text,
                    &self.space,
                    output,
                    &self.config.appearance,
                    front,
                    windows,
                    self.frame_time,
                )
            };
            // Locked, the wallpaper darkens as under the lock page (30%, 40% in light mode), so every display reads locked.
            // A client's lock surface draws its own.
            let ours = self.lock.as_ref().is_some_and(|l| l.surface_on(output).is_none());
            let lock_shade = ours.then_some(if dark { 0.3 } else { 0.4 });
            if let Some(alpha) = idle_shade.into_iter().chain(lock_shade).reduce(f32::max) {
                elements.insert(0, SceneElement::Shader(views.lock.shade(&views.shaders, geo.size, alpha, scale)));
            }
            elements.splice(0..0, switcher_elements);
            return Some((elements, animating || views.wallpaper.fading()));
        }
        self.capture_ask();
        let shot = self.shot_elements(renderer, output);
        let thumbnail = self.thumbnail_elements(renderer, output);
        let quit_list = self.force_quit.clone().map(|f| {
            let entries = self.force_quit_entries();
            let selected = f.selection(&entries);
            (entries, selected)
        });
        let search = self.spotlight_elements(renderer, output);
        let (cards, more) = self.banner_layout();
        let (dock_elements, dock_animating) = self.dock_elements(renderer, output);
        let now = self.frame_time;
        self.launchpad_frame(now);
        let pad = self.launchpad_elements(renderer, output);
        let desktop_frame = self.desktop_frame();
        let desktop_widgets = self.desktop_widgets();
        let drawer = self.drawer_elements(renderer, output);
        let overview = self.mission_frame(output);
        let slide = if overview.is_none() { self.slide_frame(output) } else { None };
        let indicator = self.space_indicator(output);
        let ripple = self.ripple_elements(output);
        let alert = self.alert_elements(renderer, output);
        let animating = overview.as_ref().is_some_and(|o| o.animating) || slide.is_some() || !ripple.is_empty();
        let idle_shade = self.idle_shade();
        let banner = cards.iter().map(|c| c.id).max();
        if let Some(views) = self.backend.as_mut() {
            views.entrances.update(self.popup, self.osd.is_some(), banner, self.config.appearance.reduce_motion, now, &mut self.anim_until);
        }
        // Launchpad and Mission Control cover the menu bar.
        let (strip, popups) = self.bar_elements(renderer, output, pad.is_some() || overview.is_some());
        let Views {
            shaders,
            wallpaper,
            gallery,
            components,
            menu_bar: _,
            bars: _,
            notifications,
            entering,
            menu: _,
            submenu: _,
            osd,
            identify: _,
            control: _,
            alert: _,
            docks: _,
            switcher: _,
            force_quit,
            spotlight: _,
            emoji: emoji_view,
            osk: _,
            launchpad: _,
            drawer: _,
            mission,
            missions: _,
            ghosts,
            tiles: _,
            entrances,
            lock: lock_view,
            screenshot: _,
            desktop: desktop_view,
            widgets: widget_view,
        } = self.backend.as_mut()?;
        // Wallpaper, layers, windows with rounded corners and shadows (E1.4, E1.5): see render::scene.
        let scale = output.current_scale().fractional_scale();
        let appearance = &self.config.appearance;
        let dark = render::is_dark(appearance);
        let width = self.space.output_geometry(output).map_or(0, |g| g.size.w);
        let accent = render::accent(appearance.accent, dark);
        // The alert in front of the switcher and the Force Quit window, those in front of the OSD, the OSD in front of popups, popups in front of the bar, the bar in
        // front of windows.
        let mut bar = Vec::new();
        bar.extend(alert);
        // The screenshot overlay and its thumbnail: in front of all but the alert.
        bar.extend(shot);
        bar.extend(thumbnail);
        let size = self.space.output_geometry(output).map_or_else(Default::default, |g| g.size);
        bar.extend(switcher_elements);
        if let Some((entries, selected)) = &quit_list {
            bar.extend(force_quit.elements(renderer, shaders, &mut self.text, &mut self.icons, entries, *selected, size, dark, accent, scale));
        }
        bar.extend(search);
        if let Some(picker) = &self.emoji {
            bar.extend(emoji_view.elements(renderer, shaders, &mut self.text, &mut self.symbols, picker, dark, accent, appearance.glass_opacity, scale));
        }
        if let Some((count, current)) = indicator {
            bar.extend(mission.indicator(shaders, count, current, width, dark, appearance.glass_opacity, scale));
        }
        bar.extend(ripple);
        let mark = bar.len();
        if let Some(shown) = &self.osd {
            bar.extend(osd.elements(renderer, shaders, &mut self.text, &mut self.symbols, shown, width, dark, accent, appearance.glass_opacity, scale));
        }
        render::motion::appear(entrances.osd(), renderer, &mut bar, mark, scale, now);
        let reduce = appearance.reduce_motion;
        entrances.fade_osd.track(renderer, &mut bar, mark, false, false, reduce, scale, now, &mut self.anim_until);
        // Locked: the OSD shows over the lock screen (E8.7), everything else in `bar` is dropped below.
        let locked_osd: Vec<SceneElement> = if self.lock.is_some() { bar.drain(mark..).collect() } else { Vec::new() };
        let hour24 = self.config.clock.format.hour24;
        let mark = bar.len();
        // Only the new card slides in, drawn on its own; the cards under it are already there.
        let split = usize::from(cards.first().is_some_and(|c| entrances.entering_banner(now) == Some(c.id)));
        if split == 1 {
            let view = View::Banners { cards: &cards[..1], more: None };
            bar.extend(entering.elements(
                renderer,
                shaders,
                &mut self.text,
                &mut self.symbols,
                view,
                &self.notifications,
                hour24,
                dark,
                accent,
                appearance.glass_opacity,
                scale,
            ));
            render::motion::appear(entrances.banner(), renderer, &mut bar, mark, scale, now);
        }
        if cards.len() > split {
            let view = View::Banners { cards: &cards[split..], more };
            bar.extend(notifications.elements(
                renderer,
                shaders,
                &mut self.text,
                &mut self.symbols,
                view,
                &self.notifications,
                hour24,
                dark,
                accent,
                appearance.glass_opacity,
                scale,
            ));
        }
        entrances.fade_banners.track(renderer, &mut bar, mark, false, false, reduce, scale, now, &mut self.anim_until);
        bar.extend(popups);
        // The app drawer: behind its right-click menu, in front of the bar and the dock.
        bar.extend(drawer);
        bar.extend(strip);
        // The dock: behind the bar and popups, above windows.
        bar.extend(dock_elements);
        // Launchpad: in front of the windows (hidden) and the bar, behind the dock and alerts.
        let pad_here = pad.is_some();
        bar.extend(pad.into_iter().flatten());
        // Ghosts of closing and minimising windows: behind the dock, in front of windows.
        ghosts.retain(|g| !g.anim.done(now));
        bar.extend(ghosts.iter().filter(|g| g.output == *output).map(|g| g.element(renderer, shaders, now, scale)));
        if let (Some(components), Some(geo)) = (components.as_mut(), self.space.output_geometry(output)) {
            bar.extend(components.elements(renderer, shaders, &mut self.text, &mut self.symbols, geo.size, dark, accent, appearance.glass_opacity, scale));
        }
        let output_geo = self.space.output_geometry(output).unwrap_or_default();
        // Where a dragged window will tile (E8.5): `panel` glass at half strength, inset like a window gap.
        if let Some(tile) = self.tile_preview {
            let (loc, size) = (tile.loc - output_geo.loc, tile.size);
            let rect = crate::wm::Rect::new((loc.x + 6, loc.y + 6).into(), (size.w - 12, size.h - 12).into());
            let spec = render::Material::Panel.spec(dark);
            let [r, g, b] = spec.tint.map(|c| f32::from(c) / 255.0);
            bar.push(SceneElement::Shader(shaders.glass_shape(rect, [r, g, b, 0.5 * appearance.glass_opacity], render::WINDOW_RADIUS, true, dark, scale)));
        }
        let windows = match (&overview, &slide) {
            (Some(overview), _) => {
                Some(mission.windows(renderer, shaders, &mut self.text, &self.space, overview, output_geo, dark, accent, appearance.glass_opacity, scale))
            }
            (None, Some(slide)) => Some(render::sliding(renderer, shaders, &mut self.text, &self.space, slide, scale, dark)),
            // Launchpad shows only the wallpaper.
            (None, None) if pad_here => Some(Vec::new()),
            (None, None) => None,
        };
        let mut elements = render::scene(renderer, shaders, wallpaper, &mut self.text, &self.space, output, appearance, bar, windows, now);
        if let (Some(gallery), Some(geo)) = (gallery.as_mut(), self.space.output_geometry(output)) {
            let cards = gallery.elements(renderer, shaders, &mut self.text, geo.size, self.config.appearance.glass_opacity, scale);
            // Above the wallpaper (the last elements), below every window.
            let at = elements.len().saturating_sub(wallpaper.drawn);
            elements.splice(at..at, cards);
        }
        // Desktop icons (E7.2): above the wallpaper, below every window; Launchpad's dim covers them.
        if let Some(frame) = &desktop_frame {
            let icons = desktop_view.elements(renderer, shaders, &mut self.text, &mut self.icons, frame, dark, accent, scale);
            let at = elements.len().saturating_sub(wallpaper.drawn);
            elements.splice(at..at, icons);
        }
        // Other apps' widgets (E8.8): with the icons, above the wallpaper.
        if !desktop_widgets.is_empty() {
            let widgets = crate::services::widgets::shown(&self.widgets, &self.config.widgets, true);
            let view = View::Widgets { rects: &desktop_widgets, widgets: &widgets };
            let cards = widget_view.elements(
                renderer,
                shaders,
                &mut self.text,
                &mut self.symbols,
                view,
                &self.notifications,
                hour24,
                dark,
                accent,
                appearance.glass_opacity,
                scale,
            );
            let at = elements.len().saturating_sub(wallpaper.drawn);
            elements.splice(at..at, cards);
        }
        // Locked (E6.4): only the lock screen, ours or a client's, over the wallpaper.
        // ponytail: the desktop's elements above are still built (cheap, and only on a redraw); skip them if a
        // locked frame ever shows up in a profile.
        if let (Some(lock), Some(geo)) = (&self.lock, self.space.output_geometry(output)) {
            elements = locked_osd;
            // A client's lock: its surface for this display.
            elements.extend(match lock.client_surface().map(|_| lock.surface_on(output)) {
                Some(surface) => lock_surface_elements(renderer, surface, scale),
                None => {
                    let content = LockContent {
                        lock,
                        battery: self.battery,
                        keyboard: &lock.keyboard,
                        layout: crate::shell::lock::layout_pill(&self.layouts),
                        message: &self.config.idle.lock_message,
                        hour24: self.config.clock.format.hour24,
                        now: chrono::Local::now(),
                        caps_lock: self.caps_lock,
                        media: self.media.as_ref(),
                    };
                    lock_view.elements(renderer, shaders, &mut self.text, &mut self.symbols, &content, geo.size, dark, accent, appearance.glass_opacity, scale)
                }
            });
            elements.extend(wallpaper.element(shaders, geo.size, scale, dark));
        }
        // Idle (E6.5): dimmed, or black while the screen is off.
        if let (Some(alpha), Some(geo)) = (idle_shade, self.space.output_geometry(output)) {
            elements.insert(0, SceneElement::Shader(lock_view.shade(shaders, geo.size, alpha, scale)));
        }
        let animating = gallery.is_some() || dock_animating || animating || wallpaper.fading();
        Some((elements, animating || self.animating()))
    }

    /// Draws one frame and tells apps it was shown. Runs only when something changed (see `schedule_redraw`).
    pub fn render(&mut self) {
        let mut device = self.device.take();
        match &mut device {
            Some(Device::Winit(winit)) => self.render_winit(winit),
            // Asked while a display was drawing (the device was out): the displays take it from here.
            Some(Device::Udev(udev)) => udev.redraw_all(&self.loop_handle),
            None => {}
        }
        self.device = device;
    }

    fn render_winit(&mut self, Winit { backend, output, damage_tracker, clock }: &mut Winit) {
        // The test scene is a benchmark: it redraws every frame (its cards are rebuilt each time).
        let benchmark = self.backend.as_ref().is_some_and(|v| v.gallery.is_some());
        let animating;
        let started = Instant::now();
        self.frame_time = clock.target().unwrap_or(started);
        let age = backend.buffer_age().unwrap_or(0);
        let (damage, elements) = match backend.bind() {
            Ok((renderer, mut framebuffer)) => {
                let Some((elements, more)) = self.frame_elements(renderer, output) else { return };
                animating = more;
                let damage = damage_tracker.render_output(renderer, &mut framebuffer, age, &elements, CLEAR).map(|result| {
                    if benchmark {
                        // The test scene is a benchmark: wait for the GPU so the time includes its work (E1.4 budget).
                        // Not glFinish via with_context: that unbinds the window surface and the swap then fails.
                        let cpu = started.elapsed();
                        let _ = result.sync.wait();
                        tracing::debug!(?cpu, cpu_gpu = ?started.elapsed(), "test scene frame");
                    }
                    result.damage.cloned()
                });
                (damage, elements)
            }
            Err(err) => {
                tracing::warn!("bind: {err}");
                return;
            }
        };
        // Always swap, even without damage: the swap waits for vblank, which keeps an app that commits
        // in a loop at the refresh rate. When drawing starts is up to frame.rs (E1.10, see schedule_redraw).
        let render_time = started.elapsed();
        let (shown, damaged) = match damage {
            Ok(damage) => {
                (backend.submit(damage.as_deref()).inspect_err(|err| tracing::warn!("submit: {err}")).is_ok(), Some(damage.is_some_and(|d| !d.is_empty())))
            }
            Err(err) => {
                tracing::warn!("render: {err}");
                (false, None)
            }
        };
        // Read back only when a screenshot or an allowed copy waits, the same way as on the real screen (udev.rs).
        let pixels = damaged.filter(|&damaged| self.frame_wanted(output, damaged)).and_then(|_| {
            let scene: Vec<&SceneElement> = match &self.zoom.plain {
                Some(plain) => vec![plain],
                None => elements.iter().filter(|e| !matches!(e, SceneElement::Tint(_))).collect(),
            };
            crate::capture::render(backend.renderer(), output, &scene, CLEAR)
        });
        drop(elements);

        // Everything on this output was shown. ponytail: stamped after the swap returns, without a vblank counter;
        // the udev backend stamps DRM page-flip events.
        let mut feedback = self.frame_callbacks(output);
        if shown {
            // ponytail: nested, the swap returning stands in for the vblank and only CPU render time is measured; DRM
            // page-flip events and GPU timestamps give the real ones (M6).
            clock.presented(Instant::now(), render_time);
            tracing::trace!(?render_time, "frame");
            let refresh = Refresh::fixed(Duration::from_nanos(1_000_000_000_000 / REFRESH_MHZ as u64));
            feedback.presented(self.protocols.clock.now(), refresh, 0, wp_presentation_feedback::Kind::empty());
        }
        if let Some(damaged) = damaged {
            self.after_frame(output, pixels, damaged);
        }
        self.frame_done();
        // The dock redraws only while it bounces, slides, zooms or glows.
        if animating {
            self.schedule_redraw();
        }
        if shown {
            self.lock_frame_shown();
        }
    }

    /// Frame callbacks for everything on `output`, and its presentation feedback to send once shown (dropped unsent:
    /// "discarded").
    pub fn frame_callbacks(&mut self, output: &Output) -> OutputPresentationFeedback {
        let mut feedback = OutputPresentationFeedback::new(output);
        let on_output = |_: &WlSurface, _: &SurfaceData| Some(output.clone());
        let elapsed = self.start_time.elapsed();
        // Hidden Spaces' apps get no frame callbacks, so they idle (E5.1).
        let top = self.space.output_geometry(output).map_or(0, |g| g.loc.y);
        // Locked, apps get none either: nothing of them is shown.
        for window in
            self.space.elements().filter(|w| self.lock.is_none() && self.space.element_location(w).is_some_and(|l| crate::spaces::row_at(l.y, top) == 0))
        {
            window.send_frame(output, elapsed, Some(Duration::ZERO), on_output);
            window.take_presentation_feedback(&mut feedback, on_output, |_, _| wp_presentation_feedback::Kind::empty());
        }
        let mut layers = layer_map_for_output(output);
        for layer in layers.layers() {
            layer.send_frame(output, elapsed, Some(Duration::ZERO), on_output);
            layer.take_presentation_feedback(&mut feedback, on_output, |_, _| wp_presentation_feedback::Kind::empty());
        }
        layers.cleanup();
        drop(layers);
        for (surface, _) in self.lock.iter().flat_map(|l| &l.surfaces).filter(|(_, o)| o == output) {
            smithay::desktop::utils::send_frames_surface_tree(surface.wl_surface(), output, elapsed, Some(Duration::ZERO), on_output);
        }
        feedback
    }

    /// After a frame: drop what closed, tell moved windows their scale and send the replies.
    pub fn frame_done(&mut self) {
        self.space.refresh();
        self.send_scales();
        self.popups.cleanup();
        let _ = self.display_handle.flush_clients();
    }
}
