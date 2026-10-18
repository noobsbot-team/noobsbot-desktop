//! Reading the screen back (E6.8): our own screenshots, and `wlr-screencopy-unstable-v1` for other programs (grim,
//! wf-recorder, xdg-desktop-portal-wlr's screen sharing) into their shm buffers. The frame just drawn is read once
//! when someone waits for it, so capturing costs nothing while nobody asks. Who may capture is perms.rs's call.
// ponytail: shm copies only (a 1080p frame is 8 MB through the CPU); dmabuf frames for zero-copy recording and
// `ext-image-copy-capture` come with the udev backend (M6). The cursor is never drawn in (nested it's the host's).

use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            ExportMem,
            gles::{GlesMapping, GlesRenderer, GlesTarget},
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
    utils::{Physical, Rectangle, Size, Transform},
    wayland::shm::with_buffer_contents_mut,
};

use crate::state::State;

/// A copy waiting for the next frame (or, `damage`, the next frame that changed).
struct Waiting {
    frame: ZwlrScreencopyFrameV1,
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

/// A frame object's region in output px; a frame copies once.
pub struct FrameData {
    region: Rectangle<i32, Physical>,
    used: AtomicBool,
}

/// One output frame read back: RGBA, top row first.
pub struct Pixels {
    pub size: Size<i32, Physical>,
    pub rgba: Vec<u8>,
}

impl Pixels {
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

/// Starts reading the whole bound frame, before the swap.
pub fn read(renderer: &mut GlesRenderer, target: &GlesTarget<'_>, size: Size<i32, Physical>) -> Option<(GlesMapping, Size<i32, Physical>)> {
    let region = Rectangle::from_size((size.w, size.h).into());
    let mapping = renderer.copy_framebuffer(target, region, Fourcc::Abgr8888).inspect_err(|err| tracing::warn!("screen read: {err}")).ok()?;
    Some((mapping, size))
}

/// Finishes a read, after the swap: mapping makes the context current without the window surface, which would fail
/// the swap. GL's rows run bottom-up (the winit surface is drawn flipped, see backend/winit.rs), so they are turned over.
// ponytail: assumes the winit Flipped180 output; the udev backend's offscreen targets read top-down (M6).
pub fn map(renderer: &mut GlesRenderer, (mapping, size): (GlesMapping, Size<i32, Physical>)) -> Option<Pixels> {
    let bytes = renderer.map_texture(&mapping).inspect_err(|err| tracing::warn!("screen map: {err}")).ok()?;
    Some(Pixels { size, rgba: flip_rows(bytes, size.w as usize * 4) })
}

fn flip_rows(bytes: &[u8], stride: usize) -> Vec<u8> {
    bytes.chunks_exact(stride.max(1)).rev().flatten().copied().collect()
}

impl Capture {
    pub fn new(dh: &DisplayHandle) -> Self {
        Self { _global: dh.create_global::<State, ZwlrScreencopyManagerV1, _>(3, ()), waiting: Vec::new(), served: HashSet::new(), verdicts: HashMap::new() }
    }

    /// Does the frame just drawn (`damaged`: it changed) have to be read?
    pub fn wants(&self, damaged: bool, allowed: impl Fn(&ClientId) -> bool) -> bool {
        self.waiting.iter().any(|w| self.ready(w, damaged) && w.frame.client().is_some_and(|c| allowed(&c.id())))
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

    /// Fills every allowed copy that this frame satisfies; `now`: the frame's presentation time (monotonic).
    /// `inverted`: the output is flipped upside down (nested), so clients that undo its transform must flip back.
    pub fn deliver(&mut self, pixels: &Pixels, damaged: bool, now: Duration, inverted: bool, allowed: impl Fn(&ClientId) -> bool) {
        let waiting = std::mem::take(&mut self.waiting);
        for w in waiting {
            let Some(client) = w.frame.client().map(|c| c.id()) else { continue };
            if !self.ready(&w, damaged) || !allowed(&client) {
                self.waiting.push(w);
                continue;
            }
            self.served.insert(client);
            if !copy_into(&w.buffer, pixels, w.region) {
                w.frame.failed();
                continue;
            }
            // ponytail: only the nested Flipped180 is undone; rotated outputs would need the pixels turned too.
            w.frame.flags(if inverted { zwlr_screencopy_frame_v1::Flags::YInvert } else { zwlr_screencopy_frame_v1::Flags::empty() });
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
    /// After drawing: reads the frame if a screenshot or an allowed copy waits for it, and hands it out.
    pub fn after_frame(&mut self, pixels: Option<Pixels>, damaged: bool, scale: f64) {
        let Some(pixels) = pixels else { return };
        if let Some(rect) = self.screenshots.pending.take() {
            let region = rect.to_f64().to_physical(scale).to_i32_round();
            let (rgba, size) = pixels.crop(region);
            self.screenshot_taken(rgba, (size.w as u32, size.h as u32), scale);
        }
        let verdicts = self.capture.verdicts.clone();
        let now = Duration::from(self.protocols.clock.now());
        let inverted = self.space.outputs().next().is_some_and(|o| o.current_transform() == Transform::Flipped180);
        self.capture.deliver(&pixels, damaged, now, inverted, |c| verdicts.get(c) == Some(&true));
    }

    /// Whether this frame must be read back (before the swap, while it is still bound).
    pub fn frame_wanted(&self, damaged: bool) -> bool {
        self.screenshots.pending.is_some() || self.capture.wants(damaged, |c| self.capture.verdicts.get(c) == Some(&true))
    }
}

/// The output a frame object captures, its region in output px (`None`: the whole output).
fn frame_region(output: &Output, region: Option<Rectangle<i32, smithay::utils::Logical>>) -> Rectangle<i32, Physical> {
    let mode = output.current_mode().map_or((0, 0).into(), |m| m.size);
    let full = Rectangle::from_size(mode);
    let Some(region) = region else { return full };
    let scale = output.current_scale().fractional_scale();
    region.to_f64().to_physical(scale).to_i32_round().intersection(full).unwrap_or_default()
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
        let region = Output::from_resource(&output).map(|o| frame_region(&o, region)).unwrap_or_default();
        let frame = init.init(frame, FrameData { region, used: AtomicBool::new(false) });
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
        state.capture.waiting.push(Waiting { frame: frame.clone(), buffer, region: data.region, damage });
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

    #[test]
    fn rows_turn_over_and_crop() {
        // 2×2: rows bottom-up as GL reads them.
        let gl = [3u8, 3, 3, 3, 4, 4, 4, 4, 1, 1, 1, 1, 2, 2, 2, 2];
        let pixels = Pixels { size: (2, 2).into(), rgba: flip_rows(&gl, 8) };
        assert_eq!(&pixels.rgba[..8], &[1, 1, 1, 1, 2, 2, 2, 2]);
        let (one, size) = pixels.crop(Rectangle::new((1, 1).into(), (5, 5).into()));
        assert_eq!((one, size), (vec![4, 4, 4, 4], (1, 1).into()));
    }
}
