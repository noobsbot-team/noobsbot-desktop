//! Our title bar (E1.3, D2): a glass strip with the traffic lights and the centred title.

use std::{cell::RefCell, sync::Arc};

use smithay::{
    backend::renderer::{
        element::{Kind, memory::MemoryRenderBufferRenderElement},
        gles::{GlesRenderer, element::PixelShaderElement},
    },
    desktop::Window,
    utils::{Point, Rectangle, Size},
    wayland::{compositor::with_states, shell::xdg::XdgToplevelSurfaceData},
};

use super::{Material, SceneElement, Shaders, WINDOW_RADIUS};
use crate::{
    decorations::{BAR, FIRST_LIGHT, LIGHT, LIGHT_STEP},
    text::{self, Rendered, Style, Text},
    wm::Rect,
};

const LIGHTS: [[u8; 3]; 3] = [[0xFF, 0x5F, 0x57], [0xFE, 0xBC, 0x2E], [0x28, 0xC8, 0x40]];
/// Title text keeps clear of the lights on both sides, so it stays centred on the bar.
const TITLE_MARGIN: i32 = 76;

/// (focused, dark, scale): what the glass elements depend on besides position.
type Key = (bool, bool, u64);

/// A window's title bar elements, kept so their damage ids survive frames (see `Shaders::glass`).
#[derive(Default)]
struct Cache {
    glass: Option<(Key, Vec<PixelShaderElement>)>,
    title: Option<Label>,
    /// × – ⤢, shown on all three lights while the pointer is over any (D2).
    glyphs: Vec<Label>,
}

type Label = (Arc<Rendered>, Option<smithay::backend::renderer::element::memory::MemoryRenderBuffer>);

const GLYPHS: [&str; 3] = ["×", "–", "⤢"];

/// A label as a render element, centred in `rect` (output-relative, logical).
fn label_element(renderer: &mut GlesRenderer, label: &Label, rect: Rect, scale: f64) -> Option<SceneElement> {
    let (rendered, Some(buffer)) = label else { return None };
    let size = Size::from(((f64::from(rendered.width) / scale).round() as i32, (f64::from(rendered.height) / scale).round() as i32));
    let loc = Point::from((rect.loc.x + (rect.size.w - size.w) / 2, rect.loc.y + (rect.size.h - size.h) / 2)).to_f64().to_physical(scale);
    MemoryRenderBufferRenderElement::from_buffer(renderer, loc, buffer, None, None, Some(size), Kind::Unspecified)
        .inspect_err(|err| tracing::warn!("title bar label: {err}"))
        .ok()
        .map(SceneElement::Memory)
}

fn title_of(window: &Window) -> String {
    let Some(toplevel) = window.toplevel() else { return String::new() };
    with_states(toplevel.wl_surface(), |s| s.data_map.get::<XdgToplevelSurfaceData>().and_then(|d| d.lock().ok()?.title.clone())).unwrap_or_default()
}

/// The title bar of `window` whose frame is `frame` (output-relative), front to back: title, lights, glass strip.
/// The strip covers the whole frame; the app's content is drawn over its lower part, so only the top `BAR` shows.
#[allow(clippy::too_many_arguments)]
pub fn elements(
    renderer: &mut GlesRenderer,
    shaders: &Shaders,
    text: &mut Text,
    window: &Window,
    frame: Rect,
    focused: bool,
    dark: bool,
    scale: f64,
) -> Vec<SceneElement> {
    window.user_data().insert_if_missing(|| RefCell::new(Cache::default()));
    let Some(cache) = window.user_data().get::<RefCell<Cache>>() else { return Vec::new() };
    let mut cache = cache.borrow_mut();

    let centre_y = frame.loc.y as f64 + f64::from(BAR) / 2.0;
    let lights: Vec<Rect> = (0..3)
        .map(|i| {
            let cx = frame.loc.x as f64 + FIRST_LIGHT + i as f64 * LIGHT_STEP;
            Rectangle::new(Point::from(((cx - LIGHT / 2.0).round() as i32, (centre_y - LIGHT / 2.0).round() as i32)), Size::from((LIGHT as i32, LIGHT as i32)))
        })
        .collect();
    let key = (focused, dark, scale.to_bits());
    match cache.glass.as_mut() {
        Some((k, glass)) if *k == key => {
            for (element, rect) in glass.iter_mut().zip(std::iter::once(frame).chain(lights.iter().copied())) {
                element.resize(super::grow(rect, 1), None);
            }
        }
        _ => {
            let spec = Material::Bar.spec(dark);
            let [r, g, b] = spec.tint.map(|c| f32::from(c) / 255.0);
            let mut glass = vec![shaders.glass_shape(frame, [r, g, b, spec.opacity], WINDOW_RADIUS, true, dark, scale)];
            for (i, rect) in lights.iter().enumerate() {
                let rgb = if focused {
                    LIGHTS[i]
                } else if dark {
                    [0x4A; 3]
                } else {
                    [0xD0; 3]
                };
                let [r, g, b] = rgb.map(|c| f32::from(c) / 255.0);
                glass.push(shaders.light(*rect, [r, g, b, 1.0], focused, scale));
            }
            cache.glass = Some((key, glass));
        }
    }

    let mut out = Vec::new();
    // label when focused, label-secondary when not (D2).
    let alpha = match (focused, dark) {
        (true, _) => 217,
        (false, false) => 128,
        (false, true) => 140,
    };
    let color = if dark { [255, 255, 255, alpha] } else { [0, 0, 0, alpha] };
    let max_width = (frame.size.w - 2 * TITLE_MARGIN).max(0) as u32;
    let rendered = text.label(&title_of(window), Style { size: 13, line_height: 16, weight: 600, color }, max_width, scale);
    if !cache.title.as_ref().is_some_and(|(r, _)| Arc::ptr_eq(r, &rendered)) {
        cache.title = Some((rendered.clone(), text::to_buffer(&rendered)));
    }
    if let Some(title) = &cache.title {
        out.extend(label_element(renderer, title, Rectangle::new(frame.loc, Size::from((frame.size.w, BAR))), scale));
    }
    if focused && crate::decorations::lights_hovered(window) {
        let style = Style { size: 10, line_height: 12, weight: 700, color: [0, 0, 0, 140] };
        let glyphs: Vec<Label> = GLYPHS
            .iter()
            .map(|g| text.label(g, style, LIGHT as u32, scale))
            .map(|r| match cache.glyphs.iter().find(|(c, _)| Arc::ptr_eq(c, &r)) {
                Some(label) => label.clone(),
                None => {
                    let buffer = text::to_buffer(&r);
                    (r, buffer)
                }
            })
            .collect();
        for (glyph, rect) in glyphs.iter().zip(&lights) {
            out.extend(label_element(renderer, glyph, *rect, scale));
        }
        cache.glyphs = glyphs;
    }
    if let Some((_, glass)) = &cache.glass {
        // Lights in front of the strip.
        out.extend(glass.iter().skip(1).cloned().map(SceneElement::Shader));
        out.extend(glass.first().cloned().map(SceneElement::Shader));
    }
    out
}
