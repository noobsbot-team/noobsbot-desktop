//! The visual engine (E1.4, E1.5): glass, rounded-corner, shadow and wallpaper shaders, and the scene of an output.
//! Shaders are compiled once; shadow and wallpaper elements are kept between frames, so a frame where nothing moved
//! damages nothing and an idle desktop draws nothing.

mod alert;
pub mod calendar;
mod control;
mod force_quit;
mod gallery;
mod menu;
pub mod menubar;
mod osd;
mod switcher;
mod titlebar;
mod wallpaper;

use std::cell::RefCell;

use smithay::{
    backend::renderer::{
        Texture,
        element::{
            AsRenderElements, Element, Id, Kind, RenderElement,
            memory::MemoryRenderBufferRenderElement,
            render_elements,
            surface::{WaylandSurfaceRenderElement, WaylandSurfaceTexture, render_elements_from_surface_tree},
            texture::TextureRenderElement,
        },
        gles::{
            GlesError, GlesFrame, GlesPixelProgram, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName, UniformType, element::PixelShaderElement,
        },
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    desktop::{PopupManager, Space, Window, layer_map_for_output},
    output::Output,
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel,
    utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform},
    wayland::shell::wlr_layer::Layer,
};
pub use gallery::Gallery;
pub use alert::AlertSheet;
pub use calendar::CalendarPopover;
pub use control::ControlCenter;
pub use force_quit::ForceQuitWindow;
pub use menu::MenuPopover;
pub use menubar::MenuBar;
pub use osd::OsdPopover;
pub use switcher::SwitcherPanel;
pub use wallpaper::Wallpaper;

use crate::{
    config::{Accent, Appearance, Mode},
    text::Text,
    wm::Rect,
};

/// D5: window corner radius.
pub const WINDOW_RADIUS: f64 = 10.0;

render_elements! {
    /// Everything one output frame is made of.
    pub SceneElement<=GlesRenderer>;
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    Rounded=RoundedElement,
    Shader=PixelShaderElement,
    Texture=TextureRenderElement<GlesTexture>,
    Memory=MemoryRenderBufferRenderElement<GlesRenderer>,
}

/// Dark mode on? ponytail: `auto` is light until the Settings schedule exists (M7).
pub fn is_dark(appearance: &Appearance) -> bool {
    appearance.mode == Mode::Dark
}

/// The accent colour (Part D's macOS set) in light or dark mode.
pub fn accent(accent: Accent, dark: bool) -> [u8; 3] {
    let (light, dark_rgb) = match accent {
        Accent::Blue => (0x007AFF, 0x0A84FF),
        Accent::Purple => (0xAF52DE, 0xBF5AF2),
        Accent::Pink => (0xFF2D55, 0xFF375F),
        Accent::Red => (0xFF3B30, 0xFF453A),
        Accent::Orange => (0xFF9500, 0xFF9F0A),
        Accent::Yellow => (0xFFCC00, 0xFFD60A),
        Accent::Green => (0x34C759, 0x30D158),
        Accent::Graphite => (0x8E8E93, 0x98989D),
    };
    let rgb: u32 = if dark { dark_rgb } else { light };
    [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8]
}

/// The four compiled shaders.
pub struct Shaders {
    glass: GlesPixelProgram,
    rounded: GlesTexProgram,
    shadow: GlesPixelProgram,
    opal: GlesPixelProgram,
}

fn floats(names: &[&'static str]) -> Vec<UniformName<'static>> {
    names.iter().map(|n| UniformName::new(*n, UniformType::_1f)).collect()
}

impl Shaders {
    pub fn new(renderer: &mut GlesRenderer) -> Result<Self, GlesError> {
        let mut glass = floats(&["px", "inset", "radius", "sheen", "highlight", "highlight_top", "hairline"]);
        glass.push(UniformName::new("fill", UniformType::_4f));
        let rounded =
            [UniformName::new("geo_size", UniformType::_2f), UniformName::new("radius", UniformType::_1f), UniformName::new("to_geo", UniformType::_4f)];
        let mut opal = vec![UniformName::new("base", UniformType::_3f)];
        opal.extend(["blob1", "blob2", "blob3", "blob4"].map(|n| UniformName::new(n, UniformType::_4f)));
        Ok(Self {
            glass: renderer.compile_custom_pixel_shader(include_str!("glass.frag"), &glass)?,
            rounded: renderer.compile_custom_texture_shader(include_str!("rounded.frag"), &rounded)?,
            shadow: renderer.compile_custom_pixel_shader(
                include_str!("shadow.frag"),
                &floats(&["scale", "margin", "offset_y", "sigma", "radius", "opacity", "hairline"]),
            )?,
            opal: renderer.compile_custom_pixel_shader(include_str!("opal.frag"), &opal)?,
        })
    }

    /// A glass surface (D3) filling `rect` (output-relative, logical). Keep the element and `resize` it: a new one is
    /// a new damage id. Its shadow is a separate `shadow(rect, spec.radius, spec.elevation, dark, scale, false)`.
    pub fn glass(&self, rect: Rect, material: Material, dark: bool, glass_opacity: f32, scale: f64) -> PixelShaderElement {
        let spec = material.spec(dark);
        let [r, g, b] = spec.tint.map(|c| f32::from(c) / 255.0);
        self.glass_shape(rect, [r, g, b, spec.opacity * glass_opacity], spec.radius, true, dark, scale)
    }

    /// A rounded rect in the glass shader with any fill; `effects` adds the D3 sheen, highlights and hairline.
    pub fn glass_shape(&self, rect: Rect, fill: [f32; 4], radius: f64, effects: bool, dark: bool, scale: f64) -> PixelShaderElement {
        let (sheen, highlight, highlight_top, hairline) = match (effects, dark) {
            (false, _) => (0.0, 0.0, 0.0, 0.0),
            (true, true) => (0.03, 0.12, 0.18, 0.50),
            (true, false) => (0.04, 0.25, 0.40, 0.10),
        };
        self.glass_element(rect, fill, radius, [sheen, highlight, highlight_top, hairline], scale)
    }

    /// A traffic light (D2): a flat circle, edged 0.5 px darker only on a focused window.
    pub fn light(&self, rect: Rect, fill: [f32; 4], edge: bool, scale: f64) -> PixelShaderElement {
        let radius = f64::from(rect.size.w) / 2.0;
        self.glass_element(rect, fill, radius, [0.0, 0.0, 0.0, if edge { 0.15 } else { 0.0 }], scale)
    }

    fn glass_element(
        &self,
        rect: Rect,
        [r, g, b, a]: [f32; 4],
        radius: f64,
        [sheen, highlight, highlight_top, hairline]: [f32; 4],
        scale: f64,
    ) -> PixelShaderElement {
        let px = scale as f32;
        let uniforms = vec![
            Uniform::new("fill", (r, g, b, a)),
            Uniform::new("px", px),
            Uniform::new("inset", px),
            Uniform::new("radius", radius as f32 * px),
            Uniform::new("sheen", sheen),
            Uniform::new("highlight", highlight),
            Uniform::new("highlight_top", highlight_top),
            Uniform::new("hairline", hairline),
        ];
        PixelShaderElement::new(self.glass.clone(), grow(rect, 1), None, 1.0, uniforms, Kind::Unspecified)
    }

    /// The elevation shadow (D6) of a rounded rect, with the 0.5 px window hairline when `hairline`; `None` at level 0.
    pub fn shadow(&self, rect: Rect, radius: f64, elevation: u8, dark: bool, scale: f64, hairline: bool) -> Option<PixelShaderElement> {
        let shadow = Shadow::of(elevation, dark)?;
        let s = scale as f32;
        let hairline = if !hairline {
            0.0
        } else if dark {
            0.50
        } else {
            0.10
        };
        let uniforms = vec![
            Uniform::new("scale", s),
            Uniform::new("margin", shadow.margin() as f32 * s),
            Uniform::new("offset_y", shadow.offset_y * s),
            Uniform::new("sigma", shadow.blur / 2.0 * s),
            Uniform::new("radius", radius as f32 * s),
            Uniform::new("opacity", shadow.opacity),
            Uniform::new("hairline", hairline),
        ];
        Some(PixelShaderElement::new(self.shadow.clone(), shadow.area(rect), None, 1.0, uniforms, Kind::Unspecified))
    }

    /// The built-in opal wallpaper (D11) over `area`.
    fn opal(&self, area: Rect, dark: bool) -> PixelShaderElement {
        let rgba = |r: u8, g: u8, b: u8, a: f32| (f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0, a);
        let (base, blobs) = if dark {
            (rgba(0x15, 0x16, 0x1A, 1.0), [rgba(52, 78, 128, 0.85), rgba(104, 52, 84, 0.8), rgba(40, 92, 72, 0.8), rgba(70, 72, 82, 0.7)])
        } else {
            (rgba(0xF4, 0xF3, 0xF1, 1.0), [rgba(176, 204, 244, 0.85), rgba(246, 196, 218, 0.85), rgba(194, 232, 210, 0.9), rgba(255, 255, 255, 0.9)])
        };
        let mut uniforms = vec![Uniform::new("base", (base.0, base.1, base.2))];
        uniforms.extend(["blob1", "blob2", "blob3", "blob4"].into_iter().zip(blobs).map(|(n, c)| Uniform::new(n, c)));
        PixelShaderElement::new(self.opal.clone(), area, Some(vec![area]), 1.0, uniforms, Kind::Unspecified)
    }
}

pub(super) fn grow(rect: Rect, by: i32) -> Rect {
    Rectangle::new(rect.loc - Point::from((by, by)), (rect.size.w + 2 * by, rect.size.h + 2 * by).into())
}

/// D3 glass materials.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Material {
    Bar,
    Panel,
    Popover,
    Card,
    Hud,
    Sheet,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MaterialSpec {
    pub tint: [u8; 3],
    pub opacity: f32,
    pub radius: f64,
    pub elevation: u8,
}

impl Material {
    pub const ALL: [Material; 6] = [Material::Bar, Material::Panel, Material::Popover, Material::Card, Material::Hud, Material::Sheet];

    /// Tint, opacity, radius and elevation from the Materials board.
    pub fn spec(self, dark: bool) -> MaterialSpec {
        let (light_tint, dark_tint, light_opacity, dark_opacity, radius, elevation) = match self {
            Material::Bar => (0xF6, 0x24, 0.85, 0.82, 0.0, 1),
            Material::Panel => (0xF2, 0x26, 0.88, 0.85, 16.0, 3),
            Material::Popover => (0xFA, 0x2C, 0.92, 0.90, 8.0, 3),
            Material::Card => (0xFF, 0x32, 0.90, 0.88, 12.0, 2),
            Material::Hud => (0xED, 0x1C, 0.88, 0.85, 12.0, 3),
            Material::Sheet => (0xFF, 0x2A, 0.96, 0.95, 12.0, 5),
        };
        let (grey, opacity) = if dark { (dark_tint, dark_opacity) } else { (light_tint, light_opacity) };
        MaterialSpec { tint: [grey; 3], opacity, radius, elevation }
    }
}

/// One step of the D6 elevation scale, in logical px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shadow {
    pub offset_y: f32,
    /// CSS blur radius; the Gaussian sigma is half of it.
    pub blur: f32,
    pub opacity: f32,
}

impl Shadow {
    /// Level 0 is flat: no shadow.
    pub fn of(elevation: u8, dark: bool) -> Option<Shadow> {
        let (offset_y, blur, light, dark_opacity) = match elevation {
            1 => (1.0, 4.0, 0.10, 0.30),
            2 => (4.0, 12.0, 0.15, 0.40),
            3 => (8.0, 24.0, 0.20, 0.50),
            4 => (8.0, 30.0, 0.22, 0.50),
            5.. => (22.0, 60.0, 0.35, 0.60),
            0 => return None,
        };
        Some(Shadow { offset_y, blur, opacity: if dark { dark_opacity } else { light } })
    }

    /// How far the shadow (and the hairline) reaches past the rect on any side: 3σ (1.5 × blur, like CSS), where
    /// the falloff is under 1% of the opacity. At 2σ it is still ~3%, a visible edge on light wallpapers.
    pub fn margin(&self) -> i32 {
        (1.5 * self.blur + self.offset_y).ceil() as i32 + 1
    }

    /// The element area for a shadow under `rect`.
    pub fn area(&self, rect: Rect) -> Rect {
        grow(rect, self.margin())
    }
}

/// A window surface drawn through `rounded.frag`, clipped to its window's rounded geometry.
#[derive(Debug)]
pub struct RoundedElement {
    inner: WaylandSurfaceRenderElement<GlesRenderer>,
    program: GlesTexProgram,
    /// The window's visible geometry, output-relative physical px.
    geo: Rectangle<i32, Physical>,
    radius: f64,
}

impl Element for RoundedElement {
    fn id(&self) -> &Id {
        self.inner.id()
    }
    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }
    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }
    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }
    fn transform(&self) -> Transform {
        self.inner.transform()
    }
    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }
    /// The surface's opaque parts minus the corners, so whatever is behind a corner still gets drawn.
    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        let geo = Rectangle::new(self.geo.loc - self.inner.geometry(scale).loc, self.geo.size);
        let r = self.radius.ceil() as i32;
        let (w, h) = ((geo.size.w - 2 * r).max(0), (geo.size.h - 2 * r).max(0));
        let cross =
            [Rectangle::new(geo.loc + Point::from((r, 0)), (w, geo.size.h).into()), Rectangle::new(geo.loc + Point::from((0, r)), (geo.size.w, h).into())];
        self.inner.opaque_regions(scale).iter().flat_map(|o| cross.iter().filter_map(|c| o.intersection(*c))).collect()
    }
    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }
    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl RenderElement<GlesRenderer> for RoundedElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        // ponytail: rotated/flipped client buffers draw unclipped (rare); invert the buffer transform here if one shows up.
        let WaylandSurfaceTexture::Texture(texture) = self.inner.texture() else { return self.inner.draw(frame, src, dst, damage, opaque_regions) };
        if self.inner.transform() != Transform::Normal || src.size.w <= 0.0 || src.size.h <= 0.0 {
            return self.inner.draw(frame, src, dst, damage, opaque_regions);
        }
        let to_geo = to_geo(texture.size(), texture.is_y_inverted(), src, dst, self.geo.loc);
        let uniforms = vec![
            Uniform::new("geo_size", (self.geo.size.w as f32, self.geo.size.h as f32)),
            Uniform::new("radius", self.radius as f32),
            Uniform::new("to_geo", to_geo),
        ];
        frame.override_default_tex_program(self.program.clone(), uniforms);
        let drawn = self.inner.draw(frame, src, dst, damage, opaque_regions);
        frame.clear_tex_program_override();
        drawn
    }
}

/// The (scale, offset) that maps Smithay's texture coordinates for `src` drawn at `dst` to a position relative to
/// `geo_loc`, in physical px: `p = v * (x, y) + (z, w)`. Inverts `build_texture_mat` for an untransformed buffer.
fn to_geo(
    tex: smithay::utils::Size<i32, Buffer>,
    y_inverted: bool,
    src: Rectangle<f64, Buffer>,
    dst: Rectangle<i32, Physical>,
    geo_loc: Point<i32, Physical>,
) -> (f32, f32, f32, f32) {
    let (sx, sy) = (f64::from(dst.size.w) / src.size.w, f64::from(dst.size.h) / src.size.h);
    let ay = f64::from(tex.h) * sy;
    let x = (f64::from(tex.w) * sx, f64::from(dst.loc.x - geo_loc.x) - src.loc.x * sx);
    let y = (if y_inverted { -ay } else { ay }, f64::from(dst.loc.y - geo_loc.y) - src.loc.y * sy);
    (x.0 as f32, y.0 as f32, x.1 as f32, y.1 as f32)
}

/// Kept on each window between frames, so its shadow keeps its damage id.
struct ShadowCache(RefCell<Option<(ShadowKey, PixelShaderElement)>>);

/// Elevation, dark, scale bits.
type ShadowKey = (u8, bool, u64);

fn window_shadow(shaders: &Shaders, window: &Window, rect: Rect, elevation: u8, dark: bool, scale: f64) -> Option<PixelShaderElement> {
    window.user_data().insert_if_missing(|| ShadowCache(RefCell::new(None)));
    let mut cache = window.user_data().get::<ShadowCache>()?.0.borrow_mut();
    let key = (elevation, dark, scale.to_bits());
    match cache.as_mut() {
        Some((k, element)) if *k == key => element.resize(Shadow::of(elevation, dark)?.area(rect), None),
        _ => *cache = Some((key, shaders.shadow(rect, WINDOW_RADIUS, elevation, dark, scale, true)?)),
    }
    cache.as_ref().map(|(_, element)| element.clone())
}

/// One window, front to back: popups, its surfaces clipped to rounded corners, our title bar if it has one, its
/// shadow (5 focused, 4 not). Fullscreen windows get neither corners nor shadow.
#[allow(clippy::too_many_arguments)]
fn window_elements(
    renderer: &mut GlesRenderer,
    shaders: &Shaders,
    text: &mut Text,
    space: &Space<Window>,
    window: &Window,
    output_loc: Point<i32, Logical>,
    scale: f64,
    dark: bool,
) -> Vec<SceneElement> {
    let (Some(toplevel), Some(geo)) = (window.toplevel(), space.element_geometry(window)) else { return Vec::new() };
    let bar = crate::decorations::bar_height(window);
    let state = toplevel.current_state();
    let surface = toplevel.wl_surface();
    let geo = Rectangle::new(geo.loc - output_loc, geo.size);
    let loc = (geo.loc - window.geometry().loc).to_physical_precise_round(scale);

    let mut out: Vec<SceneElement> = PopupManager::popups_for_surface(surface)
        .flat_map(|(popup, offset)| {
            let offset = (window.geometry().loc + offset - popup.geometry().loc).to_physical_precise_round(scale);
            render_elements_from_surface_tree(renderer, popup.wl_surface(), loc + offset, scale, 1.0, Kind::Unspecified)
        })
        .collect();
    let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = render_elements_from_surface_tree(renderer, surface, loc, scale, 1.0, Kind::Unspecified);
    // Fullscreen, or not drawn yet (mapped at 0,0 before its first buffer): no corners, no shadow.
    if state.states.contains(xdg_toplevel::State::Fullscreen) || surfaces.is_empty() || geo.size.w <= 0 || geo.size.h <= 0 {
        out.extend(surfaces.into_iter().map(SceneElement::Surface));
        return out;
    }
    // The frame: content plus our title bar. Content is clipped to the frame's corners, so under a title bar only its
    // bottom corners round.
    let frame = Rectangle::new(geo.loc - Point::from((0, bar)), geo.size + Size::from((0, bar)));
    let frame_px = frame.to_physical_precise_round(scale);
    out.extend(
        surfaces
            .into_iter()
            .map(|inner| SceneElement::Rounded(RoundedElement { inner, program: shaders.rounded.clone(), geo: frame_px, radius: WINDOW_RADIUS * scale })),
    );
    let focused = state.states.contains(xdg_toplevel::State::Activated);
    if bar > 0 {
        out.extend(titlebar::elements(renderer, shaders, text, window, frame, focused, dark, scale));
    }
    let elevation = if focused { 5 } else { 4 };
    out.extend(window_shadow(shaders, window, frame, elevation, dark, scale).map(SceneElement::Shader));
    out
}

#[allow(clippy::too_many_arguments)]
/// Every element of one output frame, front to back: top/overlay layers, `bar` (the menu bar), windows (topmost first)
/// with their shadows, background/bottom layers, wallpaper.
pub fn scene(
    renderer: &mut GlesRenderer,
    shaders: &Shaders,
    wallpaper: &mut Wallpaper,
    text: &mut Text,
    space: &Space<Window>,
    output: &Output,
    appearance: &Appearance,
    bar: Vec<SceneElement>,
) -> Vec<SceneElement> {
    let Some(output_geo) = space.output_geometry(output) else { return Vec::new() };
    let scale = output.current_scale().fractional_scale();
    let dark = is_dark(appearance);
    let layers = layer_map_for_output(output);
    let (lower, upper): (Vec<_>, Vec<_>) = layers.layers().rev().partition(|l| matches!(l.layer(), Layer::Background | Layer::Bottom));
    let layer_elements = |renderer: &mut GlesRenderer, list: Vec<_>| -> Vec<SceneElement> {
        list.into_iter()
            .filter_map(|l| layers.layer_geometry(l).map(|geo| (geo.loc, l)))
            .flat_map(|(loc, l)| {
                AsRenderElements::<GlesRenderer>::render_elements::<SceneElement>(l, renderer, loc.to_physical_precise_round(scale), Scale::from(scale), 1.0)
            })
            .collect()
    };

    let mut out = layer_elements(renderer, upper);
    out.extend(bar);
    for window in space.elements().rev() {
        out.extend(window_elements(renderer, shaders, text, space, window, output_geo.loc, scale, dark));
    }
    out.extend(layer_elements(renderer, lower));
    out.push(wallpaper.element(shaders, output_geo.size, dark));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elevation_scale_matches_d6() {
        assert_eq!(Shadow::of(0, false), None);
        assert_eq!(Shadow::of(4, false), Some(Shadow { offset_y: 8.0, blur: 30.0, opacity: 0.22 }));
        assert_eq!(Shadow::of(5, true), Some(Shadow { offset_y: 22.0, blur: 60.0, opacity: 0.60 }));
        assert_eq!(Shadow::of(9, false), Shadow::of(5, false));
        // Elevation 5 reaches 22 + 1.5 × 60 px below the window, plus the hairline.
        let area = Shadow::of(5, false).unwrap().area(Rectangle::new((100, 100).into(), (400, 300).into()));
        assert_eq!(area, Rectangle::new((-13, -13).into(), (626, 526).into()));
    }

    #[test]
    fn materials_match_d3() {
        let all = [Material::Bar, Material::Panel, Material::Popover, Material::Card, Material::Hud, Material::Sheet];
        assert!(all.iter().all(|m| m.spec(true).opacity < m.spec(false).opacity && m.spec(true).tint < m.spec(false).tint));
        assert_eq!(Material::Bar.spec(false), MaterialSpec { tint: [0xF6; 3], opacity: 0.85, radius: 0.0, elevation: 1 });
        assert_eq!(Material::Sheet.spec(true), MaterialSpec { tint: [0x2A; 3], opacity: 0.95, radius: 12.0, elevation: 5 });
    }

    #[test]
    fn texture_coords_map_to_window_geometry() {
        // A 200×100 buffer drawn 1:1 at (50, 40); the window geometry starts 10 px into it (client-side shadow).
        let src = Rectangle::new((0.0, 0.0).into(), (200.0, 100.0).into());
        let dst = Rectangle::new((50, 40).into(), (200, 100).into());
        let (ax, ay, bx, by) = to_geo((200, 100).into(), false, src, dst, (60, 50).into());
        let map = |u: f32, v: f32| (u * ax + bx, v * ay + by);
        assert_eq!(map(0.0, 0.0), (-10.0, -10.0));
        assert_eq!(map(1.0, 1.0), (190.0, 90.0));
        // A cropped, 2× scaled view: src (50, 0) 100×100 drawn into 200×200.
        let src = Rectangle::new((50.0, 0.0).into(), (100.0, 100.0).into());
        let dst = Rectangle::new((0, 0).into(), (200, 200).into());
        let (ax, _, bx, _) = to_geo((200, 100).into(), false, src, dst, (0, 0).into());
        assert_eq!((0.25 * ax + bx, 0.75 * ax + bx), (0.0, 200.0));
    }
}
