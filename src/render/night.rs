//! Night Light (E6.2, E8.4): the whole output multiplied by a colour temperature's tint as the last pass, fading over a
//! minute. Screenshots and screen copies leave it out (capture.rs gets the scene without it).
// ponytail: a multiply over the composed frame, so it keeps a fullscreen app off direct scanout while on; the GPU
// gamma table (zero cost) when that matters. The hardware cursor is not tinted.

use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Timelike};
use smithay::{
    backend::renderer::{
        Color32F,
        element::{Element, Id, RenderElement},
        gles::{GlesError, GlesFrame, GlesRenderer, ffi},
        utils::CommitCounter,
    },
    utils::{Buffer, Physical, Point, Rectangle, Scale, Size},
};

use crate::config::{NightLight as Settings, NightSchedule};

const FADE: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub struct NightLight {
    strength: f32,
    last: Instant,
    /// The Control Center's choice and the schedule it went against; dropped when the schedule turns.
    manual: Option<(bool, bool)>,
    id: Id,
    commit: CommitCounter,
    drawn: [f32; 3],
}

impl Default for NightLight {
    fn default() -> Self {
        Self { strength: 0.0, last: Instant::now(), manual: None, id: Id::new(), commit: CommitCounter::default(), drawn: [1.0; 3] }
    }
}

/// Does the schedule want it on at `now`? Sunset falls back to the hours without a place.
pub fn scheduled(settings: &Settings, now: DateTime<Local>) -> bool {
    let sun = (settings.schedule == NightSchedule::Sunset).then(|| crate::sun::place().and_then(|p| crate::sun::is_night(now.to_utc(), p))).flatten();
    settings.enabled && sun.unwrap_or_else(|| crate::sun::within(settings.from, settings.until, now.hour()))
}

impl NightLight {
    /// On now: the schedule, or the Control Center until the schedule next turns.
    pub fn on(&self, settings: &Settings) -> bool {
        let scheduled = scheduled(settings, Local::now());
        self.manual.filter(|m| m.1 == scheduled).map_or(scheduled, |m| m.0)
    }

    pub fn toggle(&mut self, settings: &Settings) {
        let on = !self.on(settings);
        self.manual = Some((on, scheduled(settings, Local::now())));
    }

    /// This frame's tint over `size` (none while off) and whether it is still fading.
    pub fn element(&mut self, settings: &Settings, now: Instant, size: Size<i32, Physical>) -> (Option<TintElement>, bool) {
        let scheduled = scheduled(settings, Local::now());
        self.manual = self.manual.filter(|m| m.1 == scheduled);
        let target = if self.on(settings) { 1.0 } else { 0.0 };
        // At most a frame's worth: after a quiet spell the fade starts now instead of jumping.
        let dt = now.saturating_duration_since(self.last).min(Duration::from_millis(100));
        self.last = now;
        let step = dt.as_secs_f32() / FADE.as_secs_f32();
        self.strength = if target > self.strength { (self.strength + step).min(target) } else { (self.strength - step).max(target) };
        let full = kelvin(settings.temperature);
        // In 1/255 steps: a frame without a visible change damages nothing.
        let rgb = full.map(|c| ((1.0 - self.strength * (1.0 - c)) * 255.0).round() / 255.0);
        if rgb != self.drawn {
            self.drawn = rgb;
            self.commit.increment();
        }
        let fading = self.strength != target;
        let element = (self.strength > 0.0).then(|| TintElement { id: self.id.clone(), commit: self.commit, rgb, size });
        (element, fading)
    }
}

/// The colour of a `k` kelvin light, 6500 K = white (Tanner Helland's fit of the blackbody curve).
pub fn kelvin(k: u32) -> [f32; 3] {
    let raw = |k: u32| {
        let t = f64::from(k.clamp(1000, 6500)) / 100.0;
        let b = if t <= 19.0 { 0.0 } else { 138.5177 * (t - 10.0).ln() - 305.0448 };
        [255.0, 99.4708 * t.ln() - 161.1196, b]
    };
    let (rgb, white) = (raw(k), raw(6500));
    [0, 1, 2].map(|i| (rgb[i] / white[i]).clamp(0.0, 1.0) as f32)
}

/// Multiplies what is under it by `rgb`.
#[derive(Debug)]
pub struct TintElement {
    id: Id,
    commit: CommitCounter,
    rgb: [f32; 3],
    size: Size<i32, Physical>,
}

impl Element for TintElement {
    fn id(&self) -> &Id {
        &self.id
    }
    fn current_commit(&self) -> CommitCounter {
        self.commit
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size((f64::from(self.size.w), f64::from(self.size.h)).into())
    }
    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        Rectangle::new(Point::default(), self.size)
    }
}

impl RenderElement<GlesRenderer> for TintElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        let [r, g, b] = self.rgb;
        // dst × colour; the renderer's own blending is put back after.
        frame.with_context(|gl| unsafe {
            gl.Enable(ffi::BLEND);
            gl.BlendFunc(ffi::ZERO, ffi::SRC_COLOR);
        })?;
        let drawn = frame.draw_solid(dst, damage, Color32F::new(r, g, b, 1.0));
        frame.with_context(|gl| unsafe { gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA) })?;
        drawn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warmer_is_less_blue() {
        assert!(kelvin(6500).iter().all(|c| (c - 1.0).abs() < 1e-6));
        let [r, g, b] = kelvin(2700);
        assert!(r == 1.0 && g < 0.75 && b < g && b > 0.3, "{g} {b}");
        assert_eq!(kelvin(500), kelvin(1000));
    }
}
