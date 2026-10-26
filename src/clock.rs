//! The menu-bar clock (E2.4): its text, and when it next has to change.

use std::time::Duration;

use chrono::{DateTime, TimeZone, Timelike};
use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct Format {
    pub hour24: bool,
    pub weekday: bool,
    pub date: bool,
    pub seconds: bool,
}

impl Default for Format {
    /// `Sat 17 Oct  9:41 AM`.
    fn default() -> Self {
        Self { hour24: false, weekday: true, date: true, seconds: false }
    }
}

/// `Sat 17 Oct  9:41 AM`, `21:41:07`, …: two spaces between the date and the time.
pub fn text<Tz: TimeZone>(now: &DateTime<Tz>, format: Format) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let mut pattern = String::new();
    if format.weekday {
        pattern.push_str("%a ");
    }
    if format.date {
        pattern.push_str("%-d %b ");
    }
    if !pattern.is_empty() {
        pattern.push(' ');
    }
    pattern.push_str(match (format.hour24, format.seconds) {
        (true, false) => "%H:%M",
        (true, true) => "%H:%M:%S",
        (false, false) => "%-I:%M %p",
        (false, true) => "%-I:%M:%S %p",
    });
    now.format(&pattern).to_string()
}

/// Time until the text next changes: the next minute boundary (second, with seconds shown). Re-arm a monotonic timer
/// with this after every tick, so the clock stays aligned even when the wall clock jumps (NTP, suspend, zone change).
pub fn until_next_tick<Tz: TimeZone>(now: &DateTime<Tz>, seconds: bool) -> Duration {
    const SECOND: u64 = 1_000_000_000;
    // A leap second shows up as nanosecond() >= 1e9: treat it as the end of the second.
    let into_second = u64::from(now.nanosecond()).min(SECOND - 1);
    let (into, period) = if seconds { (into_second, SECOND) } else { (u64::from(now.second()) * SECOND + into_second, 60 * SECOND) };
    Duration::from_nanos(period - into)
}

#[cfg(test)]
mod tests {
    use chrono::{FixedOffset, Utc};

    use super::*;

    const TIME: Format = Format { hour24: false, weekday: false, date: false, seconds: false };

    fn at(h: u32, m: u32, s: u32, ms: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 17, h, m, s).unwrap() + chrono::Duration::milliseconds(ms.into())
    }

    #[test]
    fn formats() {
        let now = at(21, 41, 7, 0);
        let full = Format::default();
        assert_eq!(text(&now, full), "Sat 17 Oct  9:41 PM");
        assert_eq!(text(&now, Format { hour24: true, ..full }), "Sat 17 Oct  21:41");
        assert_eq!(text(&now, Format { hour24: true, seconds: true, ..TIME }), "21:41:07");
        assert_eq!(text(&at(9, 5, 0, 0), TIME), "9:05 AM");
        assert_eq!(text(&at(0, 5, 0, 0), Format { hour24: true, ..TIME }), "00:05");
        assert_eq!(text(&at(0, 5, 0, 0), TIME), "12:05 AM");
    }

    #[test]
    fn local_offset_is_used() {
        let tokyo = FixedOffset::east_opt(9 * 3600).unwrap();
        let now = at(21, 41, 0, 0).with_timezone(&tokyo);
        assert_eq!(text(&now, Format { weekday: true, hour24: true, ..TIME }), "Sun  06:41");
    }

    #[test]
    fn ticks_land_on_boundaries() {
        assert_eq!(until_next_tick(&at(9, 41, 0, 0), false), Duration::from_secs(60));
        assert_eq!(until_next_tick(&at(9, 41, 59, 750), false), Duration::from_millis(250));
        assert_eq!(until_next_tick(&at(9, 41, 30, 0), false), Duration::from_secs(30));
        assert_eq!(until_next_tick(&at(9, 41, 30, 400), true), Duration::from_millis(600));
        assert_eq!(until_next_tick(&at(9, 41, 30, 0), true), Duration::from_secs(1));
    }
}
