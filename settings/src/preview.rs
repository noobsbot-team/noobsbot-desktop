//! Trackpad ▸ each gesture's preview (E7.1): a small pad at the row's left with the fingers moving the way the
//! gesture goes while the row is under the pointer; still otherwise, and with Reduce motion: the fingers with a
//! trail from where they start. The canvas asks for the next frame only while it plays, so nothing ticks at rest.

use iced::{
    Color, Element, Length, Point, Rectangle, Size, mouse,
    time::Instant,
    widget::{
        canvas::{self, Event, Frame, Geometry, LineCap, Path, Stroke},
        row, space, stack,
    },
    window,
};

use crate::{App, Message, config::Config, style::Tokens};

/// One loop of the motion, in seconds.
const LOOP: f32 = 1.5;
/// Still at the start and the end of a loop, this share of it each.
const HOLD: f32 = 0.2;
/// The pad, drawn in the room the row keeps at its left.
const PAD: Size = Size::new(48.0, 32.0);
/// A finger's radius in px, and the gap between side-by-side fingers on a 1 × 1 pad.
const DOT: f32 = 2.5;
const GAP: f32 = 0.16;

/// Fingers on a 1 × 1 pad and how they move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gesture {
    pub fingers: usize,
    /// Where they start (their middle), and how far they slide.
    pub at: (f32, f32),
    pub slide: (f32, f32),
    /// On a ring whose radius goes from .0 to .1 (a pinch), rather than side by side.
    pub ring: Option<(f32, f32)>,
    /// A tap first; any slide follows it (tap to drag).
    pub tap: bool,
}

const fn moving(fingers: usize, at: (f32, f32), slide: (f32, f32)) -> Gesture {
    Gesture { fingers, at, slide, ring: None, tap: false }
}

/// A swipe through the pad's middle.
fn swipe(fingers: usize, slide: (f32, f32)) -> Gesture {
    moving(fingers, (0.5 - slide.0 / 2.0, 0.5 - slide.1 / 2.0), slide)
}

fn tap(fingers: usize, at: (f32, f32)) -> Gesture {
    Gesture { tap: true, ..moving(fingers, at, (0.0, 0.0)) }
}

fn pinch(from: f32, to: f32) -> Gesture {
    Gesture { ring: Some((from, to)), ..moving(4, (0.5, 0.5), (0.0, 0.0)) }
}

/// The gesture Trackpad row `key` shows, with the finger counts and methods the settings have now; `None`: a row
/// without one.
pub fn gesture(key: &str, config: &Config) -> Option<Gesture> {
    // As the desktop reads it (src/config.rs `effective_swipe_fingers`): 4, or while three-finger drag is on; else 3.
    let fingers = if config.num("touchpad.swipe_fingers") == 4.0 || config.bool("touchpad.three_finger_drag") { 4 } else { 3 };
    Some(match key {
        "touchpad.tap_to_click" => tap(1, (0.5, 0.5)),
        "touchpad.secondary_click" => match config.str(key) {
            "two-fingers" => tap(2, (0.5, 0.5)),
            "corner" => tap(1, (0.84, 0.76)),
            _ => return None,
        },
        "touchpad.natural_scroll" | "touchpad.scroll_method" => match config.str("touchpad.scroll_method") {
            "edge" => moving(1, (0.88, 0.75), (0.0, -0.5)),
            "none" => return None,
            _ => swipe(2, (0.0, -0.5)),
        },
        "touchpad.tap_to_drag" | "touchpad.drag_lock" => Gesture { tap: true, ..swipe(1, (0.5, 0.0)) },
        "touchpad.three_finger_drag" => swipe(3, (0.5, 0.0)),
        "touchpad.swipe_fingers" | "touchpad.gesture_spaces" => swipe(fingers, (-0.3, 0.0)),
        "touchpad.gesture_mission_control" => swipe(fingers, (0.0, -0.5)),
        "touchpad.gesture_app_expose" => swipe(fingers, (0.0, 0.5)),
        "touchpad.gesture_launchpad" => pinch(0.32, 0.12),
        "touchpad.gesture_show_desktop" => pinch(0.12, 0.32),
        _ => return None,
    })
}

/// The fingers at `t` through a loop (0 … 1): still for `HOLD` at each end (a tap's slide waits for the second
/// half), easing in and out between.
pub fn dots(g: Gesture, t: f32) -> Vec<(f32, f32)> {
    let from = if g.tap { 0.5 } else { HOLD };
    let p = ((t - from) / (1.0 - HOLD - from)).clamp(0.0, 1.0);
    let p = p * p * (3.0 - 2.0 * p);
    let (x, y) = (g.at.0 + g.slide.0 * p, g.at.1 + g.slide.1 * p);
    let n = g.fingers as f32;
    (0..g.fingers)
        .map(|i| {
            let i = i as f32;
            match g.ring {
                Some((r0, r1)) => {
                    let (r, a) = (r0 + (r1 - r0) * p, std::f32::consts::TAU * i / n + std::f32::consts::FRAC_PI_4);
                    (x + r * a.cos(), y + r * a.sin())
                }
                None => (x + (i - (n - 1.0) / 2.0) * GAP, y),
            }
        })
        .collect()
}

/// A Trackpad row with room at its left for a preview, drawn over the row when it has a gesture.
pub fn room<'a>(app: &App, key: &str, line: Element<'a, Message>) -> Element<'a, Message> {
    let room = row![space().width(12.0 + PAD.width).height(PAD.height + 12.0), line];
    match gesture(key, &app.config) {
        Some(gesture) => {
            let preview = Preview { gesture, still: app.config.bool("appearance.reduce_motion"), tokens: app.tokens };
            stack![room, canvas::Canvas::new(preview).width(Length::Fill).height(Length::Fill)].into()
        }
        None => room.into(),
    }
}

pub struct Preview {
    gesture: Gesture,
    /// Reduce motion: never plays.
    still: bool,
    tokens: Tokens,
}

impl canvas::Program<Message> for Preview {
    /// While playing: when it started, and the seconds since then at the frame being drawn.
    type State = Option<(Instant, f32)>;

    fn update(&self, clock: &mut Self::State, event: &Event, bounds: Rectangle, cursor: mouse::Cursor) -> Option<canvas::Action<Message>> {
        let over = !self.still && cursor.is_over(bounds);
        match event {
            Event::Window(window::Event::RedrawRequested(now)) if over => {
                let start = clock.map_or(*now, |(start, _)| start);
                *clock = Some((start, now.duration_since(start).as_secs_f32()));
                Some(canvas::Action::request_redraw())
            }
            Event::Window(window::Event::RedrawRequested(_)) => {
                *clock = None;
                None
            }
            // The pointer came or went: start or stop on the next frame.
            Event::Mouse(_) if over != clock.is_some() => Some(canvas::Action::request_redraw()),
            _ => None,
        }
    }

    fn draw(&self, clock: &Self::State, renderer: &iced::Renderer, _: &iced::Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        let (t, g) = (self.tokens, self.gesture);
        let mut frame = Frame::new(renderer, bounds.size());
        let pad = Rectangle::new(Point::new(12.0, (bounds.height - PAD.height) / 2.0), PAD);
        let face = Path::rounded_rectangle(pad.position(), pad.size(), 6.0.into());
        frame.fill(&face, t.fill());
        frame.stroke(&face, Stroke::default().with_color(t.separator()).with_width(1.0));
        let at = |(x, y): (f32, f32)| Point::new(pad.x + x * pad.width, pad.y + y * pad.height);
        let ring = |frame: &mut Frame, p: (f32, f32), r: f32, a: f32| {
            frame.stroke(&Path::circle(at(p), r), Stroke::default().with_color(Color { a, ..t.accent }).with_width(1.0));
        };
        match *clock {
            Some((_, seconds)) => {
                let phase = seconds / LOOP % 1.0;
                // The fingers land and lift: in and out over a tenth of the loop.
                let a = (phase.min(1.0 - phase) / 0.1).min(1.0);
                // A tap's ripple, through the first half's second quarter.
                let q = (phase - 0.25) / 0.25;
                for p in dots(g, phase) {
                    frame.fill(&Path::circle(at(p), DOT), Color { a, ..t.accent });
                    if g.tap && (0.0..1.0).contains(&q) {
                        ring(&mut frame, p, DOT + 5.0 * q, 1.0 - q);
                    }
                }
            }
            None => {
                let trail = Stroke::default().with_color(Color { a: 0.35, ..t.accent }).with_width(DOT * 2.0).with_line_cap(LineCap::Round);
                for (from, to) in dots(g, 0.0).into_iter().zip(dots(g, 1.0)) {
                    frame.stroke(&Path::line(at(from), at(to)), trail);
                    frame.fill(&Path::circle(at(to), DOT), t.accent);
                    if g.tap {
                        ring(&mut frame, from, DOT + 2.5, 0.6);
                    }
                }
            }
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: &[(f32, f32)], b: &[(f32, f32)]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(a, b)| (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4)
    }

    #[test]
    fn swipe_eases_from_start_to_end() {
        let g = swipe(3, (-0.3, 0.0));
        assert!(near(&dots(g, 0.0), &[(0.49, 0.5), (0.65, 0.5), (0.81, 0.5)]));
        assert!(near(&dots(g, 0.1), &dots(g, 0.0)), "held at the start");
        assert!(near(&dots(g, 0.5), &[(0.34, 0.5), (0.5, 0.5), (0.66, 0.5)]));
        assert!(near(&dots(g, 1.0), &[(0.19, 0.5), (0.35, 0.5), (0.51, 0.5)]));
        // Eased: a quarter of the way through the slide has gone less than a quarter of the distance.
        assert!(dots(g, 0.35)[1].0 > 0.65 - 0.3 / 4.0);
    }

    #[test]
    fn pinch_closes_and_spread_opens_around_the_middle() {
        let radii = |g, t| dots(g, t).iter().map(|&(x, y)| ((x - 0.5).powi(2) + (y - 0.5).powi(2)).sqrt()).collect::<Vec<_>>();
        let close = |r: Vec<f32>, want: f32| r.len() == 4 && r.iter().all(|r| (r - want).abs() < 1e-4);
        let launchpad = pinch(0.32, 0.12);
        assert!(close(radii(launchpad, 0.0), 0.32));
        assert!(close(radii(launchpad, 0.5), 0.22));
        assert!(close(radii(launchpad, 1.0), 0.12));
        assert!(close(radii(pinch(0.12, 0.32), 1.0), 0.32));
    }

    #[test]
    fn a_tap_drags_only_after_it() {
        let g = Gesture { tap: true, ..swipe(1, (0.5, 0.0)) };
        assert!(near(&dots(g, 0.45), &[(0.25, 0.5)]));
        assert!(near(&dots(g, 1.0), &[(0.75, 0.5)]));
    }

    #[test]
    fn rows_follow_their_settings() {
        let dir = std::env::temp_dir().join(format!("noobsbot-settings-test-preview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (defaults, _) = Config::load(dir.join("missing.toml"));
        let fingers = |key, config: &Config| gesture(key, config).map(|g| g.fingers);
        assert_eq!(fingers("touchpad.gesture_spaces", &defaults), Some(3));
        assert_eq!(fingers("touchpad.natural_scroll", &defaults), Some(2));
        assert_eq!(fingers("touchpad.secondary_click", &defaults), Some(2));
        assert_eq!(fingers("touchpad.gesture_launchpad", &defaults), Some(4));
        assert_eq!(gesture("touchpad.gesture_mission_control", &defaults).map(|g| g.slide), Some((0.0, -0.5)));
        assert_eq!(gesture("touchpad.speed", &defaults), None);
        assert_eq!(fingers("touchpad.three_finger_drag", &defaults), Some(3));

        let path = dir.join("config.toml");
        std::fs::write(&path, "[touchpad]\nswipe_fingers = 4\nsecondary_click = \"off\"\nscroll_method = \"edge\"\n").unwrap();
        let (config, _) = Config::load(path.clone());
        assert_eq!(fingers("touchpad.gesture_app_expose", &config), Some(4));
        assert_eq!(fingers("touchpad.natural_scroll", &config), Some(1));
        assert_eq!(gesture("touchpad.secondary_click", &config), None);

        std::fs::write(&path, "[touchpad]\nthree_finger_drag = true\n").unwrap();
        let (config, _) = Config::load(path);
        assert_eq!(fingers("touchpad.gesture_spaces", &config), Some(4), "three-finger drag holds three fingers");
        let _ = std::fs::remove_dir_all(dir);
    }
}
