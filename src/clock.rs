//! The menu-bar clock (E2.4): its text, and when it next has to change. Also the World Clock widget's rows (E4.2).

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

/// The World Clock's name for an IANA zone: its last part, `_` as spaces (`America/New_York` → `New York`).
pub fn city(zone: &str) -> String {
    zone.rsplit('/').next().unwrap_or(zone).replace('_', " ")
}

/// The World Clock's rows, (city, zone), for `[widgets] clocks`; names zoneinfo doesn't know are left out.
pub fn world_clocks(names: &[String]) -> Vec<(String, crate::ical::Zone)> {
    names
        .iter()
        .filter_map(|name| match crate::ical::Zone::named(name) {
            crate::ical::Zone::Local => None,
            zone => Some((city(name), zone)),
        })
        .collect()
}

/// A World Clock row at unix time `t`, for a zone `there` seconds east of UTC seen from `here`: its time (`9:41 PM`,
/// `21:41`) and how far it is from here (`Tomorrow, +5:45`, `Yesterday, −8 h`, `Today`).
pub fn world(t: i64, there: i32, here: i32, hour24: bool) -> (String, String) {
    let wall = |offset: i32| DateTime::from_timestamp(t + i64::from(offset), 0).unwrap_or_default();
    let (at, home) = (wall(there), wall(here));
    let day = match (at.date_naive() - home.date_naive()).num_days() {
        0 => "Today",
        1.. => "Tomorrow",
        _ => "Yesterday",
    };
    let diff = there - here;
    let sign = if diff < 0 { '−' } else { '+' };
    let (h, m) = (diff.abs() / 3600, diff.abs() % 3600 / 60);
    let ahead = match (diff, m) {
        (0, _) => String::new(),
        (_, 0) => format!(", {sign}{h} h"),
        _ => format!(", {sign}{h}:{m:02}"),
    };
    (text(&at, Format { hour24, weekday: false, date: false, seconds: false }), format!("{day}{ahead}"))
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
    fn world_clock_rows() {
        assert_eq!(city("America/Argentina/Buenos_Aires"), "Buenos Aires");
        assert_eq!(city("UTC"), "UTC");
        // 21:41 UTC on Saturday 17 October, seen from Kathmandu (+5:45), where it is already 3:26 on the 18th.
        let (t, ktm) = (at(21, 41, 0, 0).timestamp(), 20_700);
        assert_eq!(world(t, 3600, ktm, false), ("10:41 PM".into(), "Yesterday, −4:45".into()));
        assert_eq!(world(t, 9 * 3600, ktm, true), ("06:41".into(), "Today, +3:15".into()));
        assert_eq!(world(t, ktm, ktm, false), ("3:26 AM".into(), "Today".into()));
        // Seen from UTC: whole hours.
        assert_eq!(world(t, 9 * 3600, 0, true), ("06:41".into(), "Tomorrow, +9 h".into()));
        assert_eq!(world(t, -4 * 3600, 0, false), ("5:41 PM".into(), "Today, −4 h".into()));
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
