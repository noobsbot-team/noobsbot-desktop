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
        calloop::{EventLoop, channel},
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
                layer_map_for_output(&winit.output).arrange();
            }
            state.schedule_redraw();
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
    let mut wallpaper = Wallpaper::new(tx);
    wallpaper.set(&state.config.appearance);
    Ok(wallpaper)
}

impl State {
    /// Every element of `output`'s frame, front to back, and whether an animation needs another frame. Both backends
    /// draw with it.
    // ponytail: the shell (bar, dock, popups) is on the first output only, since its caches and hit tests are for one
    // output; a bar and dock per display (E6.2) needs a `Views` per output.
    pub fn frame_elements(&mut self, renderer: &mut GlesRenderer, output: &Output) -> Option<(Vec<SceneElement>, bool)> {
        if self.space.outputs().next() != Some(output) {
            let views = self.backend.as_mut()?;
            let elements =
                render::scene(renderer, &views.shaders, &mut views.wallpaper, &mut self.text, &self.space, output, &self.config.appearance, Vec::new(), None);
            return Some((elements, false));
        }
        self.capture_ask();
        let shot_flags = (self.can_record(), self.screenshots.recording.is_some(), self.screenshots.delay, self.screenshots.no_thumbnail);
        let thumbnail = self.thumbnail_rect().zip(self.screenshots.thumbnail.as_ref().map(|(t, _)| t.clone()));
        let content = self.menu_bar();
        let status = (self.popup == Some(Popup::Control)).then(|| self.control_status());
        let icon_names: Vec<Option<String>> =
            self.switcher.open.as_ref().map(|o| o.apps.iter().map(|a| a.windows.first().and_then(|w| self.app_icon(w))).collect()).unwrap_or_default();
        let quit_list = self.force_quit.clone().map(|f| {
            let entries = self.force_quit_entries();
            let selected = f.selection(&entries);
            (entries, selected)
        });
        let search = self.spotlight.clone().and_then(|s| {
            let hits = self.spotlight_results();
            Some((self.spotlight_layout(hits.len())?, hits, s))
        });
        let center = self.center_layout();
        let (cards, more) = self.banner_layout();
        let batteries = self.batteries();
        let calendar = match self.popup {
            Some(Popup::Notifications { year, month }) => Some(self.calendar_view(year, month, self.config.clock.week_start.into())),
            _ => None,
        };
        let dock_items = self.dock_items();
        let dock_frame = self.dock_frame(&dock_items);
        let dock_layout = dock_frame.layout.clone();
        let held_at = self.dock_ui.dragged().and(self.seat.get_pointer()).map(|p| self.to_output(p.current_location()));
        let focused_item = self.focused_window().and_then(|w| dock_items.iter().position(|i| i.windows.contains(&w)));
        let now = Instant::now();
        self.launchpad_frame(now);
        let pad = self.launchpad.as_ref().map(|l| (l.position(now), l.shown.at(now), l.field.clone(), l.page)).zip(self.launchpad_grid());
        let dock_menu =
            self.dock_ui.menu.as_ref().filter(|_| self.popup == Some(Popup::Menu(crate::shell::Menu::Dock))).map(|(_, at)| (self.dock_menu_rows(), *at));
        let input_menu = self.input_menu_rows();
        let update_ready = self.update_ready();
        let desktop_menu = (self.popup == Some(Popup::Menu(crate::shell::Menu::Desktop)))
            .then(|| self.desktop_menu_rows())
            .and_then(|rows| Some((self.desktop_menu_anchor(&rows)?, rows)));
        let desktop_frame = self.desktop_frame();
        let overview = self.mission_frame();
        let slide = if overview.is_none() { self.slide_frame() } else { None };
        let indicator = self.spaces.hud.is_some().then(|| self.desk_names()).map(|(names, current)| (names.len(), current));
        let output_size = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map(|g| g.size);
        let ripple = output_size.and_then(|size| self.ripple_frame(size));
        let animating = overview.as_ref().is_some_and(|o| o.animating) || slide.is_some() || ripple.is_some();
        let idle_shade = self.idle_shade();
        let Views {
            shaders,
            wallpaper,
            gallery,
            components,
            menu_bar,
            notifications,
            entering,
            menu,
            osd,
            control,
            alert: sheet,
            dock,
            switcher,
            force_quit,
            spotlight,
            emoji: emoji_view,
            launchpad: pad_view,
            mission,
            ghosts,
            entrances,
            lock: lock_view,
            screenshot,
            desktop: desktop_view,
        } = self.backend.as_mut()?;
        // Wallpaper, layers, windows with rounded corners and shadows (E1.4, E1.5): see render::scene.
        let scale = output.current_scale().fractional_scale();
        let appearance = &self.config.appearance;
        let dark = render::is_dark(appearance);
        let (geo_origin, width) = self.space.output_geometry(output).map_or(((0, 0).into(), 0), |g| (g.loc, g.size.w));
        let accent = render::accent(appearance.accent, dark);
        let banner = cards.iter().map(|c| c.id).max();
        entrances.update(self.popup, self.osd.is_some(), banner, appearance.reduce_motion, now, &mut self.anim_until);
        // The alert in front of the switcher and the Force Quit window, those in front of the OSD, the OSD in front of popups, popups in front of the bar, the bar in
        // front of windows.
        let mut bar = Vec::new();
        match &self.alert {
            Some(shown) => {
                let size = self.space.output_geometry(output).map_or_else(Default::default, |g| g.size);
                bar.extend(sheet.elements(renderer, shaders, &mut self.text, &mut self.symbols, shown, size, dark, accent, appearance.glass_opacity, scale));
            }
            None => sheet.hide(),
        }
        // The screenshot overlay and its thumbnail: in front of all but the alert.
        if let Some(shot) = &self.screenshots.shot {
            let size = self.space.output_geometry(output).map_or_else(Default::default, |g| g.size);
            bar.extend(screenshot.elements(
                renderer,
                shaders,
                &mut self.text,
                &mut self.symbols,
                shot,
                shot_flags,
                size,
                dark,
                accent,
                appearance.glass_opacity,
                scale,
            ));
        }
        match &thumbnail {
            Some((rect, thumb)) => bar.extend(screenshot.thumbnail(renderer, shaders, thumb, *rect, dark, scale)),
            None => screenshot.hide_thumbnail(),
        }
        let size = self.space.output_geometry(output).map_or_else(Default::default, |g| g.size);
        if let Some(open) = self.switcher.open.as_ref().filter(|o| o.visible) {
            let names: Vec<String> = open.apps.iter().map(|a| a.name.clone()).collect();
            bar.extend(switcher.elements(
                renderer,
                shaders,
                &mut self.text,
                &mut self.icons,
                &names,
                &icon_names,
                open.selected,
                size,
                dark,
                appearance.glass_opacity,
                scale,
            ));
        }
        if let Some((entries, selected)) = &quit_list {
            bar.extend(force_quit.elements(renderer, shaders, &mut self.text, &mut self.icons, entries, *selected, size, dark, accent, scale));
        }
        if let Some((layout, hits, open)) = &search {
            let selected = open.selected.min(layout.rows.len().saturating_sub(1));
            bar.extend(spotlight.elements(
                renderer,
                shaders,
                &mut self.text,
                &mut self.symbols,
                &mut self.icons,
                hits,
                selected,
                &open.field,
                layout,
                dark,
                accent,
                appearance.glass_opacity,
                scale,
            ));
        }
        if let Some(picker) = &self.emoji {
            bar.extend(emoji_view.elements(renderer, shaders, &mut self.text, &mut self.symbols, picker, dark, accent, appearance.glass_opacity, scale));
        }
        if let Some((count, current)) = indicator {
            bar.extend(mission.indicator(shaders, count, current, width, dark, appearance.glass_opacity, scale));
        }
        // The hot-corner ripple (E5.4): a faint glass disc with a light rim, growing out of the corner and fading.
        if let Some((rect, alpha)) = ripple {
            let spec = render::Material::Hud.spec(dark);
            let [r, g, b] = spec.tint.map(|c| f32::from(c) / 255.0);
            let radius = f64::from(rect.size.w) / 2.0;
            let rim = if dark { [1.0, 1.0, 1.0, 0.5 * alpha] } else { [0.0, 0.0, 0.0, 0.22 * alpha] };
            bar.push(render::SceneElement::Shader(shaders.ring(rect, radius, 1, rim, scale)));
            let fill = [r, g, b, spec.opacity * appearance.glass_opacity * alpha * 0.8];
            bar.push(render::SceneElement::Shader(shaders.glass_shape(rect, fill, radius, false, dark, scale)));
        }
        let mark = bar.len();
        if let Some(shown) = &self.osd {
            bar.extend(osd.elements(renderer, shaders, &mut self.text, &mut self.symbols, shown, width, dark, accent, appearance.glass_opacity, scale));
        }
        render::motion::appear(entrances.osd(), renderer, &mut bar, mark, scale, now);
        let reduce = appearance.reduce_motion;
        entrances.fade_osd.track(renderer, &mut bar, mark, false, false, reduce, scale, now, &mut self.anim_until);
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
        // The bar is laid out before its menus, so they line up with this frame's items (it is drawn behind them).
        let active = self.popup.and_then(Popup::item);
        // Launchpad covers the menu bar.
        let strip = match content.as_ref().filter(|_| pad.is_none() && overview.is_none()) {
            Some(content) => {
                menu_bar.elements(renderer, shaders, &mut self.text, &mut self.symbols, content, active, width, dark, appearance.glass_opacity, scale)
            }
            None => {
                menu_bar.hide();
                Vec::new()
            }
        };
        let mark = bar.len();
        match self.popup {
            Some(Popup::Notifications { year, month }) => {
                let week_start = self.config.clock.week_start.into();
                let (dots, up_next) = calendar.clone().unwrap_or_default();
                let shown = render::calendar::Month { year, month, week_start, today: chrono::Local::now().date_naive(), dots };
                if let Some(center) = &center {
                    let view = View::Center { center, month: &shown, up_next: &up_next, batteries: &batteries, dnd: self.dnd, weather: self.weather.as_ref() };
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
            }
            Some(popup @ Popup::Menu(kind)) => {
                let app = content.as_ref().and_then(|c| c.app.as_deref()).map(|id| menu_bar.name(id)).unwrap_or_default();
                let tray = self.tray_menu.as_ref().map_or(&[][..], |(_, _, rows)| rows);
                let (rows, anchor) = match (kind, dock_menu) {
                    (crate::shell::Menu::Dock, Some((rows, at))) => (rows, crate::shell::Anchor::Above(at)),
                    (crate::shell::Menu::Desktop, _) => {
                        desktop_menu.clone().map(|(anchor, rows)| (rows, anchor)).unwrap_or((Vec::new(), crate::shell::Anchor::Below(0)))
                    }
                    _ => (
                        if kind == crate::shell::Menu::Input { input_menu.clone() } else { crate::shell::menu_rows(kind, &app, tray, update_ready) },
                        crate::shell::Anchor::Below(popup.item().and_then(|i| menu_bar.pill(i)).map_or(0, |r| r.loc.x)),
                    ),
                };
                let hover = self.seat.get_pointer().map(|p| p.current_location()).unwrap_or_default() - geo_origin.to_f64();
                bar.extend(menu.elements(renderer, shaders, &mut self.text, &rows, anchor, width, hover, dark, accent, appearance.glass_opacity, scale));
            }
            Some(Popup::Control) => {
                if let Some(status) = &status {
                    bar.extend(control.elements(
                        renderer,
                        shaders,
                        &mut self.text,
                        &mut self.symbols,
                        status,
                        width,
                        dark,
                        accent,
                        appearance.glass_opacity,
                        scale,
                    ));
                }
            }
            None => {}
        }
        render::motion::appear(entrances.popup(), renderer, &mut bar, mark, scale, now);
        let replaced = entrances.replaced;
        entrances.fade_popup.track(renderer, &mut bar, mark, replaced, true, reduce, scale, now, &mut self.anim_until);
        bar.extend(strip);
        // The dock: behind the bar and popups, above windows.
        if let Some(layout) = &dock_layout {
            let mut icons: Vec<DockIcon> = dock_items
                .iter()
                .zip(&dock_frame.shades)
                .enumerate()
                .map(|(i, (item, shade))| DockIcon { name: item.icon(), windows: item.windows.len(), focused: focused_item == Some(i), shade: *shade })
                .collect();
            let zoom = if self.config.dock.zoom { f64::from(self.config.dock.zoom_percent) / 100.0 } else { 1.0 };
            let px = (layout.icon * zoom * scale).ceil() as u32;
            // The dragged icon follows the pointer: one more icon, without a column (so without dots).
            let mut layout = layout.clone();
            let held = self.dock_ui.dragged().and_then(|key| dock_items.iter().find(|i| i.key == key));
            if let Some((item, at)) = held.zip(held_at) {
                let s = layout.icon;
                layout.icons.push(smithay::utils::Rectangle::new((at.x - s / 2.0, at.y - s / 2.0).into(), (s, s).into()));
                let shade = crate::shell::dock_fx::Shade { alpha: 1.0, lighten: 0.0, darken: 0.0 };
                icons.push(DockIcon { name: item.icon(), windows: 0, focused: false, shade });
            }
            let badges: Vec<_> = dock_frame.badges.iter().map(|(i, entry)| (layout.icons[*i].to_i32_round(), *entry)).collect();
            bar.extend(dock.tooltip(renderer, shaders, &mut self.text, &mut self.symbols, dock_frame.tip.as_ref(), dark, scale));
            bar.extend(dock.badges(renderer, shaders, &mut self.text, &mut self.symbols, &badges, accent, scale));
            bar.extend(dock.elements(renderer, shaders, &mut self.icons, &icons, &layout, px, scale));
            bar.extend(DockView::poofs(shaders, &dock_frame.poofs, layout.icon, scale));
        } else {
            bar.extend(dock.tooltip(renderer, shaders, &mut self.text, &mut self.symbols, None, dark, scale));
        }
        bar.extend(DockView::glows(shaders, &dock_frame.glows, scale));
        // Launchpad: in front of the windows (hidden) and the bar, behind the dock and alerts.
        if let Some(((position, shown, field, page), grid)) = &pad {
            let apps = crate::shell::launchpad::filter(self.apps.list(), &field.text);
            let frame = LaunchpadFrame { apps: &apps, grid, output: size, position: *position, page: *page, shown: *shown, field };
            bar.extend(pad_view.elements(
                renderer,
                shaders,
                &mut self.text,
                &mut self.symbols,
                &mut self.icons,
                &frame,
                dark,
                accent,
                appearance.glass_opacity,
                scale,
            ));
        }
        // Ghosts of closing and minimising windows: behind the dock, in front of windows.
        ghosts.retain(|g| !g.anim.done(now));
        bar.extend(ghosts.iter().map(|g| g.element(renderer, now, scale)));
        if let (Some(components), Some(geo)) = (components.as_mut(), self.space.output_geometry(output)) {
            bar.extend(components.elements(renderer, shaders, &mut self.text, &mut self.symbols, geo.size, dark, accent, appearance.glass_opacity, scale));
        }
        let output_geo = self.space.output_geometry(output).unwrap_or_default();
        let windows = match (&overview, &slide) {
            (Some(overview), _) => {
                Some(mission.windows(renderer, shaders, &mut self.text, &self.space, overview, output_geo, dark, accent, appearance.glass_opacity, scale))
            }
            (None, Some(slide)) => Some(render::sliding(renderer, shaders, &mut self.text, &self.space, slide, scale, dark)),
            // Launchpad shows only the wallpaper.
            (None, None) if pad.is_some() => Some(Vec::new()),
            (None, None) => None,
        };
        let mut elements = render::scene(renderer, shaders, wallpaper, &mut self.text, &self.space, output, appearance, bar, windows);
        if let (Some(gallery), Some(geo)) = (gallery.as_mut(), self.space.output_geometry(output)) {
            let cards = gallery.elements(renderer, shaders, &mut self.text, geo.size, self.config.appearance.glass_opacity, scale);
            // Above the wallpaper (the last element), below every window.
            elements.splice(elements.len().saturating_sub(1)..elements.len().saturating_sub(1), cards);
        }
        // Desktop icons (E7.2): above the wallpaper, below every window; Launchpad's dim covers them.
        if let Some(frame) = &desktop_frame {
            let icons = desktop_view.elements(renderer, shaders, &mut self.text, &mut self.icons, frame, dark, accent, scale);
            elements.splice(elements.len().saturating_sub(1)..elements.len().saturating_sub(1), icons);
        }
        // Locked (E6.4): only the lock screen, ours or a client's, over the wallpaper.
        // ponytail: the desktop's elements above are still built (cheap, and only on a redraw); skip them if a
        // locked frame ever shows up in a profile.
        if let (Some(lock), Some(geo)) = (&self.lock, self.space.output_geometry(output)) {
            elements = match lock.client_surface() {
                Some(surface) => smithay::backend::renderer::element::surface::render_elements_from_surface_tree(
                    renderer,
                    surface.wl_surface(),
                    (0, 0),
                    scale,
                    1.0,
                    smithay::backend::renderer::element::Kind::Unspecified,
                )
                .into_iter()
                .map(SceneElement::Surface)
                .collect(),
                None => {
                    let content = LockContent {
                        lock,
                        battery: self.battery,
                        keyboard: &lock.keyboard,
                        hour24: self.config.clock.format.hour24,
                        now: chrono::Local::now(),
                    };
                    lock_view.elements(renderer, shaders, &mut self.text, &mut self.symbols, &content, geo.size, dark, accent, appearance.glass_opacity, scale)
                }
            };
            elements.push(wallpaper.element(shaders, geo.size, dark));
        }
        // Idle (E6.5): dimmed, or black while the screen is off.
        if let (Some(alpha), Some(geo)) = (idle_shade, self.space.output_geometry(output)) {
            elements.insert(0, SceneElement::Shader(lock_view.shade(shaders, geo.size, alpha, scale)));
        }
        let animating = gallery.is_some() || dock_frame.animating || animating;
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
        let pixels =
            damaged.filter(|&damaged| self.frame_wanted(output, damaged)).and_then(|_| crate::capture::render(backend.renderer(), output, &elements, CLEAR));
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
        for surface in self.lock.iter().flat_map(|l| &l.surfaces) {
            smithay::desktop::utils::send_frames_surface_tree(surface.wl_surface(), output, elapsed, Some(Duration::ZERO), on_output);
        }
        feedback
    }

    /// After a frame: drop what closed and send the replies.
    pub fn frame_done(&mut self) {
        self.space.refresh();
        self.popups.cleanup();
        let _ = self.display_handle.flush_clients();
    }
}
