//! Screenshots (E6.8): Super+Shift+3 takes the whole screen, Super+Shift+4 a dragged region (Space switches to
//! picking a window), Super+Shift+5 opens the toolbar (Screen · Window · Region · Record · Options · Capture). The
//! overlay goes away, the next frame is read back (capture.rs), and a worker thread saves it as
//! `~/Pictures/Screenshots/Screenshot <date> at <time>.png`; then it is on the clipboard and a thumbnail floats at the
//! bottom right for 5 s (click opens it, drag drops it into an app). Record pipes the display's frames through ffmpeg
//! into `~/Videos/Screen Recording <date> at <time>.mp4`. Drawn by render/screenshot.rs.

use std::{
    ffi::OsString,
    io::Write,
    os::fd::OwnedFd,
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use smithay::{
    input::{
        keyboard::Keysym,
        pointer::{CursorImageStatus, GrabStartData, MotionEvent},
    },
    reexports::{
        calloop::{
            RegistrationToken, channel,
            timer::{TimeoutAction, Timer},
        },
        wayland_server::protocol::wl_data_device_manager::DndAction,
    },
    utils::{Logical, Physical, Point, Rectangle, SERIAL_COUNTER, Size, Transform},
    wayland::selection::data_device::{SourceMetadata, start_dnd},
};

use crate::{
    capture::Pixels,
    input::BTN_LEFT,
    shell::{Anchor, Row, menu_layout},
    state::State,
    text::ts,
    wm::Rect,
};

/// What a screenshot key starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    Screen,
    /// The focused window, at once.
    Window,
    Region,
    Toolbar,
}

/// What a capture takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Screen,
    Window,
    Region,
}

/// The overlay while choosing what to capture (Super+Shift+4, or the toolbar).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Shot {
    pub mode: Mode,
    /// Super+Shift+5's toolbar is up; without it a region is taken as soon as the drag ends.
    pub toolbar: bool,
    /// The drag in progress: where it started and where the pointer is, output-relative.
    pub drag: Option<(Point<f64, Logical>, Point<f64, Logical>)>,
    /// The toolbar's chosen region, kept until Capture.
    pub region: Option<Rect>,
    /// Window mode: the window frame under the pointer.
    pub window: Option<Rect>,
    /// The Options menu is open.
    pub options: bool,
    /// The toolbar part under the pointer.
    pub hover: Option<Hit>,
}

/// A saved screenshot: its file, the PNG (for the clipboard) and a small RGBA copy for the thumbnail.
pub struct Saved {
    pub path: PathBuf,
    pub png: Vec<u8>,
    pub thumb: Thumb,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Thumb {
    pub size: (u32, u32),
    pub rgba: Arc<[u8]>,
}

enum Msg {
    Saved(Saved),
    /// The recording that started then is finished: its file, or why it failed.
    RecordingEnded(Instant, Result<PathBuf, String>),
}

/// A screen recording: `output`'s frames read back (capture.rs) go to the recording thread (`encode`), which pipes
/// them through ffmpeg into an mp4.
pub struct Recorder {
    pub output: smithay::output::Output,
    pub since: Instant,
    /// The thread's queue; a frame that finds it full (ffmpeg lags) is dropped.
    frames: mpsc::SyncSender<Pixels>,
    pace: Pace,
    /// A redraw is set for when the waiting change is due.
    retry: bool,
}

#[derive(Default)]
pub struct Screenshots {
    pub shot: Option<Shot>,
    /// The display the overlay is on and the next capture is of: the pointer's, or the focused window's for Window.
    pub output: Option<smithay::output::Output>,
    /// Read from that display's next frame, relative to it, logical px.
    pub pending: Option<(smithay::output::Output, Rect)>,
    /// The display the last shot was of, where its thumbnail shows.
    pub thumb_output: Option<smithay::output::Output>,
    /// The floating thumbnail and the file it opens, until its 5 s are up.
    pub thumbnail: Option<(Thumb, PathBuf)>,
    thumb_timer: Option<RegistrationToken>,
    /// Where the left button went down on the thumbnail: a click opens it, a drag starts dragging it.
    thumb_press: Option<Point<f64, Logical>>,
    /// The thumbnail is being dragged into an app (it follows the pointer; its timer waits).
    pub dragging: bool,
    /// The last dragged file: drop targets read it after the drop.
    drag_file: Option<PathBuf>,
    /// Options ▸ timer: seconds before a toolbar capture.
    pub delay: u8,
    /// Options ▸ Show Floating Thumbnail, unticked.
    pub no_thumbnail: bool,
    pub recording: Option<Recorder>,
    last_mode: Mode,
    tx: Option<channel::Sender<Msg>>,
}

/// The toolbar (a `hud` strip above the dock), output-relative.
#[derive(Debug, Clone, PartialEq)]
pub struct Toolbar {
    pub rect: Rect,
    pub close: Rect,
    /// Screen, Window, Region.
    pub modes: [Rect; 3],
    pub record: Rect,
    pub options: Rect,
    pub capture: Rect,
    /// The 1 px dividers between the groups.
    pub dividers: [Rect; 3],
}

/// What a toolbar click hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Close,
    Mode(Mode),
    Record,
    Options,
    Capture,
    /// A row of the Options menu.
    Option(usize),
}

pub const BAR_H: i32 = 52;
pub const RADIUS: f64 = 12.0;
const PAD: i32 = 8;
const CLOSE: i32 = 28;
const MODE_W: i32 = 44;
const MODE_H: i32 = 36;
const GAP: i32 = 4;
const DIVIDER_GAP: i32 = 8;
/// `OPTIONS_W`, `CAPTURE_W`, `BUTTON_H` and the Options menu's width grow with the text size (`text::ts`).
const OPTIONS_W: i32 = 84;
const CAPTURE_W: i32 = 80;
const BUTTON_H: i32 = 28;
/// Above the dock (84 px icons and their padding).
const BOTTOM: i32 = 128;
/// The floating thumbnail's width and its margin from the screen corner.
pub const THUMB_W: i32 = 200;
pub const THUMB_MARGIN: i32 = 16;
const THUMB_TIME: Duration = Duration::from_secs(5);
/// The Options menu (D8 rows).
pub const OPTION_ROWS: [Option<&str>; 5] = [Some("No Timer"), Some("5 Seconds"), Some("10 Seconds"), None, Some("Show Floating Thumbnail")];
const OPTIONS_MENU_W: i32 = 210;

pub fn toolbar(output: Size<i32, Logical>) -> Toolbar {
    let divider = DIVIDER_GAP * 2 + 1;
    let (options_w, capture_w, button_h) = (ts(OPTIONS_W), ts(CAPTURE_W), ts(BUTTON_H));
    let w = PAD + CLOSE + divider + 3 * MODE_W + 2 * GAP + divider + MODE_W + divider + options_w + GAP + capture_w + PAD;
    let rect = Rectangle::new(((output.w - w) / 2, (output.h - BOTTOM - BAR_H).max(0)).into(), (w, BAR_H).into());
    let y = rect.loc.y;
    let mut x = rect.loc.x + PAD;
    // Left to right: each part takes its width; a gap is a part of height 0.
    let mut take = |width: i32, height: i32| {
        let r = Rectangle::new((x, y + (BAR_H - height) / 2).into(), (width, height).into());
        x += width;
        r
    };
    let line = |take: &mut dyn FnMut(i32, i32) -> Rect| {
        take(DIVIDER_GAP, 0);
        let r = take(1, 28);
        take(DIVIDER_GAP, 0);
        r
    };
    let close = take(CLOSE, CLOSE);
    let first = line(&mut take);
    let screen = take(MODE_W, MODE_H);
    take(GAP, 0);
    let window = take(MODE_W, MODE_H);
    take(GAP, 0);
    let region = take(MODE_W, MODE_H);
    let second = line(&mut take);
    let record = take(MODE_W, MODE_H);
    let third = line(&mut take);
    let options = take(options_w, button_h);
    take(GAP, 0);
    let capture = take(capture_w, button_h);
    Toolbar { rect, close, modes: [screen, window, region], record, options, capture, dividers: [first, second, third] }
}

/// The Options menu over its button: its rect and one rect per row.
pub fn options_menu(bar: &Toolbar, output: Size<i32, Logical>) -> (Rect, Vec<Rect>) {
    let rows: Vec<Option<Row>> = OPTION_ROWS.iter().map(|r| r.map(|label| Row { sub: Vec::new(), label: label.into(), shortcut: "", action: None })).collect();
    let at = (bar.options.loc.x + bar.options.size.w / 2, bar.rect.loc.y).into();
    menu_layout(Anchor::Above(at), output, ts(OPTIONS_MENU_W), &rows, 0)
}

/// The rectangle between two drag points, at least 1 px.
pub fn region(a: Point<f64, Logical>, b: Point<f64, Logical>) -> Rect {
    let (x0, y0) = (a.x.min(b.x).round() as i32, a.y.min(b.y).round() as i32);
    let (x1, y1) = (a.x.max(b.x).round() as i32, a.y.max(b.y).round() as i32);
    Rectangle::new((x0, y0).into(), ((x1 - x0).max(1), (y1 - y0).max(1)).into())
}

/// `XDG_<name>_DIR` from the environment, else from `user-dirs.dirs` (`"$HOME/Pictures"`), else `~/<fallback>`.
pub fn user_dir(name: &str, fallback: &str) -> PathBuf {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let var = format!("XDG_{name}_DIR");
    if let Some(dir) = std::env::var_os(&var).map(PathBuf::from).filter(|p| p.is_absolute()) {
        return dir;
    }
    let dirs = std::fs::read_to_string(crate::config::xdg_dir("XDG_CONFIG_HOME", ".config").join("user-dirs.dirs")).unwrap_or_default();
    parse_user_dir(&dirs, &var, &home).unwrap_or_else(|| home.join(fallback))
}

/// One `XDG_PICTURES_DIR="$HOME/Pictures"` line of `user-dirs.dirs`.
fn parse_user_dir(text: &str, var: &str, home: &Path) -> Option<PathBuf> {
    let line = text.lines().map(str::trim).find(|l| l.strip_prefix(var).is_some_and(|r| r.trim_start().starts_with('=')))?;
    let value = line.split_once('=')?.1.trim().trim_matches('"');
    let path = match value.strip_prefix("$HOME") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(value),
    };
    path.is_absolute().then_some(path)
}

/// `<dir>/<prefix> 2026-10-18 at 13.28.05.<ext>`, with ` (2)` and up when that name is taken.
pub fn file_name(dir: &Path, prefix: &str, ext: &str, now: &chrono::DateTime<chrono::Local>) -> PathBuf {
    let stem = format!("{prefix} {}", now.format("%Y-%m-%d at %H.%M.%S"));
    let mut path = dir.join(format!("{stem}.{ext}"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem} ({n}).{ext}"));
        n += 1;
    }
    path
}

/// A `w`×`h` RGBA image shrunk by a whole `step` so it is at most `max_w` wide, each pixel the average of its
/// `step`×`step` block (cut short at the right and bottom edges).
pub fn shrink(rgba: &[u8], w: u32, h: u32, max_w: u32) -> Thumb {
    let step = w.div_ceil(max_w.max(1)).max(1);
    let (tw, th) = (w.div_ceil(step), h.div_ceil(step));
    let mut out = Vec::with_capacity((tw * th * 4) as usize);
    for ty in 0..th {
        let ys = ty * step..((ty + 1) * step).min(h);
        for tx in 0..tw {
            let xs = tx * step..((tx + 1) * step).min(w);
            let mut sum = [0u32; 4];
            for y in ys.clone() {
                let row = &rgba[((y * w + xs.start) * 4) as usize..((y * w + xs.end) * 4) as usize];
                for px in row.as_chunks::<4>().0 {
                    sum.iter_mut().zip(px).for_each(|(s, &v)| *s += u32::from(v));
                }
            }
            let n = ys.len() as u32 * xs.len() as u32;
            out.extend(sum.map(|s| ((s + n / 2) / n) as u8));
        }
    }
    Thumb { size: (tw, th), rgba: out.into() }
}

fn encode_png(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>, png::EncodingError> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_compression(png::Compression::Fast);
    encoder.write_header()?.write_image_data(rgba)?;
    Ok(bytes)
}

/// A recording's length as the menu bar shows it: `0:42`, `1:02:03`.
pub fn elapsed(d: Duration) -> String {
    let s = d.as_secs();
    if s < 3600 { format!("{}:{:02}", s / 60, s % 60) } else { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) }
}

/// What a dragged screenshot offers apps: its URI (browsers, chats, file managers) and its picture.
pub const DRAG_MIMES: [&str; 2] = ["text/uri-list", "image/png"];

/// The `file://` URI of `path`, percent-encoded (spaces as `%20`).
pub fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

/// What a drop target asked for as `mime`: the URI list, or the file's bytes; `None` for a type we don't offer.
pub fn drag_bytes(mime: &str, path: &Path) -> Option<Vec<u8>> {
    match mime {
        "text/uri-list" => Some(format!("{}\r\n", file_uri(path)).into_bytes()),
        "image/png" => std::fs::read(path).ok(),
        _ => None,
    }
}

/// The display (index into `screens`, global) a click at global `at` is on when that is not the overlay's (`shown`):
/// a Screen-mode click there captures it, and the toolbar takes none of it.
pub fn clicked_elsewhere(screens: &[Rect], shown: usize, at: Point<f64, Logical>) -> Option<usize> {
    screens.iter().position(|s| s.to_f64().contains(at)).filter(|i| *i != shown)
}

/// Frames a second a recording reads at most.
const FPS: u32 = 30;

/// `rect` (physical px as seen) inside a `frame`-sized picture, its width and height rounded down to even (x264's
/// yuv420p needs that); `None` when less than 2×2 is left.
fn even_crop(rect: Rectangle<i32, Physical>, frame: Size<i32, Physical>) -> Option<Rectangle<i32, Physical>> {
    let r = rect.intersection(Rectangle::from_size(frame))?;
    let size = Size::from((r.size.w & !1, r.size.h & !1));
    (size.w >= 2 && size.h >= 2).then(|| Rectangle::new(r.loc, size))
}

/// ffmpeg's arguments: raw RGBA frames of `size` on stdin, stamped as they arrive and repeated to a steady `FPS` (a
/// still screen sends none), into an H.264 mp4 at `path`.
fn ffmpeg_args(size: Size<i32, Physical>, path: &Path) -> Vec<OsString> {
    let (size, fps) = (format!("{}x{}", size.w, size.h), FPS.to_string());
    #[rustfmt::skip]
    let args = [
        "-hide_banner", "-loglevel", "error",
        "-f", "rawvideo", "-pixel_format", "rgba", "-video_size", &size, "-framerate", &fps, "-use_wallclock_as_timestamps", "1", "-i", "-",
        "-fps_mode", "cfr", "-r", &fps, "-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-pix_fmt", "yuv420p", "-n",
    ];
    args.into_iter().map(OsString::from).chain([path.into()]).collect()
}

/// When a recording reads a frame: one that changed, at most `FPS` a second; a change that comes sooner is read with
/// the first frame after.
#[derive(Debug, Default)]
struct Pace {
    /// When the next frame may be read; `None`: now.
    next: Option<Instant>,
    /// A change waits for `next`.
    dirty: bool,
}

impl Pace {
    fn due(&self, changed: bool, now: Instant) -> bool {
        (changed || self.dirty) && self.next.is_none_or(|n| now >= n)
    }

    /// The frame drawn at `now` (`read`: its pixels are here): `true` to record it.
    fn frame(&mut self, changed: bool, read: bool, now: Instant) -> bool {
        let due = self.due(changed, now);
        if due {
            self.next = Some(now + Duration::from_secs(1) / FPS);
        }
        self.dirty = (self.dirty || changed) && !(due && read);
        due && read
    }
}

/// The recording thread: each frame's `region` into ffmpeg's stdin. Stop closes the queue: the last frame goes once
/// more (so a still end keeps its length) and ffmpeg finishes the file.
fn encode(mut ffmpeg: Child, frames: mpsc::Receiver<Pixels>, region: Rectangle<i32, Physical>) -> Result<(), String> {
    let mut stdin = ffmpeg.stdin.take().ok_or("ffmpeg has no stdin")?;
    let mut last = Vec::new();
    let written = loop {
        let pixels = match frames.recv_timeout(Duration::from_secs(1)) {
            Ok(pixels) => pixels,
            Err(mpsc::RecvTimeoutError::Timeout) if matches!(ffmpeg.try_wait(), Ok(None)) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => break Err("ffmpeg quit".to_owned()),
            Err(mpsc::RecvTimeoutError::Disconnected) => break stdin.write_all(&last).map_err(|e| format!("ffmpeg: {e}")),
        };
        let pixels = if pixels.transform == Transform::Normal { pixels } else { pixels.shown() };
        let (rgba, size) = pixels.crop(region);
        if size != region.size {
            break Err("the display changed size".to_owned());
        }
        if let Err(err) = stdin.write_all(&rgba) {
            break Err(format!("ffmpeg: {err}"));
        }
        last = rgba;
    };
    drop(stdin);
    let status = ffmpeg.wait().map_err(|e| format!("ffmpeg: {e}"))?;
    written?;
    status.success().then_some(()).ok_or_else(|| format!("ffmpeg: {status}"))
}

/// Whether `program` is on `$PATH`.
fn installed(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

impl State {
    pub fn screenshot(&mut self, start: Start) {
        match start {
            Start::Screen => {
                self.screenshots.output = self.pointer_output();
                self.shoot(Mode::Screen, None);
            }
            Start::Window => {
                let Some(frame) = self.focused_window().and_then(|w| self.frame(&w)) else { return };
                self.screenshots.output = self.display_with(Some(frame.loc + Point::from((frame.size.w / 2, frame.size.h / 2))));
                self.shoot(Mode::Window, Some(Rectangle::new(frame.loc - self.shot_origin(), frame.size)));
            }
            Start::Region => self.open_shot(Shot { mode: Mode::Region, ..Default::default() }),
            // Its key again closes it.
            Start::Toolbar if self.screenshots.shot.as_ref().is_some_and(|s| s.toolbar) => self.close_shot(),
            Start::Toolbar => {
                let mode = self.screenshots.last_mode;
                self.open_shot(Shot { mode, toolbar: true, ..Default::default() });
            }
        }
    }

    /// Opens the overlay on the pointer's display.
    fn open_shot(&mut self, shot: Shot) {
        self.popup = None;
        self.screenshots.output = self.pointer_output();
        self.screenshots.shot = Some(shot);
        self.shot_cursor(false);
        self.schedule_redraw();
    }

    pub fn close_shot(&mut self) {
        if self.screenshots.shot.take().is_some() {
            self.set_cursor(CursorImageStatus::default_named());
            self.schedule_redraw();
        }
    }

    /// The crosshair while picking a region; the arrow over the toolbar and in the other modes.
    fn shot_cursor(&mut self, on_bar: bool) {
        let Some(shot) = &self.screenshots.shot else { return };
        let icon = match shot.mode {
            Mode::Region if !on_bar => smithay::input::pointer::CursorIcon::Crosshair,
            _ => smithay::input::pointer::CursorIcon::Default,
        };
        if self.protocols.cursor != CursorImageStatus::Named(icon) {
            self.set_cursor(CursorImageStatus::Named(icon));
        }
    }

    /// The captured display's top-left corner, in global coordinates.
    fn shot_origin(&self) -> Point<i32, Logical> {
        self.overlay_geometry(self.screenshots.output.as_ref()).map_or((0, 0).into(), |g| g.loc)
    }

    /// The captured display's size, which the toolbar and the selection are laid out in.
    pub fn shot_size(&self) -> Size<i32, Logical> {
        self.overlay_geometry(self.screenshots.output.as_ref()).map_or_else(Default::default, |g| g.size)
    }

    /// The window frame (title bar included) at `at`, output-relative.
    fn shot_window(&self, at: Point<f64, Logical>) -> Option<Rect> {
        let origin = self.shot_origin();
        let global = at + origin.to_f64();
        let frame = self.space.elements().rev().filter(|w| self.on_current(w)).find_map(|w| self.frame(w).filter(|f| f.to_f64().contains(global)))?;
        Some(Rectangle::new(frame.loc - origin, frame.size))
    }

    /// The toolbar part (or Options row) at `at`.
    fn shot_hit(&self, at: Point<f64, Logical>) -> Option<Hit> {
        let shot = self.screenshots.shot.as_ref().filter(|s| s.toolbar)?;
        let size = self.shot_size();
        let bar = toolbar(size);
        if shot.options {
            let (menu, rows) = options_menu(&bar, size);
            if menu.to_f64().contains(at) {
                return rows.iter().enumerate().find(|(i, r)| OPTION_ROWS[*i].is_some() && r.to_f64().contains(at)).map(|(i, _)| Hit::Option(i));
            }
        }
        let parts = [
            (bar.close, Hit::Close),
            (bar.modes[0], Hit::Mode(Mode::Screen)),
            (bar.modes[1], Hit::Mode(Mode::Window)),
            (bar.modes[2], Hit::Mode(Mode::Region)),
            (bar.record, Hit::Record),
            (bar.options, Hit::Options),
            (bar.capture, Hit::Capture),
        ];
        parts.iter().find(|(r, _)| r.to_f64().contains(at)).map(|(_, h)| *h)
    }

    /// Over the toolbar strip or its open menu, where the arrow shows and clicks don't pick.
    fn on_toolbar(&self, at: Point<f64, Logical>) -> bool {
        let Some(shot) = self.screenshots.shot.as_ref().filter(|s| s.toolbar) else { return false };
        let size = self.shot_size();
        let bar = toolbar(size);
        bar.rect.to_f64().contains(at) || (shot.options && options_menu(&bar, size).0.to_f64().contains(at))
    }

    /// Pointer motion while the overlay is up (first-output-relative): `true` if it took it, so no app sees the
    /// pointer.
    pub fn shot_motion(&mut self, to: Point<f64, Logical>) -> bool {
        if self.screenshots.shot.is_none() {
            self.thumbnail_motion(to);
            return false;
        }
        let to = self.to_display(self.screenshots.output.as_ref(), to);
        let hover = self.shot_hit(to);
        let on_bar = self.on_toolbar(to);
        let window = (!on_bar).then(|| self.shot_window(to)).flatten();
        let Some(shot) = self.screenshots.shot.as_mut() else { return false };
        let before = shot.clone();
        shot.hover = hover;
        if let Some((_, end)) = shot.drag.as_mut() {
            *end = to;
        }
        shot.window = if shot.mode == Mode::Window { window } else { None };
        let changed = *shot != before;
        self.shot_cursor(on_bar);
        if changed {
            self.schedule_redraw();
        }
        true
    }

    /// A press while the overlay is up: it takes every click.
    pub fn shot_press(&mut self, at: Point<f64, Logical>, left: bool) -> bool {
        let Some(shot) = self.screenshots.shot.clone() else { return self.thumbnail_press(at, left) };
        let outputs: Vec<_> = self.space.outputs().cloned().collect();
        let screens: Vec<Rect> = outputs.iter().map(|o| self.space.output_geometry(o).unwrap_or_default()).collect();
        let shown = self.overlay_output(self.screenshots.output.as_ref()).and_then(|o| outputs.iter().position(|x| *x == o));
        let elsewhere = clicked_elsewhere(&screens, shown.unwrap_or(0), at + self.first_origin().to_f64()).map(|i| outputs[i].clone());
        let at = self.to_display(self.screenshots.output.as_ref(), at);
        if !left {
            // A right click cancels.
            self.close_shot();
            return true;
        }
        if let Some(hit) = self.shot_hit(at).filter(|_| elsewhere.is_none()) {
            self.shot_button(hit);
            return true;
        }
        if elsewhere.is_none() && self.on_toolbar(at) {
            return true;
        }
        if shot.options {
            if let Some(s) = self.screenshots.shot.as_mut() {
                s.options = false;
            }
            self.schedule_redraw();
            return true;
        }
        match shot.mode {
            Mode::Region => {
                if let Some(s) = self.screenshots.shot.as_mut() {
                    s.drag = Some((at, at));
                    s.region = None;
                }
                self.schedule_redraw();
            }
            Mode::Window => {
                if let Some(frame) = self.shot_window(at) {
                    self.shoot(Mode::Window, Some(frame));
                }
            }
            Mode::Screen => {
                if elsewhere.is_some() {
                    self.screenshots.output = elsewhere;
                }
                self.shoot(Mode::Screen, None);
            }
        }
        true
    }

    /// The left button came up: a region drag ends; without the toolbar it is taken at once.
    pub fn shot_release(&mut self) {
        let Some(shot) = self.screenshots.shot.as_mut() else { return self.thumbnail_release() };
        let Some((a, b)) = shot.drag.take() else { return };
        let rect = region(a, b);
        // A click without a drag is no region.
        if rect.size.w < 4 || rect.size.h < 4 {
            self.schedule_redraw();
            return;
        }
        if shot.toolbar {
            shot.region = Some(rect);
            self.schedule_redraw();
        } else {
            self.shoot(Mode::Region, Some(rect));
        }
    }

    fn shot_button(&mut self, hit: Hit) {
        let Some(shot) = self.screenshots.shot.as_mut() else { return };
        match hit {
            Hit::Close => return self.close_shot(),
            Hit::Mode(mode) => {
                shot.mode = mode;
                shot.window = None;
                shot.options = false;
                self.screenshots.last_mode = mode;
            }
            Hit::Options => shot.options = !shot.options,
            Hit::Option(i) => {
                match i {
                    0 => self.screenshots.delay = 0,
                    1 => self.screenshots.delay = 5,
                    2 => self.screenshots.delay = 10,
                    _ => self.screenshots.no_thumbnail = !self.screenshots.no_thumbnail,
                }
                if let Some(s) = self.screenshots.shot.as_mut() {
                    s.options = false;
                }
            }
            Hit::Capture => return self.shot_capture(),
            Hit::Record => return self.toggle_recording(),
        }
        self.shot_cursor(true);
        self.schedule_redraw();
    }

    /// Capture (or Return) in the toolbar: the chosen mode; a region needs one dragged first.
    fn shot_capture(&mut self) {
        let Some(shot) = self.screenshots.shot.clone() else { return };
        match shot.mode {
            Mode::Screen => self.shoot(Mode::Screen, None),
            Mode::Region => {
                if let Some(rect) = shot.region {
                    self.shoot(Mode::Region, Some(rect));
                }
            }
            // A window is picked by clicking it.
            Mode::Window => {
                if let Some(rect) = shot.window {
                    self.shoot(Mode::Window, Some(rect));
                }
            }
        }
    }

    /// Keys while the overlay is up: Esc cancels, Return captures, Space toggles window and region picking.
    pub fn shot_key(&mut self, key: Keysym) -> bool {
        let Some(shot) = self.screenshots.shot.as_mut() else { return false };
        match key {
            Keysym::Escape => self.close_shot(),
            Keysym::Return | Keysym::KP_Enter => self.shot_capture(),
            Keysym::space if shot.drag.is_none() => {
                shot.mode = if shot.mode == Mode::Window { Mode::Region } else { Mode::Window };
                shot.window = None;
                self.shot_cursor(false);
                self.schedule_redraw();
            }
            _ => {}
        }
        // Every key: nothing typed while picking reaches an app.
        true
    }

    /// Takes `rect` (the whole screen for `None`) from the next frame, after the toolbar's timer if one is set.
    fn shoot(&mut self, mode: Mode, rect: Option<Rect>) {
        let toolbar = self.screenshots.shot.as_ref().is_some_and(|s| s.toolbar);
        self.close_shot();
        self.screenshots.last_mode = if toolbar { mode } else { self.screenshots.last_mode };
        let Some(output) = self.overlay_output(self.screenshots.output.as_ref()) else { return };
        let size = self.shot_size();
        self.screenshots.thumb_output = Some(output.clone());
        let rect = rect.and_then(|r| r.intersection(Rectangle::from_size(size))).unwrap_or_else(|| Rectangle::from_size(size));
        // The thumbnail of the last one is not in the next.
        self.hide_thumbnail();
        let delay = if toolbar { self.screenshots.delay } else { 0 };
        if delay == 0 {
            self.screenshots.pending = Some((output, rect));
            self.schedule_redraw();
            return;
        }
        let timer = Timer::from_duration(Duration::from_secs(u64::from(delay)));
        let started = self.loop_handle.insert_source(timer, move |_, _, state| {
            state.screenshots.pending = Some((output.clone(), rect));
            state.schedule_redraw();
            TimeoutAction::Drop
        });
        if let Err(err) = started {
            tracing::warn!("screenshot timer: {err}");
        }
    }

    /// The frame was read: `rgba` is `size` physical px, top row first. Saves it off the frame thread.
    pub fn screenshot_taken(&mut self, rgba: Vec<u8>, size: (u32, u32), scale: f64) {
        let tx = self.screenshot_channel();
        let dir = user_dir("PICTURES", "Pictures").join("Screenshots");
        let thumb_w = (f64::from(THUMB_W) * scale).round() as u32;
        let spawned = std::thread::Builder::new().name("screenshot".into()).spawn(move || {
            let (w, h) = size;
            let saved = std::fs::create_dir_all(&dir).map_err(|e| e.to_string()).and_then(|()| {
                let png = encode_png(&rgba, w, h).map_err(|e| e.to_string())?;
                let path = file_name(&dir, "Screenshot", "png", &chrono::Local::now());
                std::fs::write(&path, &png).map_err(|e| format!("{}: {e}", path.display()))?;
                Ok(Saved { path, png, thumb: shrink(&rgba, w, h, thumb_w) })
            });
            match (saved, tx) {
                (Ok(saved), Some(tx)) => {
                    let _ = tx.send(Msg::Saved(saved));
                }
                (Ok(_), None) => {}
                (Err(err), _) => tracing::warn!("screenshot: {err}"),
            }
        });
        if let Err(err) = spawned {
            tracing::warn!("screenshot thread: {err}");
        }
    }

    /// The worker threads' way back to the loop, made on first use.
    fn screenshot_channel(&mut self) -> Option<channel::Sender<Msg>> {
        if self.screenshots.tx.is_none() {
            let (tx, rx) = channel::channel();
            let inserted = self.loop_handle.insert_source(rx, |event, _, state| match event {
                channel::Event::Msg(Msg::Saved(saved)) => state.screenshot_saved(saved),
                channel::Event::Msg(Msg::RecordingEnded(since, result)) => state.recording_ended(since, result),
                channel::Event::Closed => {}
            });
            match inserted {
                Ok(_) => self.screenshots.tx = Some(tx),
                Err(err) => tracing::warn!("screenshot channel: {err}"),
            }
        }
        self.screenshots.tx.clone()
    }

    fn screenshot_saved(&mut self, saved: Saved) {
        tracing::info!("screenshot saved to {}", saved.path.display());
        self.ui_sound("screen-capture");
        self.clipboard.set_bytes(&self.display_handle, &self.seat.clone(), &["image/png"], saved.png);
        if self.screenshots.no_thumbnail {
            return;
        }
        self.show_thumbnail(saved.thumb, saved.path);
    }

    pub fn show_thumbnail(&mut self, thumb: Thumb, path: PathBuf) {
        self.hide_thumbnail();
        self.screenshots.thumbnail = Some((thumb, path));
        let timer = self.loop_handle.insert_source(Timer::from_duration(THUMB_TIME), |_, _, state| {
            state.screenshots.thumb_timer = None;
            state.hide_thumbnail();
            TimeoutAction::Drop
        });
        self.screenshots.thumb_timer = timer.inspect_err(|err| tracing::warn!("thumbnail timer: {err}")).ok();
        self.schedule_redraw();
    }

    fn hide_thumbnail(&mut self) {
        if let Some(token) = self.screenshots.thumb_timer.take() {
            self.loop_handle.remove(token);
        }
        if self.screenshots.thumbnail.take().is_some() {
            self.schedule_redraw();
        }
    }

    /// The display the thumbnail shows on: the shot's, or the pointer's while it is dragged.
    pub fn thumbnail_output(&self) -> Option<smithay::output::Output> {
        if self.screenshots.dragging { self.pointer_output() } else { self.overlay_output(self.screenshots.thumb_output.as_ref()) }
    }

    /// The thumbnail's rect, relative to `thumbnail_output`.
    pub fn thumbnail_rect(&self) -> Option<Rect> {
        let ((w, h), _) = self.screenshots.thumbnail.as_ref().map(|(t, p)| (t.size, p))?;
        let size = self.overlay_geometry(self.thumbnail_output().as_ref())?.size;
        let th = (i64::from(THUMB_W) * i64::from(h) / i64::from(w.max(1))) as i32;
        Some(Rectangle::new((size.w - THUMB_MARGIN - THUMB_W, size.h - THUMB_MARGIN - th).into(), (THUMB_W, th).into()))
    }

    /// The thumbnail's rect (relative to `thumbnail_output`) and opacity; while dragged it is half size and centred
    /// on the pointer.
    pub fn thumbnail_frame(&self) -> Option<(Rect, Thumb, f32)> {
        let rect = self.thumbnail_rect()?;
        let thumb = self.screenshots.thumbnail.as_ref()?.0.clone();
        if !self.screenshots.dragging {
            return Some((rect, thumb, 1.0));
        }
        let at = self.to_display(self.thumbnail_output().as_ref(), self.to_output(self.pointer_at)).to_i32_round();
        let size = rect.size.downscale(2);
        Some((Rectangle::new(at - Point::from((size.w / 2, size.h / 2)), size), thumb, 0.6))
    }

    /// A press on the thumbnail: a click (see `thumbnail_release`) opens the screenshot, a drag drops it into an app.
    fn thumbnail_press(&mut self, at: Point<f64, Logical>, left: bool) -> bool {
        if !self.thumbnail_rect().is_some_and(|r| r.to_f64().contains(self.to_display(self.thumbnail_output().as_ref(), at))) {
            return false;
        }
        self.screenshots.thumb_press = left.then_some(at);
        true
    }

    /// The left button came up after a press on the thumbnail without dragging it: it opens.
    fn thumbnail_release(&mut self) {
        if self.screenshots.thumb_press.take().is_none() || self.screenshots.dragging {
            return;
        }
        if let Some((_, path)) = self.screenshots.thumbnail.clone() {
            self.hide_thumbnail();
            self.spawn(&["xdg-open".to_owned(), path.to_string_lossy().into_owned()]);
        }
    }

    /// The pointer moved with no overlay up: past a few px from a press on the thumbnail, the drag starts.
    fn thumbnail_motion(&mut self, to: Point<f64, Logical>) {
        if self.screenshots.dragging {
            return self.schedule_redraw();
        }
        let Some(from) = self.screenshots.thumb_press else { return };
        let Some((_, path)) = self.screenshots.thumbnail.clone() else { return };
        if (to.x - from.x).hypot(to.y - from.y) < 6.0 {
            return;
        }
        let Some(pointer) = self.seat.get_pointer() else { return };
        self.screenshots.thumb_press = None;
        self.screenshots.dragging = true;
        self.screenshots.drag_file = Some(path);
        // The timer waits while it is dragged.
        if let Some(token) = self.screenshots.thumb_timer.take() {
            self.loop_handle.remove(token);
        }
        let start = GrabStartData { focus: None, button: BTN_LEFT, location: pointer.current_location() };
        let metadata = SourceMetadata { mime_types: DRAG_MIMES.map(str::to_owned).to_vec(), dnd_action: DndAction::Copy };
        let (dh, seat) = (self.display_handle.clone(), self.seat.clone());
        start_dnd(&dh, &seat, self, SERIAL_COUNTER.next_serial(), Some(start), None, metadata);
        self.schedule_redraw();
    }

    /// A drop target asks for the dragged screenshot as `mime`: written on a worker thread.
    pub fn thumbnail_send(&mut self, mime: String, fd: OwnedFd) {
        let Some(path) = self.screenshots.drag_file.clone() else { return };
        crate::clipboard::transfer(move || {
            if let Some(bytes) = drag_bytes(&mime, &path) {
                let _ = std::fs::File::from(fd).write_all(&bytes);
            }
        });
    }

    /// The drag ended over an app that took it: the thumbnail goes.
    pub fn thumbnail_dropped(&mut self) {
        if std::mem::take(&mut self.screenshots.dragging) {
            self.hide_thumbnail();
        }
    }

    /// No app took the drag: on the dock it is dropped there (the app it lands on opens it); elsewhere the thumbnail comes
    /// back for another 5 s.
    pub fn thumbnail_cancelled(&mut self) {
        if !std::mem::take(&mut self.screenshots.dragging) {
            return;
        }
        let Some(path) = self.screenshots.drag_file.clone() else { return };
        let on_dock = self.dock_covers(self.to_dock(self.to_output(self.pointer_at)));
        if on_dock {
            *self.dock_ui.uris.lock().unwrap_or_else(|e| e.into_inner()) = Some(file_uri(&path));
            self.dock_drop();
            return self.hide_thumbnail();
        }
        if let Some((thumb, path)) = self.screenshots.thumbnail.take() {
            self.show_thumbnail(thumb, path);
        }
    }

    /// Esc while the thumbnail is dragged: the drag is cancelled (no app or dock gets it) and the thumbnail comes back.
    pub fn thumbnail_escape(&mut self) -> bool {
        let Some(pointer) = self.seat.get_pointer() else { return false };
        if !std::mem::take(&mut self.screenshots.dragging) {
            return false;
        }
        let serial = SERIAL_COUNTER.next_serial();
        let time = Duration::from(self.protocols.clock.now()).as_millis() as u32;
        // Leaving the app under it first, so ending the grab drops nothing.
        pointer.motion(self, None, &MotionEvent { location: pointer.current_location(), serial, time });
        pointer.unset_grab(self, serial, time);
        if let Some((thumb, path)) = self.screenshots.thumbnail.take() {
            self.show_thumbnail(thumb, path);
        }
        true
    }

    /// Record in the toolbar, the screen-record key or the menu bar's stop item: starts recording the toolbar's
    /// display (its dragged region in Region mode), else the pointer's, into ~/Videos, or stops it.
    pub fn toggle_recording(&mut self) {
        if self.screenshots.recording.is_some() {
            self.stop_recording();
        } else if let Err(err) = self.start_recording() {
            tracing::warn!("recording: {err}");
        }
        self.close_shot();
    }

    /// Dropping the recorder closes the thread's queue: it hands ffmpeg the last frame and lets it finish the file.
    fn stop_recording(&mut self) {
        if self.screenshots.recording.take().is_some() {
            self.schedule_redraw();
        }
    }

    fn start_recording(&mut self) -> Result<(), String> {
        let toolbar = self.screenshots.shot.as_ref().is_some_and(|s| s.toolbar);
        let output = if toolbar { self.overlay_output(self.screenshots.output.as_ref()) } else { self.pointer_output() };
        let output = output.ok_or("no display")?;
        let frame = output.current_transform().transform_size(output.current_mode().ok_or("no display mode")?.size);
        // The toolbar's dragged region in Region mode, else the whole display.
        let area = self.screenshots.shot.as_ref().filter(|s| toolbar && s.mode == Mode::Region).and_then(|s| s.region);
        let scale = output.current_scale().fractional_scale();
        let rect = area.map_or(Rectangle::from_size(frame), |r| r.to_f64().to_physical(scale).to_i32_round());
        let region = even_crop(rect, frame).ok_or("the area is too small")?;
        let dir = user_dir("VIDEOS", "Videos");
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = file_name(&dir, "Screen Recording", "mp4", &chrono::Local::now());
        let mut command = std::process::Command::new("ffmpeg");
        command.args(ffmpeg_args(region.size, &path)).stdin(Stdio::piped()).stdout(Stdio::null());
        let ffmpeg = command.spawn().map_err(|e| format!("ffmpeg: {e}"))?;
        tracing::info!("recording the screen to {}", path.display());
        let (frames, queue) = mpsc::sync_channel(2);
        let (since, tx) = (Instant::now(), self.screenshot_channel());
        let pid = ffmpeg.id() as libc::pid_t;
        let thread = std::thread::Builder::new().name("recording".into()).spawn(move || {
            let result = encode(ffmpeg, queue, region).map(|()| path);
            if let Some(tx) = tx {
                let _ = tx.send(Msg::RecordingEnded(since, result));
            }
        });
        thread.map_err(|e| {
            // SAFETY: our own child, which went unwaited with the closure: ended and reaped, not left a zombie.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
                libc::waitpid(pid, std::ptr::null_mut(), 0);
            }
            format!("recording thread: {e}")
        })?;
        // The first frame is read even if nothing changes.
        let pace = Pace { next: None, dirty: true };
        self.screenshots.recording = Some(Recorder { output, since, frames, pace, retry: false });
        // The menu bar's elapsed time ticks each second; the recorded display unplugged ends it.
        let second = Duration::from_secs(1);
        let ticks = self.loop_handle.insert_source(Timer::from_duration(second), move |_, _, state| {
            let Some(r) = state.screenshots.recording.as_ref().filter(|r| r.since == since) else { return TimeoutAction::Drop };
            if !state.space.outputs().any(|o| *o == r.output) {
                tracing::info!("the recorded display is gone: recording stopped");
                state.stop_recording();
                return TimeoutAction::Drop;
            }
            state.schedule_redraw();
            TimeoutAction::ToDuration(second)
        });
        if let Err(err) = ticks {
            tracing::warn!("recording timer: {err}");
        }
        self.schedule_redraw();
        Ok(())
    }

    /// Whether `output`'s frame just drawn (`changed`) is one to record, so it is read back (capture.rs).
    pub fn record_wants(&self, output: &smithay::output::Output, changed: bool) -> bool {
        self.screenshots.recording.as_ref().is_some_and(|r| r.output == *output && r.pace.due(changed, Instant::now()))
    }

    /// After `output`'s frame (`changed`, `pixels` read back if wanted): records it when due. A change that came too
    /// soon gets a redraw once the next frame is due.
    // ponytail: a synchronous glReadPixels per recorded frame on the frame thread (capture::render); a PBO or dmabuf
    // path is the upgrade if it shows in the frame budget.
    pub fn record_frame(&mut self, output: &smithay::output::Output, changed: bool, pixels: Option<Pixels>) {
        let now = Instant::now();
        let Some(r) = self.screenshots.recording.as_mut().filter(|r| r.output == *output) else { return };
        let recorded = r.pace.frame(changed, pixels.is_some(), now);
        match pixels.filter(|_| recorded) {
            // Full: ffmpeg lags, and the frame is dropped. Closed: the thread ended and says why (`RecordingEnded`).
            Some(pixels) => {
                let _ = r.frames.try_send(pixels);
            }
            None if r.pace.dirty && !r.retry => {
                r.retry = true;
                let wait = r.pace.next.map_or(Duration::ZERO, |n| n.saturating_duration_since(now));
                let timer = self.loop_handle.insert_source(Timer::from_duration(wait), |_, _, state| {
                    if let Some(r) = state.screenshots.recording.as_mut() {
                        r.retry = false;
                    }
                    state.schedule_redraw();
                    TimeoutAction::Drop
                });
                if let Err(err) = timer {
                    tracing::warn!("recording timer: {err}");
                }
            }
            None => {}
        }
    }

    /// The recording thread is done: the file is finished, or ffmpeg failed, which ends the recording and the OSD says.
    fn recording_ended(&mut self, since: Instant, result: Result<PathBuf, String>) {
        if self.screenshots.recording.as_ref().is_some_and(|r| r.since == since) {
            self.stop_recording();
        }
        match result {
            Ok(path) => tracing::info!("screen recording saved to {}", path.display()),
            Err(err) => {
                tracing::warn!("recording: {err}");
                self.show_osd(crate::shell::Osd { symbol: crate::symbols::Symbol::Stop, level: None, text: Some("Recording Failed".into()) });
            }
        }
    }

    /// Record is greyed out without `ffmpeg`.
    pub fn can_record(&self) -> bool {
        self.screenshots.recording.is_some() || installed("ffmpeg")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolbar_fits_and_is_centred() {
        let bar = toolbar((1280, 800).into());
        assert!((1280 - (bar.rect.loc.x * 2 + bar.rect.size.w)).abs() <= 1);
        assert_eq!(bar.rect.loc.y + BAR_H + BOTTOM, 800);
        // Left to right, inside the strip, none overlapping.
        let parts =
            [bar.close, bar.dividers[0], bar.modes[0], bar.modes[1], bar.modes[2], bar.dividers[1], bar.record, bar.dividers[2], bar.options, bar.capture];
        assert!(parts.windows(2).all(|p| p[0].loc.x + p[0].size.w <= p[1].loc.x));
        assert!(parts.iter().all(|p| bar.rect.contains_rect(*p)));
        assert_eq!(bar.capture.loc.x + bar.capture.size.w + PAD, bar.rect.loc.x + bar.rect.size.w);
        // The menu opens above the bar.
        let (menu, rows) = options_menu(&bar, (1280, 800).into());
        assert!(menu.loc.y + menu.size.h <= bar.rect.loc.y && rows.len() == OPTION_ROWS.len());
    }

    #[test]
    fn recorded_area_is_even_and_on_screen() {
        let frame = Size::from((1920, 1080));
        let crop = |x, y, w, h| even_crop(Rectangle::new((x, y).into(), (w, h).into()), frame).map(|r| (r.loc.x, r.loc.y, r.size.w, r.size.h));
        assert_eq!(crop(0, 0, 1920, 1080), Some((0, 0, 1920, 1080)));
        assert_eq!(crop(11, 7, 301, 199), Some((11, 7, 300, 198)));
        // Clipped to the display first, then made even.
        assert_eq!(crop(1800, 1000, 500, 500), Some((1800, 1000, 120, 80)));
        assert_eq!(crop(-5, -5, 10, 10), Some((0, 0, 4, 4)));
        assert_eq!(crop(100, 100, 1, 50), None);
        assert_eq!(crop(5000, 0, 10, 10), None);
    }

    #[test]
    fn ffmpeg_reads_raw_frames_and_writes_mp4() {
        let args = ffmpeg_args((1280, 720).into(), Path::new("/v/Screen Recording.mp4"));
        let args: Vec<_> = args.iter().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args.join(" "),
            "-hide_banner -loglevel error -f rawvideo -pixel_format rgba -video_size 1280x720 -framerate 30 \
             -use_wallclock_as_timestamps 1 -i - -fps_mode cfr -r 30 -c:v libx264 -preset veryfast -crf 23 -pix_fmt yuv420p \
             -n /v/Screen Recording.mp4"
        );
    }

    /// The real pipeline on a few synthetic frames: a playable mp4 comes out. Skipped without ffmpeg.
    #[test]
    fn encodes_frames_into_an_mp4() {
        if !installed("ffmpeg") {
            return;
        }
        let path = std::env::temp_dir().join(format!("noobsbot recording {}.mp4", std::process::id()));
        let region = Rectangle::new((2, 2).into(), (64, 48).into());
        let ffmpeg = std::process::Command::new("ffmpeg").args(ffmpeg_args(region.size, &path)).stdin(Stdio::piped()).stdout(Stdio::null()).spawn().unwrap();
        let (frames, queue) = mpsc::sync_channel(2);
        let thread = std::thread::spawn(move || encode(ffmpeg, queue, region));
        for (i, transform) in [Transform::Normal, Transform::Flipped180, Transform::Normal].into_iter().enumerate() {
            frames.send(Pixels { size: (68, 52).into(), rgba: vec![i as u8 * 100; 68 * 52 * 4], transform }).unwrap();
            std::thread::sleep(Duration::from_millis(100));
        }
        drop(frames);
        assert_eq!(thread.join().unwrap(), Ok(()));
        let probe = installed("ffprobe").then(|| {
            let args = ["-v", "error", "-show_entries", "stream=codec_name,width,height", "-of", "csv=p=0"];
            std::process::Command::new("ffprobe").args(args).arg(&path).output().unwrap()
        });
        let mp4 = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(&mp4[4..8] == b"ftyp" && mp4.windows(4).any(|w| w == b"moov"), "a finished mp4");
        if let Some(probe) = probe {
            assert_eq!(String::from_utf8_lossy(&probe.stdout).trim(), "h264,64,48");
        }
    }

    #[test]
    fn frames_are_paced() {
        let t = Instant::now();
        let ms = |n| t + Duration::from_millis(n);
        // The first frame is read even if nothing changed.
        let mut pace = Pace { next: None, dirty: true };
        assert!(pace.frame(false, true, t));
        // Unchanged frames are not read.
        assert!(!pace.due(false, ms(100)));
        assert!(pace.frame(true, true, ms(100)));
        // A change 10 ms later waits, and is read with the first frame from 33 ms on.
        assert!(!pace.frame(true, true, ms(110)) && pace.dirty);
        assert!(!pace.due(false, ms(120)));
        assert!(pace.due(false, ms(134)) && pace.frame(false, true, ms(134)) && !pace.dirty);
        // Due but not read back: it stays waiting, and the next try is a frame later.
        assert!(!pace.frame(true, false, ms(200)) && pace.dirty);
        assert!(!pace.due(false, ms(210)) && pace.frame(false, true, ms(234)));
    }

    #[test]
    fn a_click_on_another_display_is_for_that_display() {
        // The overlay on the right display; the left one sits above-left of it.
        let screens = [Rectangle::new((0, 0).into(), (1280, 800).into()), Rectangle::new((-1280, -200).into(), (1280, 1024).into())];
        assert_eq!(clicked_elsewhere(&screens, 0, (-10.0, 700.0).into()), Some(1));
        // Where the toolbar would be on the overlay's display, but on the other: not the toolbar's.
        let bar = toolbar(screens[0].size).rect.loc.to_f64() + screens[1].loc.to_f64();
        assert_eq!(clicked_elsewhere(&screens, 0, bar), Some(1));
        assert_eq!(clicked_elsewhere(&screens, 0, (640.0, 400.0).into()), None);
        assert_eq!(clicked_elsewhere(&screens, 1, (640.0, 400.0).into()), Some(0));
        // Off every display (between them): the overlay's.
        assert_eq!(clicked_elsewhere(&screens, 0, (-10.0, 900.0).into()), None);
    }

    #[test]
    fn toolbar_grows_with_text_size() {
        let at = |k| crate::text::with_scale(k, || (toolbar((1280, 800).into()), options_menu(&toolbar((1280, 800).into()), (1280, 800).into())));
        let (one, big) = (at(1.0), at(1.5));
        assert_eq!(one.0, toolbar((1280, 800).into()), "1.0 is the default");
        assert_eq!((one.1.0.size.w, big.1.0.size.w), (210, 315), "the Options menu");
        assert_eq!((big.0.options.size, big.0.capture.size), ((126, 42).into(), (120, 42).into()));
        let (bar, (menu, _)) = big;
        assert!([bar.options, bar.capture].iter().all(|p| bar.rect.contains_rect(*p)) && bar.options.loc.x + bar.options.size.w < bar.capture.loc.x);
        assert!(menu.loc.y + menu.size.h <= bar.rect.loc.y && (1280 - (bar.rect.loc.x * 2 + bar.rect.size.w)).abs() <= 1);
    }

    #[test]
    fn region_from_any_drag_direction() {
        let r = region((50.0, 80.0).into(), (10.4, 20.0).into());
        assert_eq!(r, Rectangle::new((10, 20).into(), (40, 60).into()));
        assert_eq!(region((5.0, 5.0).into(), (5.0, 5.0).into()).size, (1, 1).into());
    }

    #[test]
    fn user_dirs() {
        let home = Path::new("/home/u");
        let text = "# comment\nXDG_PICTURES_DIR=\"$HOME/Bilder\"\nXDG_VIDEOS_DIR=\"/data/v\"\n";
        assert_eq!(parse_user_dir(text, "XDG_PICTURES_DIR", home), Some(PathBuf::from("/home/u/Bilder")));
        assert_eq!(parse_user_dir(text, "XDG_VIDEOS_DIR", home), Some(PathBuf::from("/data/v")));
        assert_eq!(parse_user_dir(text, "XDG_MUSIC_DIR", home), None);
    }

    #[test]
    fn names_never_overwrite() {
        let dir = std::env::temp_dir().join(format!("noobsbot-shot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let now = chrono::Local::now();
        let first = file_name(&dir, "Screenshot", "png", &now);
        assert!(first.file_name().unwrap().to_string_lossy().starts_with("Screenshot ") && first.extension().unwrap() == "png");
        std::fs::write(&first, b"x").unwrap();
        assert!(file_name(&dir, "Screenshot", "png", &now).to_string_lossy().ends_with(" (2).png"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn shrink_keeps_aspect() {
        let rgba: Vec<u8> = (0..10 * 4 * 4).map(|i| i as u8).collect();
        let t = shrink(&rgba, 10, 4, 5);
        assert_eq!(t.size, (5, 2));
        // Each the average of a 2×2 block: source pixels 0, 1, 10 and 11, then 2, 3, 12 and 13.
        assert_eq!(&t.rgba[..8], &[22, 23, 24, 25, 30, 31, 32, 33]);
        assert_eq!(shrink(&rgba, 10, 4, 100).rgba[..], rgba[..]);
        // The block at the right edge is cut short.
        let row = [0, 0, 0, 0, 10, 10, 10, 10, 200, 200, 200, 200];
        assert_eq!(shrink(&row, 3, 1, 2).rgba[..], [5, 5, 5, 5, 200, 200, 200, 200]);
    }

    #[test]
    fn recording_time_reads_as_minutes_and_seconds() {
        assert_eq!(elapsed(Duration::from_secs(42)), "0:42");
        assert_eq!(elapsed(Duration::from_secs(62)), "1:02");
        assert_eq!(elapsed(Duration::from_secs(3723)), "1:02:03");
    }

    #[test]
    fn drag_offers_uri_and_png() {
        let path = Path::new("/home/u/Pictures/Screenshots/Screenshot 2026-10-27 at 9.41.png");
        assert_eq!(file_uri(path), "file:///home/u/Pictures/Screenshots/Screenshot%202026-10-27%20at%209.41.png");
        assert_eq!(crate::apps::file_path(&file_uri(path)).as_deref(), path.to_str(), "round trip");
        assert_eq!(file_uri(Path::new("/tmp/a%b#ü")), "file:///tmp/a%25b%23%C3%BC");
        assert_eq!(drag_bytes("text/uri-list", path), Some(format!("{}\r\n", file_uri(path)).into_bytes()));
        let file = std::env::temp_dir().join(format!("noobsbot drag {}.png", std::process::id()));
        std::fs::write(&file, b"png").unwrap();
        assert_eq!(drag_bytes("image/png", &file).as_deref(), Some(&b"png"[..]));
        assert_eq!(drag_bytes("text/plain", &file), None, "only what we offer");
        assert!(DRAG_MIMES.iter().all(|m| drag_bytes(m, &file).is_some()));
        std::fs::remove_file(&file).unwrap();
    }

    #[test]
    fn png_round_trip() {
        let rgba = [255u8, 0, 0, 255, 0, 255, 0, 255];
        let bytes = encode_png(&rgba, 2, 1).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().unwrap();
        let mut out = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut out).unwrap();
        assert_eq!(out, rgba);
    }
}
