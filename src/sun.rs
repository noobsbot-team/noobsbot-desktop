//! Sunrise and sunset (NOAA's general solar position approximation, within a few minutes below the polar circles), for
//! `auto` dark mode and Night Light (E8.4). The place is the one typed for weather; nothing is located by IP.

use std::sync::Mutex;

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};

/// (lat, lon) of the weather place, kept current by the weather thread.
static PLACE: Mutex<Option<(f64, f64)>> = Mutex::new(None);

pub fn place() -> Option<(f64, f64)> {
    *PLACE.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn set_place(place: Option<(f64, f64)>) {
    *PLACE.lock().unwrap_or_else(|e| e.into_inner()) = place;
}

/// Sunrise and sunset on `date` at (`lat`, `lon`) (degrees, east positive), in minutes after that date's 00:00 UTC
/// (either may be below 0 or above 1440). `None`: the sun stays up or down all day.
pub fn times(date: NaiveDate, lat: f64, lon: f64) -> Option<(f64, f64)> {
    let g = 2.0 * std::f64::consts::PI / 365.0 * f64::from(date.ordinal0());
    let eqtime = 229.18 * (0.000075 + 0.001868 * g.cos() - 0.032077 * g.sin() - 0.014615 * (2.0 * g).cos() - 0.040849 * (2.0 * g).sin());
    let decl = 0.006918 - 0.399912 * g.cos() + 0.070257 * g.sin() - 0.006758 * (2.0 * g).cos() + 0.000907 * (2.0 * g).sin() - 0.002697 * (3.0 * g).cos()
        + 0.00148 * (3.0 * g).sin();
    let lat = lat.to_radians();
    // 90.833°: refraction and the sun's radius.
    let cos_ha = 90.833_f64.to_radians().cos() / (lat.cos() * decl.cos()) - lat.tan() * decl.tan();
    if !(-1.0..=1.0).contains(&cos_ha) {
        return None;
    }
    let ha = cos_ha.acos().to_degrees();
    Some((720.0 - 4.0 * (lon + ha) - eqtime, 720.0 - 4.0 * (lon - ha) - eqtime))
}

/// Is the sun down at `now` at `place` (lat, lon)? `None` under midnight sun or polar night.
pub fn is_night(now: DateTime<Utc>, (lat, lon): (f64, f64)) -> Option<bool> {
    // The place's own solar day, so the sunrise compared is the one coming, not yesterday's.
    let date = (now + Duration::minutes((lon * 4.0) as i64)).date_naive();
    let minutes = (now - date.and_hms_opt(0, 0, 0)?.and_utc()).num_seconds() as f64 / 60.0;
    let (rise, set) = times(date, lat, lon)?;
    Some(minutes < rise || minutes >= set)
}

/// `hour` falls from `from` until `until` (whole hours, local), across midnight when `from` is the later one.
pub fn within(from: u32, until: u32, hour: u32) -> bool {
    let (from, until) = (from % 24, until % 24);
    if from <= until { (from..until).contains(&hour) } else { hour >= from || hour < until }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn london_midsummer() {
        // Published: sunrise 03:43, sunset 20:21 UTC on 21 June 2024.
        let (rise, set) = times(NaiveDate::from_ymd_opt(2024, 6, 21).unwrap(), 51.5074, -0.1278).unwrap();
        assert!((rise - 223.0).abs() < 3.0 && (set - 1221.0).abs() < 3.0, "{rise} {set}");
        // Kathmandu (UTC+5:45), the solar day starting the evening before in UTC.
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().to_utc();
        let ktm = (27.7172, 85.324);
        assert_eq!(is_night(at("2024-06-21T12:00:00+05:45"), ktm), Some(false));
        assert_eq!(is_night(at("2024-06-21T21:00:00+05:45"), ktm), Some(true));
        assert_eq!(is_night(at("2024-06-21T04:30:00+05:45"), ktm), Some(true));
        assert_eq!(is_night(at("2024-06-21T05:30:00+05:45"), ktm), Some(false));
        // Midnight sun in Tromsø.
        assert_eq!(times(NaiveDate::from_ymd_opt(2024, 6, 21).unwrap(), 69.65, 18.96), None);
    }
}
