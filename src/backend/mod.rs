//! Backends: `winit` runs nested in a window (`--nested`), `udev` on the real screen (E6.1). Both draw the same frame
//! (`State::frame_elements` in winit.rs) with the shell's caches below.

pub mod cursor;
pub mod udev;
pub mod winit;

/// What shows where nothing is drawn.
const CLEAR: [f32; 4] = [0.1, 0.1, 0.1, 1.0];

use smithay::{
    backend::{
        allocator::format::FormatSet,
        drm::DrmNode,
        renderer::gles::{GlesError, GlesRenderer},
    },
    wayland::dmabuf::DmabufFeedbackBuilder,
};

use crate::{
    render::{
        self, AlertSheet, Components, ControlCenter, DesktopView, DockView, DrawerView, EmojiPicker, ForceQuitWindow, Gallery, LaunchpadView, LockView,
        MenuBar, MenuPopover, MissionView, NotificationViews, OsdPopover, ScreenshotView, Shaders, SpotlightPanel, SwitcherPanel, Wallpaper,
    },
    state::State,
};

/// The shell's drawing caches (and the rects its hit tests read), made with the primary renderer.
pub struct Views {
    pub shaders: Shaders,
    pub wallpaper: Wallpaper,
    /// The `--test-scene` material gallery (E1.4), drawn over the wallpaper when set.
    pub gallery: Option<Gallery>,
    /// The `--show gallery` component gallery (E2.1), over the whole output under the bar and popups.
    pub components: Option<Components>,
    pub menu_bar: MenuBar,
    pub notifications: NotificationViews,
    /// The banner sliding in, drawn apart from the stack under it.
    pub entering: NotificationViews,
    pub menu: MenuPopover,
    pub osd: OsdPopover,
    pub control: ControlCenter,
    pub alert: AlertSheet,
    pub dock: DockView,
    pub switcher: SwitcherPanel,
    pub force_quit: ForceQuitWindow,
    pub spotlight: SpotlightPanel,
    pub emoji: EmojiPicker,
    pub launchpad: LaunchpadView,
    pub drawer: DrawerView,
    pub mission: MissionView,
    /// Closing and minimising windows still animating (E5.5).
    pub ghosts: Vec<render::motion::Ghost>,
    /// Minimised windows' last frames, drawn on their dock tiles.
    pub tiles: Vec<render::motion::Ghost>,
    pub entrances: render::motion::Entrances,
    pub lock: LockView,
    pub screenshot: ScreenshotView,
    pub desktop: DesktopView,
}

impl Views {
    /// Shaders compile once, with the renderer everything is drawn with.
    pub fn new(renderer: &mut GlesRenderer, wallpaper: Wallpaper) -> Result<Self, GlesError> {
        Ok(Self {
            shaders: Shaders::new(renderer)?,
            wallpaper,
            gallery: None,
            components: None,
            menu_bar: MenuBar::default(),
            notifications: NotificationViews::default(),
            entering: NotificationViews::default(),
            menu: MenuPopover::default(),
            osd: OsdPopover::default(),
            control: ControlCenter::default(),
            alert: AlertSheet::default(),
            dock: DockView::default(),
            switcher: SwitcherPanel::default(),
            force_quit: ForceQuitWindow::default(),
            spotlight: SpotlightPanel::default(),
            emoji: EmojiPicker::default(),
            launchpad: LaunchpadView::default(),
            drawer: DrawerView::default(),
            mission: MissionView::default(),
            ghosts: Vec::new(),
            tiles: Vec::new(),
            entrances: Default::default(),
            lock: LockView::default(),
            screenshot: ScreenshotView::default(),
            desktop: DesktopView::default(),
        })
    }
}

/// linux-dmabuf v4 tells apps which GPU and formats we import (E1.1); without a render node, v3 still works.
pub fn dmabuf_global(state: &mut State, formats: FormatSet, node: Option<DrmNode>) {
    if let Some(node) = node {
        crate::sync::init(state, node);
    }
    match node.map(|node| DmabufFeedbackBuilder::new(node.dev_id(), formats.clone()).build()) {
        Some(Ok(feedback)) => {
            state.dmabuf_state.create_global_with_default_feedback::<State>(&state.display_handle, &feedback);
        }
        other => {
            tracing::warn!("no render node for dmabuf feedback ({other:?}), using linux-dmabuf v3");
            state.dmabuf_state.create_global::<State>(&state.display_handle, formats);
        }
    }
}

/// What owns the renderer and the outputs.
pub enum Device {
    Winit(Box<winit::Winit>),
    Udev(Box<udev::Udev>),
}

impl Device {
    /// The renderer the shell's caches and imported buffers belong to.
    pub fn renderer(&mut self) -> Option<&mut GlesRenderer> {
        match self {
            Self::Winit(winit) => Some(winit.backend.renderer()),
            Self::Udev(udev) => udev.renderer(),
        }
    }
}
