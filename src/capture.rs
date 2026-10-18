//! Reading the screen back (E6.8): our own screenshots, and `wlr-screencopy-unstable-v1` for other programs (grim,
//! wf-recorder, xdg-desktop-portal-wlr's screen sharing) into their shm buffers. When someone waits, the frame just
//! drawn is drawn once more into an offscreen buffer and read (both backends, see `render`), so capturing costs
//! nothing while nobody asks. Who may capture is perms.rs's call.
// ponytail: shm copies only (a 1080p frame is 8 MB through the CPU); dmabuf frames for zero-copy recording and
// `ext-image-copy-capture` later. The cursor is never drawn in (nested it's the host's; grim -c is not honoured).

use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, ExportMem, Offscreen,
            damage::OutputDamageTracker,
            element::RenderElement,
            gles::{GlesRenderer, GlesTexture},
        },
    },
    output::Output,
    reexports::{
        wayland_protocols_wlr::screencopy::v1::server::{
            zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
            zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
            backend::{ClientId, GlobalId},
            protocol::{wl_buffer::WlBuffer, wl_shm},
        },
    },
    utils::{Physical, Point, Rectangle, Size, Transform},
    wayland::shm::with_buffer_contents_mut,
};

use crate::state::State;

/// A copy waiting for the next frame of `output` (or, `damage`, the next frame that changed).
struct Waiting {
    frame: ZwlrScreencopyFrameV1,
    output: Output,
    buffer: WlBuffer,
    region: Rectangle<i32, Physical>,
    damage: bool,
}

pub struct Capture {
    _global: GlobalId,
    waiting: Vec<Waiting>,
    /// Clients that got a frame: their next `copy_with_damage` waits for something to change.
    served: HashSet<ClientId>,
    /// The user's answer per connection (perms.rs).
    pub verdicts: HashMap<ClientId, bool>,
}

/// A frame object's output and region (output buffer px); a frame copies once.
pub struct FrameData {
    output: Option<Output>,
    region: Rectangle<i32, Physical>,
    used: AtomicBool,
}

/// One output frame read back: RGBA rows as the output's buffer holds them (before its transform, which is what
/// screencopy clients expect: they apply the transform themselves).
pub struct Pixels {
    pub size: Size<i32, Physical>,
    pub rgba: Vec<u8>,
    pub transform: Transform,
}

impl Pixels {
    /// The frame as it is seen (the output's transform applied), top row first.
    pub fn shown(&self) -> Pixels {
        let t = self.transform;
        let size = t.transform_size(self.size);
        let (w, h) = (self.size.w as usize, self.size.h as usize);
        let mut rgba = vec![0u8; w * h * 4];
        // Pixel corners: `transform_point_in` maps the area's corners, so a pixel's far corner lands on its new one.
        let last = Size::from((self.size.w - 1, self.size.h - 1));
        for y in 0..self.size.h {
            for x in 0..self.size.w {
                let to = t.transform_point_in(Point::<i32, Physical>::from((x, y)), &last);
                let (from, to) = ((y as usize * w + x as usize) * 4, (to.y as usize * size.w as usize + to.x as usize) * 4);
                rgba[to..to + 4].copy_from_slice(&self.rgba[from..from + 4]);
            }
        }
        Pixels { size, rgba, transform: Transform::Normal }
    }

    /// `region` (clipped to the frame) as its own RGBA rows.
    pub fn crop(&self, region: Rectangle<i32, Physical>) -> (Vec<u8>, Size<i32, Physical>) {
        let region = region.intersection(Rectangle::from_size(self.size)).unwrap_or_default();
        let (w, stride) = (region.size.w as usize * 4, self.size.w as usize * 4);
        let mut out = Vec::with_capacity(w * region.size.h as usize);
        for y in region.loc.y..region.loc.y + region.size.h {
            let start = y as usize * stride + region.loc.x as usize * 4;
            out.extend_from_slice(&self.rgba[start..start + w]);
        }
        (out, region.size)
    }
}

/// Draws `elements` (the frame just shown, without the cursor) once more into an offscreen texture the size of
/// `output`'s mode, with its transform, and reads it back: the same pixels the output's buffer got. Used by both
/// backends, so the nested window exercises the real screen's path; call it after the swap (nested, binding a texture
/// before it would fail the swap).
// ponytail: a fresh texture and full redraw per captured frame; keep one per output (and its damage) if recording
// ever shows up in the frame budget.
pub fn render<E: RenderElement<GlesRenderer>>(renderer: &mut GlesRenderer, output: &Output, elements: &[E], clear: [f32; 4]) -> Option<Pixels> {
    let size = output.current_mode()?.size;
    let (scale, transform) = (output.current_scale().fractional_scale(), output.current_transform());
    let read = |renderer: &mut GlesRenderer| -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, (size.w, size.h).into())?;
        let mut target = renderer.bind(&mut texture)?;
        OutputDamageTracker::new(size, scale, transform).render_output(renderer, &mut target, 0, elements, clear)?;
        let mapping = renderer.copy_framebuffer(&target, Rectangle::from_size((size.w, size.h).into()), Fourcc::Abgr8888)?;
        drop(target);
        Ok(renderer.map_texture(&mapping)?.to_vec())
    };
    let rgba = read(renderer).inspect_err(|err| tracing::warn!("screen read: {err}")).ok()?;
    Some(Pixels { size, rgba, transform })
}

impl Capture {
    pub fn new(dh: &DisplayHandle) -> Self {
        Self { _global: dh.create_global::<State, ZwlrScreencopyManagerV1, _>(3, ()), waiting: Vec::new(), served: HashSet::new(), verdicts: HashMap::new() }
    }

    /// Does the frame just drawn on `output` (`damaged`: it changed) have to be read?
    pub fn wants(&self, output: &Output, damaged: bool, allowed: impl Fn(&ClientId) -> bool) -> bool {
        self.waiting.iter().any(|w| w.output == *output && self.ready(w, damaged) && w.frame.client().is_some_and(|c| allowed(&c.id())))
    }

    fn ready(&self, w: &Waiting, damaged: bool) -> bool {
        !w.damage || damaged || w.frame.client().is_none_or(|c| !self.served.contains(&c.id()))
    }

    pub fn waiting_clients(&self) -> Vec<Client> {
        self.waiting.iter().filter_map(|w| w.frame.client()).collect()
    }

    /// A denied client's copies fail, now and later.
    pub fn fail_client(&mut self, client: &ClientId) {
        self.waiting.retain(|w| {
            let mine = w.frame.client().is_some_and(|c| c.id() == *client);
            if mine {
                w.frame.failed();
            }
            !mine
        });
    }

    /// Fills every allowed copy of `output` that this frame satisfies; `now`: the frame's presentation time (monotonic).
    pub fn deliver(&mut self, output: &Output, pixels: &Pixels, damaged: bool, now: Duration, allowed: impl Fn(&ClientId) -> bool) {
        let waiting = std::mem::take(&mut self.waiting);
        for w in waiting {
            let Some(client) = w.frame.client().map(|c| c.id()) else { continue };
            if w.output != *output || !self.ready(&w, damaged) || !allowed(&client) {
                self.waiting.push(w);
                continue;
            }
            self.served.insert(client);
            if !copy_into(&w.buffer, pixels, w.region) {
                w.frame.failed();
                continue;
            }
            // Buffer px as they are: the client applies the output's transform (wl_output.geometry).
            w.frame.flags(zwlr_screencopy_frame_v1::Flags::empty());
            if w.damage && w.frame.version() >= 2 {
                // ponytail: the whole region; the damage tracker's rects would let recorders skip unchanged parts.
                w.frame.damage(0, 0, w.region.size.w as u32, w.region.size.h as u32);
            }
            let secs = now.as_secs();
            w.frame.ready((secs >> 32) as u32, secs as u32, now.subsec_nanos());
        }
    }
}

/// `region` of `pixels` into a client's XRGB8888 shm buffer, swapping red and blue. `false`: the buffer is unusable.
fn copy_into(buffer: &WlBuffer, pixels: &Pixels, region: Rectangle<i32, Physical>) -> bool {
    let (rgba, size) = pixels.crop(region);
    let written = with_buffer_contents_mut(buffer, |ptr, len, data| {
        let (w, h, stride) = (size.w as usize, size.h as usize, data.stride as usize);
        if data.width as usize != w || data.height as usize != h || stride < w * 4 || data.offset as usize + stride * h > len {
            return false;
        }
        let mut row = vec![0u8; w * 4];
        for y in 0..h {
            for (out, px) in row.as_chunks_mut::<4>().0.iter_mut().zip(rgba[y * w * 4..(y + 1) * w * 4].as_chunks::<4>().0) {
                *out = [px[2], px[1], px[0], px[3]];
            }
            // SAFETY: bounds checked above; the pool stays mapped for the callback, and we only write (no reference is
            // made into memory the client may change at the same time).
            unsafe { std::ptr::copy_nonoverlapping(row.as_ptr(), ptr.add(data.offset as usize + y * stride), w * 4) };
        }
        true
    });
    written.unwrap_or(false)
}

impl State {
    /// After drawing `output`'s frame: hands the pixels read back (see `frame_wanted`) to a screenshot and the copies.
    pub fn after_frame(&mut self, output: &Output, pixels: Option<Pixels>, damaged: bool) {
        let Some(pixels) = pixels else { return };
        // Screenshots are of the first display (their rect is relative to it, see shell/screenshot.rs).
        if self.space.outputs().next() == Some(output)
            && let Some(rect) = self.screenshots.pending.take()
        {
            let scale = output.current_scale().fractional_scale();
            let region = rect.to_f64().to_physical(scale).to_i32_round();
            let (rgba, size) = pixels.shown().crop(region);
            self.screenshot_taken(rgba, (size.w as u32, size.h as u32), scale);
        }
        let verdicts = self.capture.verdicts.clone();
        let now = Duration::from(self.protocols.clock.now());
        self.capture.deliver(output, &pixels, damaged, now, |c| verdicts.get(c) == Some(&true));
    }

    /// Whether `output`'s frame just drawn (`damaged`: it changed) must be read back.
    pub fn frame_wanted(&self, output: &Output, damaged: bool) -> bool {
        (self.screenshots.pending.is_some() && self.space.outputs().next() == Some(output))
            || self.capture.wants(output, damaged, |c| self.capture.verdicts.get(c) == Some(&true))
    }
}

/// The region a frame object captures in the output's buffer px (`None`: the whole output). `region` is logical,
/// as seen; the buffer is before the output's transform.
fn frame_region(output: &Output, region: Option<Rectangle<i32, smithay::utils::Logical>>) -> Rectangle<i32, Physical> {
    let mode = output.current_mode().map_or((0, 0).into(), |m| m.size);
    let Some(region) = region else { return Rectangle::from_size(mode) };
    buffer_region(region.to_f64().to_physical(output.current_scale().fractional_scale()).to_i32_round(), mode, output.current_transform())
}

/// A region as seen (physical px) in the buffer of `mode` size under `transform`.
fn buffer_region(seen: Rectangle<i32, Physical>, mode: Size<i32, Physical>, transform: Transform) -> Rectangle<i32, Physical> {
    let shown = transform.transform_size(mode);
    let seen = seen.intersection(Rectangle::from_size(shown)).unwrap_or_default();
    transform.invert().transform_rect_in(seen, &shown)
}

impl GlobalDispatch<ZwlrScreencopyManagerV1, ()> for State {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<ZwlrScreencopyManagerV1>, _: &(), init: &mut DataInit<'_, Self>) {
        init.init(resource, ());
    }

    /// Sandboxed apps never see it (E6.11).
    fn can_view(client: Client, _: &()) -> bool {
        crate::protocols::unsandboxed(&client)
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        let (frame, output, region) = match request {
            zwlr_screencopy_manager_v1::Request::CaptureOutput { frame, output, .. } => (frame, output, None),
            zwlr_screencopy_manager_v1::Request::CaptureOutputRegion { frame, output, x, y, width, height, .. } => {
                (frame, output, Some(Rectangle::new((x, y).into(), (width, height).into())))
            }
            _ => return,
        };
        let output = Output::from_resource(&output);
        let region = output.as_ref().map(|o| frame_region(o, region)).unwrap_or_default();
        let frame = init.init(frame, FrameData { output, region, used: AtomicBool::new(false) });
        if region.is_empty() {
            frame.failed();
            return;
        }
        frame.buffer(wl_shm::Format::Xrgb8888, region.size.w as u32, region.size.h as u32, region.size.w as u32 * 4);
        if frame.version() >= 3 {
            frame.buffer_done();
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, FrameData> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        frame: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &FrameData,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        let (buffer, damage) = match request {
            zwlr_screencopy_frame_v1::Request::Copy { buffer } => (buffer, false),
            zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer } => (buffer, true),
            _ => return,
        };
        if data.used.swap(true, Ordering::Relaxed) {
            frame.post_error(zwlr_screencopy_frame_v1::Error::AlreadyUsed, "frame already copied");
            return;
        }
        let fits = smithay::wayland::shm::with_buffer_contents(&buffer, |_, _, b| {
            b.format == wl_shm::Format::Xrgb8888 && b.width == data.region.size.w && b.height == data.region.size.h && b.stride >= b.width * 4
        });
        if !matches!(fits, Ok(true)) {
            frame.post_error(zwlr_screencopy_frame_v1::Error::InvalidBuffer, "need an xrgb8888 shm buffer of the advertised size");
            return;
        }
        let verdict = state.may_record(client);
        if let Some(answer) = verdict {
            state.capture.verdicts.insert(client.id(), answer);
        }
        if verdict == Some(false) {
            frame.failed();
            return;
        }
        let Some(output) = data.output.clone() else { return frame.failed() };
        state.capture.waiting.push(Waiting { frame: frame.clone(), output, buffer, region: data.region, damage });
        state.capture_ask();
        state.schedule_redraw();
    }

    fn destroyed(state: &mut Self, _: ClientId, frame: &ZwlrScreencopyFrameV1, _: &FrameData) {
        state.capture.waiting.retain(|w| w.frame != *frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3×2 buffer, one byte value per pixel: 1 2 3 / 4 5 6.
    fn buffer(transform: Transform) -> Pixels {
        Pixels { size: (3, 2).into(), rgba: (1..=6).flat_map(|v| [v; 4]).collect(), transform }
    }

    fn values(p: &Pixels) -> Vec<u8> {
        p.rgba.chunks(4).map(|c| c[0]).collect()
    }

    #[test]
    fn crop_clips() {
        let (one, size) = buffer(Transform::Normal).crop(Rectangle::new((2, 1).into(), (5, 5).into()));
        assert_eq!((one, size), (vec![6; 4], (1, 1).into()));
    }

    #[test]
    fn shown_applies_the_transform() {
        let shown = |t| {
            let p = buffer(t).shown();
            (values(&p), (p.size.w, p.size.h))
        };
        assert_eq!(shown(Transform::Normal), (vec![1, 2, 3, 4, 5, 6], (3, 2)));
        // Nested: the GL rows run bottom-up.
        assert_eq!(shown(Transform::Flipped180), (vec![4, 5, 6, 1, 2, 3], (3, 2)));
        assert_eq!(shown(Transform::_180), (vec![6, 5, 4, 3, 2, 1], (3, 2)));
        assert_eq!(shown(Transform::Flipped), (vec![3, 2, 1, 6, 5, 4], (3, 2)));
        // A turned display: 2 wide, 3 high.
        assert_eq!(shown(Transform::_90), (vec![4, 1, 5, 2, 6, 3], (2, 3)));
        assert_eq!(shown(Transform::_270), (vec![3, 6, 2, 5, 1, 4], (2, 3)));
        assert_eq!(shown(Transform::Flipped90), (vec![1, 4, 2, 5, 3, 6], (2, 3)));
        assert_eq!(shown(Transform::Flipped270), (vec![6, 3, 5, 2, 4, 1], (2, 3)));
    }

    #[test]
    fn region_as_seen_is_the_same_buffer_px() {
        let all = [
            Transform::Normal,
            Transform::_90,
            Transform::_180,
            Transform::_270,
            Transform::Flipped,
            Transform::Flipped90,
            Transform::Flipped180,
            Transform::Flipped270,
        ];
        for t in all {
            let raw = buffer(t);
            let seen = raw.shown();
            for x in 0..seen.size.w {
                for y in 0..seen.size.h {
                    let px = Rectangle::new((x, y).into(), (1, 1).into());
                    let at = buffer_region(px, raw.size, t);
                    assert_eq!(raw.crop(at).0, seen.crop(px).0, "{t:?} at {x},{y}");
                }
            }
            assert_eq!(buffer_region(Rectangle::from_size((9, 9).into()), raw.size, t), Rectangle::from_size(raw.size), "{t:?}");
        }
    }
}
