//! Zoom and colour filters (E8.6): the output's frame drawn offscreen, then drawn back magnified around the pointer
//! (the whole screen, or a lens) and through a colour filter. Visual only: input lands where the pointer really is,
//! and full-screen zoom keeps the pointer drawn there too. Screenshots get `plain`, the frame as it was.
// ponytail: a fresh full-output texture and a full redraw every frame while on; keep one per output with its damage if
// that shows in the frame budget. The hardware cursor is neither magnified nor filtered. The view follows whichever
// moved last: the pointer, the text caret (text-input-v3) or keyboard focus (`[accessibility] zoom_follows_focus`).

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use smithay::{
    backend::renderer::{
        Renderer,
        element::{Element, Id, Kind, RenderElement, texture::TextureRenderElement},
        gles::{GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform},
};

use super::{SceneElement, Shaders, motion};
use crate::config::{Accessibility, ColourFilter, ZoomStyle};

const MAX: f64 = 16.0;
const STEP: f64 = std::f64::consts::SQRT_2;
/// The lens, logical px.
const LENS: (f64, f64) = (480.0, 320.0);

/// Zoom on or off, the level it goes to, and the factor drawn this frame (easing towards it).
pub struct Zoom {
    on: bool,
    level: f64,
    factor: f64,
    last: Instant,
    /// What each output's view follows (by name): the others stay put while the pointer is elsewhere.
    follow: HashMap<String, Follow>,
    /// Bumped when keyboard focus moves to another window.
    focus_serial: u64,
    /// The last frame without zoom and filter, for a screenshot of it; none when neither is on.
    pub plain: Option<SceneElement>,
}

impl Default for Zoom {
    fn default() -> Self {
        Self { on: false, level: 2.0, factor: 1.0, last: Instant::now(), follow: HashMap::new(), focus_serial: 0, plain: None }
    }
}

/// One output's followed point, and the pointer, caret and focus seen last: only a change moves the view.
#[derive(Default)]
struct Follow {
    at: Point<f64, Logical>,
    pointer: Option<Point<f64, Logical>>,
    caret: Option<Rectangle<f64, Logical>>,
    focus_serial: u64,
}

impl Zoom {
    /// Keyboard focus moved to another window: the view goes to it on the next frame.
    pub fn refocus(&mut self) {
        self.focus_serial += 1;
    }

    pub fn toggle(&mut self) {
        self.on = !self.on;
    }

    /// One step in or out; in from off turns Zoom on, out to 1x turns it off.
    pub fn step(&mut self, zoom_in: bool) {
        match (self.on, zoom_in) {
            (false, true) => self.on = true,
            (false, false) => {}
            (true, _) => {
                self.level = if zoom_in { self.level * STEP } else { self.level / STEP }.clamp(1.0, MAX);
                if self.level < 1.01 {
                    (self.on, self.level) = (false, 2.0);
                }
            }
        }
    }

    /// Eases the drawn factor towards its target; whether it still moves.
    pub fn advance(&mut self, now: Instant, reduce_motion: bool) -> bool {
        let target = if self.on { self.level } else { 1.0 };
        // At most a frame's worth: after a quiet spell it starts now instead of jumping.
        let dt = now.saturating_duration_since(self.last).min(Duration::from_millis(20)).as_secs_f64();
        self.last = now;
        // In log space, so every step in or out takes as long (about 200 ms).
        let k = if reduce_motion { 1.0 } else { 1.0 - (-dt * 20.0).exp() };
        self.factor = (self.factor.ln() + (target.ln() - self.factor.ln()) * k).exp();
        if (self.factor / target - 1.0).abs() < 0.002 {
            self.factor = target;
        }
        self.factor != target
    }

    /// `elements` (an output of logical `size`, front to back) magnified and filtered as `settings` say. `pointer`,
    /// the focused text field's `caret` and the focused `window` are output-relative, `None` while elsewhere.
    #[expect(clippy::too_many_arguments, reason = "the frame, where it is shown and how")]
    pub fn apply(
        &mut self,
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        elements: Vec<SceneElement>,
        output: &str,
        size: Size<f64, Logical>,
        pointer: Option<Point<f64, Logical>>,
        caret: Option<Rectangle<f64, Logical>>,
        window: Option<Rectangle<f64, Logical>>,
        settings: &Accessibility,
        scale: f64,
    ) -> Vec<SceneElement> {
        self.plain = None;
        // Kept up while off too, so turning Zoom on shows what moved last.
        let f = self.follow.entry(output.into()).or_insert_with(|| Follow { at: (size.w / 2.0, size.h / 2.0).into(), ..Follow::default() });
        if let Some(p) = pointer.filter(|p| f.pointer != Some(*p)) {
            f.at = p;
        }
        let target = match (caret, window) {
            (Some(c), _) if f.caret != Some(c) => Some(c),
            (_, Some(w)) if f.focus_serial != self.focus_serial => Some(w),
            _ => None,
        };
        if let Some(rect) = target {
            f.at = match settings.zoom_style {
                ZoomStyle::Lens => rect.loc + Point::from((rect.size.w / 2.0, rect.size.h / 2.0)),
                ZoomStyle::Full => towards(size, f.at, rect, if self.on { self.level } else { 1.0 }),
            };
        }
        (f.pointer, f.caret, f.focus_serial) = (pointer, caret, self.focus_serial);
        let pointer = f.at;
        let filter = match settings.colour_filter {
            ColourFilter::None => 0.0,
            ColourFilter::Invert => 1.0,
            ColourFilter::InvertHue => 2.0,
            ColourFilter::Greyscale => 3.0,
        };
        if self.factor <= 1.0 && filter == 0.0 {
            return elements;
        }
        let area = Rectangle::from_size(size.to_physical_precise_round(scale));
        let Some(shot) = motion::snapshot(renderer, &elements, area, scale) else { return elements };
        let (whole, context) = (Rectangle::from_size(size), renderer.context_id());
        let quad = |src: Rectangle<f64, Logical>, dst: Rectangle<f64, Logical>, filter: f32| {
            // Buffer scale 1: the source rect is in texture pixels.
            let src = src.to_physical(scale);
            let inner = TextureRenderElement::from_static_texture(
                Id::new(),
                context.clone(),
                dst.loc.to_physical(scale),
                shot.texture.clone(),
                1,
                Transform::Normal,
                None,
                Some(Rectangle::new((src.loc.x, src.loc.y).into(), (src.size.w, src.size.h).into())),
                Some(dst.size.to_i32_round()),
                None,
                Kind::Unspecified,
            );
            match filter {
                0.0 => SceneElement::Texture(inner),
                _ => SceneElement::Filter(FilterElement { inner, program: shaders.filter.clone(), filter }),
            }
        };
        self.plain = Some(quad(whole, whole, 0.0));
        let mut out = Vec::new();
        match settings.zoom_style {
            _ if self.factor <= 1.0 => {}
            ZoomStyle::Full => return vec![quad(viewport(size, pointer, self.factor), whole, filter)],
            ZoomStyle::Lens => {
                let (src, dst) = lens(size, pointer, self.factor);
                out.push(SceneElement::Shader(shaders.ring(dst.to_i32_round(), 0.0, 2, [0.45, 0.45, 0.45, 1.0], scale)));
                out.push(quad(src, dst, filter));
            }
        }
        out.push(quad(whole, whole, filter));
        out
    }
}

/// Lays a `len` span centred on `centre` inside `0 … total`.
fn place(centre: f64, len: f64, total: f64) -> f64 {
    (centre - len / 2.0).clamp(0.0, (total - len).max(0.0))
}

fn clamp(p: Point<f64, Logical>, size: Size<f64, Logical>) -> Point<f64, Logical> {
    (p.x.clamp(0.0, size.w), p.y.clamp(0.0, size.h)).into()
}

/// The part of an output of `size` shown at `factor`, following `pointer`: in proportion, so the point under the
/// pointer is drawn where the pointer is, and the edges are reached as the pointer reaches them.
pub fn viewport(size: Size<f64, Logical>, pointer: Point<f64, Logical>, factor: f64) -> Rectangle<f64, Logical> {
    let p = clamp(pointer, size);
    Rectangle::new((p.x - p.x / factor, p.y - p.y / factor).into(), (size.w / factor, size.h / factor).into())
}

/// The point to follow so that full-screen zoom at `factor` shows `rect`, moving the view from where following `at`
/// puts it as little as it can; a rect bigger than the view shows its top-left.
pub fn towards(size: Size<f64, Logical>, at: Point<f64, Logical>, rect: Rectangle<f64, Logical>, factor: f64) -> Point<f64, Logical> {
    if factor <= 1.0 {
        return rect.loc + Point::from((rect.size.w / 2.0, rect.size.h / 2.0));
    }
    let view = viewport(size, at, factor);
    let pan = |lo: f64, len: f64, r_lo: f64, r_len: f64, total: f64| {
        let lo = if r_len > len || r_lo < lo {
            r_lo
        } else if r_lo + r_len > lo + len {
            r_lo + r_len - len
        } else {
            lo
        };
        lo.clamp(0.0, (total - len).max(0.0))
    };
    let (x, y) = (pan(view.loc.x, view.size.w, rect.loc.x, rect.size.w, size.w), pan(view.loc.y, view.size.h, rect.loc.y, rect.size.h, size.h));
    // `viewport` puts the view at `at × (1 − 1/factor)`: the inverse.
    let k = 1.0 - 1.0 / factor;
    (x / k, y / k).into()
}

/// The lens: what it shows (`src`) and where (`dst`), centred on `pointer` and kept on the output.
pub fn lens(size: Size<f64, Logical>, pointer: Point<f64, Logical>, factor: f64) -> (Rectangle<f64, Logical>, Rectangle<f64, Logical>) {
    let p = clamp(pointer, size);
    let len = Size::from((LENS.0.min(size.w), LENS.1.min(size.h)));
    let dst = Rectangle::new((place(p.x, len.w, size.w), place(p.y, len.h, size.h)).into(), len);
    // The point under the pointer stays under it even where the lens is pushed off centre at an edge.
    let centre = dst.loc + Point::from((len.w / 2.0, len.h / 2.0));
    let shown = Size::from((len.w / factor, len.h / factor));
    let at = |p: f64, c: f64| p - (p - c) / factor;
    let src = Rectangle::new((place(at(p.x, centre.x), shown.w, size.w), place(at(p.y, centre.y), shown.h, size.h)).into(), shown);
    (src, dst)
}

/// The frame drawn through `filter.frag`.
#[derive(Debug)]
pub struct FilterElement {
    inner: TextureRenderElement<GlesTexture>,
    program: GlesTexProgram,
    filter: f32,
}

impl Element for FilterElement {
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
    fn transform(&self) -> Transform {
        self.inner.transform()
    }
    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }
    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl RenderElement<GlesRenderer> for FilterElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        frame.override_default_tex_program(self.program.clone(), vec![Uniform::new("mode", self.filter)]);
        let drawn = RenderElement::<GlesRenderer>::draw(&self.inner, frame, src, dst, damage, opaque_regions);
        frame.clear_tex_program_override();
        drawn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (f64, f64) = (1920.0, 1080.0);

    #[test]
    fn viewport_follows_the_pointer_inside_the_output() {
        let size = SIZE.into();
        assert_eq!(viewport(size, (500.0, 500.0).into(), 1.0), Rectangle::from_size(size));
        // Corners show corners; the centre stays centred; off the output clamps to it.
        assert_eq!(viewport(size, (0.0, 0.0).into(), 2.0), Rectangle::new((0.0, 0.0).into(), (960.0, 540.0).into()));
        assert_eq!(viewport(size, (1920.0, 1080.0).into(), 2.0).loc, (960.0, 540.0).into());
        assert_eq!(viewport(size, (960.0, 540.0).into(), 4.0).loc, (720.0, 405.0).into());
        assert_eq!(viewport(size, (-50.0, 5000.0).into(), 2.0).loc, (0.0, 540.0).into());
        // The point under the pointer is drawn where the pointer is.
        let (p, f) = (Point::<f64, Logical>::from((300.0, 700.0)), 3.0);
        let v = viewport(size, p, f);
        assert!(((p.x - v.loc.x) * f - p.x).abs() < 1e-9 && ((p.y - v.loc.y) * f - p.y).abs() < 1e-9);
    }

    #[test]
    fn towards_pans_just_enough_to_show_the_rect() {
        let size: Size<f64, Logical> = SIZE.into();
        let centre = Point::from((960.0, 540.0));
        // At 2× around the centre the view is 480…1440 × 270…810.
        let inside = Rectangle::new((600.0, 400.0).into(), (2.0, 20.0).into());
        assert_eq!(towards(size, centre, inside, 2.0), centre, "already shown: stays");
        // A caret right of the view: the view's right edge comes to it, nothing else moves.
        let right = Rectangle::new((1600.0, 400.0).into(), (2.0, 20.0).into());
        let v = viewport(size, towards(size, centre, right, 2.0), 2.0);
        assert!((v.loc.x + v.size.w - 1602.0).abs() < 1e-9 && (v.loc.y - 270.0).abs() < 1e-9);
        // A window bigger than the view: its top-left corner.
        let window = Rectangle::new((100.0, 50.0).into(), (1200.0, 900.0).into());
        assert_eq!(viewport(size, towards(size, centre, window, 2.0), 2.0).loc, (100.0, 50.0).into());
        // Unzoomed: its centre.
        assert_eq!(towards(size, centre, window, 1.0), (700.0, 500.0).into());
    }

    #[test]
    fn lens_stays_on_the_output_around_the_pointer() {
        let size = SIZE.into();
        let (src, dst) = lens(size, (960.0, 540.0).into(), 2.0);
        assert_eq!(dst, Rectangle::new((720.0, 380.0).into(), (480.0, 320.0).into()));
        assert_eq!(src, Rectangle::new((840.0, 460.0).into(), (240.0, 160.0).into()));
        // In a corner the lens is pushed in, and still shows the corner under the pointer.
        let (src, dst) = lens(size, (0.0, 0.0).into(), 2.0);
        assert_eq!((dst.loc, src.loc), ((0.0, 0.0).into(), (0.0, 0.0).into()));
        let (src, dst) = lens(size, (1920.0, 1080.0).into(), 4.0);
        assert_eq!(dst.loc + Point::from((dst.size.w, dst.size.h)), (1920.0, 1080.0).into());
        assert_eq!(src.loc + Point::from((src.size.w, src.size.h)), (1920.0, 1080.0).into());
    }

    #[test]
    fn steps_turn_zoom_on_and_off() {
        let mut z = Zoom::default();
        z.step(false);
        assert!(!z.on);
        z.step(true);
        assert!(z.on && z.level == 2.0);
        z.step(true);
        assert!((z.level - 2.0 * STEP).abs() < 1e-9);
        (0..20).for_each(|_| z.step(true));
        assert_eq!(z.level, MAX);
        z.level = 2.0;
        z.step(false);
        z.step(false);
        assert!(!z.on && z.level == 2.0);
    }
}
