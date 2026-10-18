//! Window motion (E5.5): while a window animates, its elements are drawn into an offscreen texture each frame and
//! that texture is drawn moved, scaled and faded; no app redraw needed. A closing or minimising window leaves a
//! `Ghost`: its last frame, animating on its own after the window is gone.

use std::{cell::RefCell, time::Instant};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, Color32F, Frame, Offscreen, Renderer,
            element::{Element, Id, Kind, RenderElement, texture::TextureRenderElement},
            gles::{GlesRenderer, GlesTexture},
        },
    },
    desktop::{Space, Window},
    utils::{Logical, Physical, Point, Rectangle, Size, Transform},
};

use super::{SceneElement, Shaders, Shadow, grow, window_elements};
use crate::{
    anim::{Anim, Entrance, Look, Motion},
    text::Text,
};

/// On a window while it animates.
struct Moving(RefCell<Option<Motion>>);

pub fn start(window: &Window, motion: Motion) {
    window.user_data().insert_if_missing(|| Moving(RefCell::new(None)));
    if let Some(moving) = window.user_data().get::<Moving>() {
        *moving.0.borrow_mut() = Some(motion);
    }
}

/// The motion `window` is in; dropped once settled.
pub fn current(window: &Window, now: Instant) -> Option<Motion> {
    let mut motion = window.user_data().get::<Moving>()?.0.borrow_mut();
    if motion.is_some_and(|m| m.anim.done(now)) {
        *motion = None;
    }
    *motion
}

/// A window's frame (our title bar plus its visible content), output-relative.
pub fn window_frame(space: &Space<Window>, window: &Window, output_loc: Point<i32, Logical>) -> Option<Rectangle<f64, Logical>> {
    let geo = space.element_geometry(window)?;
    let bar = crate::decorations::bar_height(window);
    let frame = Rectangle::new(geo.loc - output_loc - Point::from((0, bar)), geo.size + Size::from((0, bar)));
    (frame.size.w > 0 && frame.size.h > 0).then(|| frame.to_f64())
}

/// The frame grown by the deepest window shadow: everything a window draws (popups past it are clipped).
fn area(frame: Rectangle<f64, Logical>) -> Rectangle<f64, Logical> {
    let margin = Shadow::of(5, false).map_or(0, |s| s.margin());
    grow(frame.to_i32_round(), margin).to_f64()
}

/// Some elements drawn offscreen: `src` is where they are in `texture` (and on the output, physical px).
struct Shot {
    texture: GlesTexture,
    src: Rectangle<i32, Physical>,
}

/// Draws `elements` (front to back) where they are on the output into a transparent texture reaching to the far
/// corner of `area`, keeping `area`'s on-output part. Drawn in place, so elements that clip to output coordinates
/// (rounded windows) still line up. ponytail: parts left of or above the output are cut off.
fn snapshot(renderer: &mut GlesRenderer, elements: &[SceneElement], area: Rectangle<i32, Physical>, scale: f64) -> Option<Shot> {
    let size = Size::from((area.loc.x + area.size.w, area.loc.y + area.size.h));
    let src = area.intersection(Rectangle::from_size(size))?;
    let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, (size.w, size.h).into()).ok()?;
    {
        let mut target = renderer.bind(&mut texture).ok()?;
        let mut frame = renderer.render(&mut target, size, Transform::Normal).ok()?;
        frame.clear(Color32F::TRANSPARENT, &[src]).ok()?;
        for element in elements.iter().rev() {
            let geo = element.geometry(scale.into());
            if let Err(err) = element.draw(&mut frame, element.src(), geo, &[Rectangle::from_size(geo.size)], &[]) {
                tracing::debug!("snapshot: {err}");
            }
        }
        // Same GL context: the frame that samples it is ordered after this one.
        let _ = frame.finish().ok()?;
    }
    Some(Shot { texture, src })
}

/// The shot drawn so `frame` (logical, output-relative) lands on `look`.
fn quad(renderer: &GlesRenderer, shot: &Shot, frame: Rectangle<f64, Logical>, look: Look, scale: f64) -> SceneElement {
    let at = look.map(frame, shot.src.to_f64().to_logical(scale));
    let size = at.size.to_i32_round::<i32>();
    // Buffer scale 1: the source rect is in texture pixels.
    let src = Rectangle::new((f64::from(shot.src.loc.x), f64::from(shot.src.loc.y)).into(), (f64::from(shot.src.size.w), f64::from(shot.src.size.h)).into());
    SceneElement::Texture(TextureRenderElement::from_static_texture(
        Id::new(),
        renderer.context_id(),
        at.loc.to_physical(scale),
        shot.texture.clone(),
        1,
        Transform::Normal,
        Some(look.alpha as f32),
        Some(src),
        Some((size.w, size.h).into()),
        None,
        Kind::Unspecified,
    ))
}

/// `elements` of a window with frame `frame`, drawn at `look` instead; `None` if the snapshot failed.
pub fn moved(renderer: &mut GlesRenderer, elements: &[SceneElement], frame: Rectangle<f64, Logical>, look: Look, scale: f64) -> Option<SceneElement> {
    let shot = snapshot(renderer, elements, area(frame).to_physical_precise_round(scale), scale)?;
    Some(quad(renderer, &shot, frame, look, scale))
}

/// Shell pieces (popovers, banners, the OSD) drawn as one snapshot at `look(their bounds)`; unchanged if empty.
pub fn transformed(renderer: &mut GlesRenderer, elements: Vec<SceneElement>, scale: f64, look: impl Fn(Rectangle<f64, Logical>) -> Look) -> Vec<SceneElement> {
    let Some(bounds) = elements.iter().map(|e| e.geometry(scale.into())).reduce(|a, b| a.merge(b)) else { return elements };
    match snapshot(renderer, &elements, bounds, scale) {
        Some(shot) => {
            let area = shot.src.to_f64().to_logical(scale);
            vec![quad(renderer, &shot, area, look(area), scale)]
        }
        None => elements,
    }
}

/// When the open popup, the OSD and the newest banner appeared.
// ponytail: entrances only; they still vanish at once (a 150 ms fade out needs their last elements kept). A new
// banner slides the whole stack in, not just itself.
#[derive(Default)]
pub struct Entrances {
    popup: Option<(std::mem::Discriminant<crate::shell::Popup>, Entrance)>,
    osd: Option<Entrance>,
    banner: Option<(u32, Entrance)>,
}

impl Entrances {
    /// Starts an entrance for whatever just appeared, keeping frames coming (`until`) while it plays.
    pub fn update(&mut self, popup: Option<crate::shell::Popup>, osd: bool, banner: Option<u32>, reduce: bool, now: Instant, until: &mut Option<Instant>) {
        let mut new = |slide| {
            let e = Entrance { start: now, reduce, slide };
            crate::anim::extend(until, e.end());
            e
        };
        // Switching between menus while one is open (or the calendar's month) is not a new entrance.
        let kind = popup.as_ref().map(std::mem::discriminant);
        if kind != self.popup.map(|(k, _)| k) {
            self.popup = kind.map(|k| (k, new(false)));
        }
        if osd != self.osd.is_some() {
            self.osd = osd.then(|| new(false));
        }
        if let Some(id) = banner.filter(|id| self.banner.is_none_or(|(old, _)| id > &old)) {
            self.banner = Some((id, new(true)));
        }
    }

    pub fn popup(&self) -> Option<Entrance> {
        self.popup.map(|(_, e)| e)
    }

    pub fn osd(&self) -> Option<Entrance> {
        self.osd
    }

    pub fn banner(&self) -> Option<Entrance> {
        self.banner.map(|(_, e)| e)
    }
}

/// While `entrance` plays, `bar[from..]` (one shell piece) is drawn at its look.
pub fn appear(entrance: Option<Entrance>, renderer: &mut GlesRenderer, bar: &mut Vec<SceneElement>, from: usize, scale: f64, now: Instant) {
    if let Some(e) = entrance.filter(|e| now < e.end()) {
        let piece = bar.split_off(from.min(bar.len()));
        bar.extend(transformed(renderer, piece, scale, |rect| e.look(rect, now)));
    }
}

/// What a window looked like, to animate after it is gone.
pub struct Ghost {
    pub window: Window,
    shot: Shot,
    frame: Rectangle<f64, Logical>,
    pub from: Look,
    pub to: Look,
    pub anim: Anim,
}

impl Ghost {
    /// Snapshots `window` as it is drawn now; `from`/`to`/`anim` are set by the caller.
    #[expect(clippy::too_many_arguments, reason = "what drawing a window takes")]
    pub fn capture(
        renderer: &mut GlesRenderer,
        shaders: &Shaders,
        text: &mut Text,
        space: &Space<Window>,
        window: &Window,
        output_loc: Point<i32, Logical>,
        scale: f64,
        dark: bool,
    ) -> Option<Ghost> {
        let frame = window_frame(space, window, output_loc)?;
        let elements = window_elements(renderer, shaders, text, space, window, output_loc, scale, dark);
        let shot = snapshot(renderer, &elements, area(frame).to_physical_precise_round(scale), scale)?;
        let look = Look::at(frame);
        Some(Ghost { window: window.clone(), shot, frame, from: look, to: look, anim: Anim::new(crate::anim::Curve::Ease(crate::anim::FADE_OUT)) })
    }

    pub fn frame(&self) -> Rectangle<f64, Logical> {
        self.frame
    }

    pub fn look(&self, now: Instant) -> Look {
        self.from.lerp(self.to, self.anim.progress(now))
    }

    pub fn element(&self, renderer: &GlesRenderer, now: Instant, scale: f64) -> SceneElement {
        quad(renderer, &self.shot, self.frame, self.look(now), scale)
    }
}
