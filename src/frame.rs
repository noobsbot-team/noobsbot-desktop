//! Frame scheduling (E1.10): draw as late as safely possible before the next vblank, so a frame includes the newest
//! input; fall back to drawing right after vblank when the estimate keeps missing (E1.11).

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

/// Render time assumed before any frame was measured.
const FIRST_ESTIMATE: Duration = Duration::from_millis(4);
/// Safety margin on top of the measured render time.
const MARGIN: Duration = Duration::from_millis(1);
/// Frames in the render-time moving average.
const SAMPLES: usize = 30;
/// Misses in a row before falling back, and how long the fallback lasts.
const MISSES: u32 = 3;
const FALLBACK: Duration = Duration::from_secs(1);

pub struct FrameClock {
    refresh: Duration,
    last_vblank: Option<Instant>,
    render_times: VecDeque<Duration>,
    /// The vblank the frame being drawn aims for.
    target: Option<Instant>,
    misses: u32,
    fallback_until: Option<Instant>,
}

impl FrameClock {
    pub fn new(refresh: Duration) -> Self {
        Self { refresh, last_vblank: None, render_times: VecDeque::with_capacity(SAMPLES), target: None, misses: 0, fallback_until: None }
    }

    fn render_estimate(&self) -> Duration {
        match self.render_times.len() {
            0 => FIRST_ESTIMATE,
            n => self.render_times.iter().sum::<Duration>() / n as u32,
        }
    }

    /// The first vblank after `now`, from the last one seen and the refresh period.
    fn next_vblank(&self, now: Instant) -> Option<Instant> {
        let last = self.last_vblank?;
        let periods = now.saturating_duration_since(last).as_nanos() / self.refresh.as_nanos().max(1) + 1;
        Some(last + self.refresh * periods as u32)
    }

    /// How long to wait from `now` before drawing; zero means draw at once. Remembers the vblank it aims for.
    pub fn delay(&mut self, now: Instant) -> Duration {
        let fallback = self.fallback_until.is_some_and(|until| now < until);
        let Some(next) = self.next_vblank(now).filter(|_| !fallback) else {
            self.target = None;
            return Duration::ZERO;
        };
        self.target = Some(next);
        (next - self.render_estimate() - MARGIN).saturating_duration_since(now)
    }

    /// A frame reached the screen at `at` (the vblank) after `render_time` of drawing.
    pub fn presented(&mut self, at: Instant, render_time: Duration) {
        if self.render_times.len() == SAMPLES {
            self.render_times.pop_front();
        }
        self.render_times.push_back(render_time);
        self.last_vblank = Some(at);
        // Shown more than half a period after the vblank it aimed for: the deadline was missed.
        let missed = self.target.take().is_some_and(|target| at > target + self.refresh / 2);
        self.misses = if missed { self.misses + 1 } else { 0 };
        if self.misses >= MISSES {
            tracing::debug!(estimate = ?self.render_estimate(), "3 frame deadlines missed, drawing right after vblank for 1 s");
            self.misses = 0;
            self.fallback_until = Some(at + FALLBACK);
            self.render_times.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HZ60: Duration = Duration::from_nanos(16_666_667);

    #[test]
    fn first_frame_draws_at_once_then_as_late_as_possible() {
        let mut clock = FrameClock::new(HZ60);
        let t0 = Instant::now();
        assert_eq!(clock.delay(t0), Duration::ZERO, "no vblank seen yet");
        clock.presented(t0, Duration::from_millis(2));
        // 3 ms after a vblank: the next is at 16.67 ms, drawing (2 ms) + margin (1 ms) starts at 13.67 ms.
        let delay = clock.delay(t0 + Duration::from_millis(3));
        assert_eq!(delay, HZ60 - Duration::from_millis(3) - Duration::from_millis(3));
        // Past that point: draw at once, for the vblank after.
        assert_eq!(clock.delay(t0 + Duration::from_millis(15)), Duration::ZERO);
    }

    #[test]
    fn three_misses_fall_back_for_a_second() {
        let mut clock = FrameClock::new(HZ60);
        let mut t = Instant::now();
        clock.presented(t, Duration::from_millis(2));
        for _ in 0..3 {
            clock.delay(t + Duration::from_millis(1));
            // Shown two periods later than aimed for.
            t += HZ60 * 3;
            clock.presented(t, Duration::from_millis(2));
        }
        assert_eq!(clock.delay(t + Duration::from_millis(1)), Duration::ZERO, "fallback: draw right away");
        assert!(clock.delay(t + Duration::from_millis(1001)) > Duration::ZERO, "re-measures after 1 s");
    }
}
