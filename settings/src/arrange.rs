//! The Displays page's arrangement (E6.2, like Cinnamon's): the displays as rectangles to drag, which snap edge to
//! edge onto the others; and the "Keep these display settings?" countdown.

use std::time::{Duration, Instant};

use iced::{
    Color, Point, Rectangle, Size, mouse,
    widget::canvas::{self, Event, Frame, Geometry, Path, Stroke, Text},
};

use crate::{Message, display_info::Display, style::Tokens};

/// A display's place in logical px.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Area {
    fn of(d: &Display) -> Area {
        Area { x: d.x, y: d.y, w: d.width.max(1), h: d.height.max(1) }
    }

    fn overlaps(self, o: Area) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }
}

/// Where `moved` lands when dropped: touching one of `others` edge to edge (never apart, never overlapping when it
/// can help it), at the place nearest the drop; along the shared edge, its ends snap to the other's ends or centre
/// within `pull` px.
pub fn snap(moved: Area, others: &[Area], pull: i32) -> (i32, i32) {
    let along = |pos: i32, len: i32, o_pos: i32, o_len: i32| {
        // At least a little of the edge is shared.
        let pos = pos.clamp(o_pos - len + 1, o_pos + o_len - 1);
        [o_pos, o_pos + o_len - len, o_pos + (o_len - len) / 2].into_iter().find(|p| (p - pos).abs() <= pull).unwrap_or(pos)
    };
    let mut spots = Vec::new();
    for o in others {
        let y = along(moved.y, moved.h, o.y, o.h);
        let x = along(moved.x, moved.w, o.x, o.w);
        spots.extend([(o.x + o.w, y), (o.x - moved.w, y), (x, o.y + o.h), (x, o.y - moved.h)]);
    }
    let distance = |&(x, y): &(i32, i32)| i64::from(x - moved.x).pow(2) + i64::from(y - moved.y).pow(2);
    let free = |&(x, y): &(i32, i32)| !others.iter().any(|o| Area { x, y, ..moved }.overlaps(*o));
    spots.iter().copied().filter(free).min_by_key(distance).or_else(|| spots.iter().copied().min_by_key(distance)).unwrap_or((moved.x, moved.y))
}

/// The places moved so the top-left one starts at 0, 0 (the layout keeps its shape).
pub fn normalise(places: &mut [(String, i32, i32)]) {
    let (min_x, min_y) = places.iter().fold((i32::MAX, i32::MAX), |(mx, my), (_, x, y)| (mx.min(*x), my.min(*y)));
    for (_, x, y) in places {
        *x -= min_x;
        *y -= min_y;
    }
}

/// How long "Keep these display settings?" waits before putting the old ones back.
pub const KEEP_FOR: Duration = Duration::from_secs(15);

/// A display change waiting to be kept: the whole config file from before it, and when it was made.
#[derive(Debug, Clone)]
pub struct Keep {
    pub previous: String,
    pub since: Instant,
}

impl Keep {
    /// Whole seconds left on the countdown, rounded up (15 … 1, then 0).
    pub fn left(&self, now: Instant) -> u64 {
        let left = KEEP_FOR.saturating_sub(now.saturating_duration_since(self.since));
        left.as_millis().div_ceil(1000) as u64
    }

    pub fn expired(&self, now: Instant) -> bool {
        self.left(now) == 0
    }
}

/// A `()` every half second, for the countdown; subscribed only while it shows (the app has no timer executor, so a
/// small thread sleeps, and ends once the subscription is dropped).
pub fn ticks() -> impl iced::futures::Stream<Item = ()> {
    iced::stream::channel(1, async |tx: iced::futures::channel::mpsc::Sender<()>| {
        let mut sender = tx.clone();
        let spawned = std::thread::Builder::new().name("keep-countdown".into()).spawn(move || {
            while !sender.is_closed() {
                std::thread::sleep(Duration::from_millis(500));
                let _ = sender.try_send(());
            }
        });
        if let Err(err) = spawned {
            eprintln!("display countdown: {err}");
        }
        std::future::pending::<()>().await;
        drop(tx);
    })
}

/// The arrangement view: every display that is on, as a rectangle to click (select) and drag (move).
pub struct Arrangement<'a> {
    pub displays: Vec<&'a Display>,
    pub selected: Option<&'a str>,
    pub tokens: Tokens,
}

/// A drag in progress: which display, where it was grabbed (logical px from its corner), where it is now.
#[derive(Default)]
pub struct Drag {
    held: Option<(usize, Point, Point)>,
}

const PAD: f32 = 16.0;
/// Snap reach in screen px, whatever the zoom.
const PULL: f32 = 12.0;

impl Arrangement<'_> {
    /// Logical → canvas: the scale and the canvas point of logical 0, 0 that fit every display in `bounds`, centred.
    fn fit(&self, bounds: Size) -> (f32, Point) {
        let areas: Vec<Area> = self.displays.iter().map(|d| Area::of(d)).collect();
        let (x0, y0) = areas.iter().fold((i32::MAX, i32::MAX), |(x, y), a| (x.min(a.x), y.min(a.y)));
        let (x1, y1) = areas.iter().fold((i32::MIN, i32::MIN), |(x, y), a| (x.max(a.x + a.w), y.max(a.y + a.h)));
        if areas.is_empty() {
            return (1.0, Point::ORIGIN);
        }
        let (w, h) = ((x1 - x0) as f32, (y1 - y0) as f32);
        // Room around them to drag one beside the others.
        let k = ((bounds.width - 2.0 * PAD) / (w * 1.6)).min((bounds.height - 2.0 * PAD) / (h * 1.6)).min(0.2);
        let origin = Point::new((bounds.width - w * k) / 2.0 - x0 as f32 * k, (bounds.height - h * k) / 2.0 - y0 as f32 * k);
        (k, origin)
    }

    /// The display under canvas point `p`.
    fn hit(&self, bounds: Size, p: Point) -> Option<usize> {
        let (k, o) = self.fit(bounds);
        self.displays.iter().rposition(|d| {
            let a = Area::of(d);
            Rectangle::new(Point::new(o.x + a.x as f32 * k, o.y + a.y as f32 * k), Size::new(a.w as f32 * k, a.h as f32 * k)).contains(p)
        })
    }

    /// Where display `i` lands if dropped with its corner at logical `at`.
    fn landing(&self, i: usize, at: Point, k: f32) -> (i32, i32) {
        let moved = Area { x: at.x.round() as i32, y: at.y.round() as i32, ..Area::of(self.displays[i]) };
        let others: Vec<Area> = self.displays.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, d)| Area::of(d)).collect();
        if others.is_empty() {
            return (0, 0);
        }
        snap(moved, &others, (PULL / k).round() as i32)
    }
}

impl canvas::Program<Message> for Arrangement<'_> {
    type State = Drag;

    fn update(&self, drag: &mut Drag, event: &Event, bounds: Rectangle, cursor: mouse::Cursor) -> Option<canvas::Action<Message>> {
        let (k, o) = self.fit(bounds.size());
        let logical = |p: Point| Point::new((p.x - o.x) / k, (p.y - o.y) / k);
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_in(bounds)?;
                let i = self.hit(bounds.size(), p)?;
                let d = self.displays[i];
                let at = Point::new(d.x as f32, d.y as f32);
                drag.held = Some((i, Point::new(logical(p).x - at.x, logical(p).y - at.y), at));
                Some(canvas::Action::publish(Message::SelectDisplay(d.name.clone())).and_capture())
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let (i, grab, _) = drag.held?;
                let p = logical(cursor.position_in(bounds).or(cursor.position().map(|p| Point::new(p.x - bounds.x, p.y - bounds.y)))?);
                drag.held = Some((i, grab, Point::new(p.x - grab.x, p.y - grab.y)));
                Some(canvas::Action::request_redraw().and_capture())
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let (i, _, at) = drag.held.take()?;
                let (x, y) = self.landing(i, at, k);
                let d = self.displays[i];
                if (x, y) == (d.x, d.y) {
                    return Some(canvas::Action::request_redraw());
                }
                let mut places: Vec<(String, i32, i32)> = self.displays.iter().map(|d| (d.name.clone(), d.x, d.y)).collect();
                places[i] = (d.name.clone(), x, y);
                normalise(&mut places);
                Some(canvas::Action::publish(Message::Arrange(places)).and_capture())
            }
            _ => None,
        }
    }

    fn draw(&self, drag: &Drag, renderer: &iced::Renderer, _: &iced::Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        let t = self.tokens;
        let mut frame = Frame::new(renderer, bounds.size());
        let (k, o) = self.fit(bounds.size());
        for (i, d) in self.displays.iter().enumerate() {
            let (x, y) = match drag.held {
                Some((held, _, at)) if held == i => self.landing(i, at, k),
                _ => (d.x, d.y),
            };
            let rect = Rectangle::new(Point::new(o.x + x as f32 * k, o.y + y as f32 * k), Size::new(d.width as f32 * k, d.height as f32 * k));
            let selected = self.selected == Some(d.name.as_str());
            // Inset by a pixel so neighbours' edges show apart.
            let face = Path::rounded_rectangle(Point::new(rect.x + 1.0, rect.y + 1.0), Size::new(rect.width - 2.0, rect.height - 2.0), 6.0.into());
            let fill = if t.dark { Color::from_rgb8(0x3A, 0x5A, 0x8C) } else { Color::from_rgb8(0x8F, 0xB8, 0xE8) };
            frame.fill(&face, fill);
            if selected {
                frame.stroke(&face, Stroke::default().with_color(t.accent).with_width(2.5));
            }
            // The primary one has the menu bar: a white strip along its top.
            if d.primary {
                let bar = Path::rounded_rectangle(
                    Point::new(rect.x + 1.0, rect.y + 1.0),
                    Size::new(rect.width - 2.0, (rect.height * 0.08).clamp(4.0, 8.0)),
                    iced::border::Radius { top_left: 6.0, top_right: 6.0, bottom_right: 0.0, bottom_left: 0.0 },
                );
                frame.fill(&bar, Color { a: 0.9, ..Color::WHITE });
            }
            frame.fill_text(Text {
                content: crate::pages::display_name(&d.name),
                position: rect.center(),
                max_width: rect.width - 8.0,
                color: Color::WHITE,
                size: 12.0.into(),
                font: crate::font(iced::font::Weight::Semibold),
                align_x: iced::advanced::text::Alignment::Center,
                align_y: iced::alignment::Vertical::Center,
                ..Text::default()
            });
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(&self, drag: &Drag, bounds: Rectangle, cursor: mouse::Cursor) -> mouse::Interaction {
        match drag.held {
            Some(_) => mouse::Interaction::Grabbing,
            None if cursor.position_in(bounds).and_then(|p| self.hit(bounds.size(), p)).is_some() => mouse::Interaction::Grab,
            None => mouse::Interaction::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(x: i32, y: i32, w: i32, h: i32) -> Area {
        Area { x, y, w, h }
    }

    #[test]
    fn dropped_displays_meet_an_edge() {
        let laptop = area(0, 0, 1536, 960);
        let monitor = |x, y| area(x, y, 2560, 1440);
        assert_eq!(snap(monitor(1700, 30), &[laptop], 40), (1536, 0), "a gap closes; tops line up within reach");
        assert_eq!(snap(monitor(1400, 300), &[laptop], 40), (1536, 300), "an overlap is pushed out to the edge");
        assert_eq!(snap(monitor(-2700, -500), &[laptop], 40), (-2560, -480), "bottoms line up within reach");
        assert_eq!(snap(monitor(-2500, -2000), &[laptop], 40), (-2500, -1440), "above, sharing part of the edge");
        assert_eq!(snap(monitor(5000, 5000), &[laptop], 40), (1535, 960), "far away: the nearest spot, still touching");
        assert_eq!(snap(area(10, 1000, 1536, 960), &[area(0, 0, 1536, 960)], 40), (0, 960), "left edges line up below");
        assert_eq!(snap(area(500, 980, 500, 300), &[laptop], 40), (518, 960), "centred under it within reach");
    }

    #[test]
    fn snapping_avoids_a_third_display() {
        let a = area(0, 0, 1000, 1000);
        let b = area(1000, 0, 1000, 1000);
        // Right of `a` is taken by `b`; the next best spot that overlaps neither.
        let (x, y) = snap(area(900, 50, 1000, 1000), &[a, b], 40);
        assert!(![a, b].iter().any(|o| area(x, y, 1000, 1000).overlaps(*o)), "{x},{y}");
    }

    #[test]
    fn layout_starts_at_zero() {
        let mut places = vec![("eDP-1".to_string(), -1536, 200), ("HDMI-A-1".to_string(), 0, 0)];
        normalise(&mut places);
        assert_eq!(places, [("eDP-1".to_string(), 0, 200), ("HDMI-A-1".to_string(), 1536, 0)]);
    }

    #[test]
    fn keep_counts_down_then_reverts() {
        let start = Instant::now();
        let keep = Keep { previous: String::new(), since: start };
        assert_eq!(keep.left(start), 15);
        assert_eq!(keep.left(start + Duration::from_millis(200)), 15, "rounded up");
        assert_eq!(keep.left(start + Duration::from_secs(14)), 1);
        assert!(!keep.expired(start + Duration::from_millis(14_999)));
        assert!(keep.expired(start + KEEP_FOR));
        assert!(keep.expired(start + Duration::from_secs(60)));
    }
}
