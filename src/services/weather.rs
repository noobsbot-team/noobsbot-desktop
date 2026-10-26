//! The Weather widget's forecast (E7.2), from Open-Meteo: no account, no key, nothing sent but the place. Off by
//! default; while on, every 30 minutes. A city is geocoded once and its coordinates kept in the cache file
//! (`~/.local/state/noobsbot-desktop/weather.json`: Open-Meteo's response with our `place` in front), which is also
//! what shows, marked stale, while offline. Never located by IP.

use std::{
    path::PathBuf,
    process::Command,
    sync::mpsc::{self, RecvTimeoutError},
    time::{Duration, SystemTime},
};

use smithay::reexports::calloop::channel::Sender;

use crate::{
    config::{self, Units},
    symbols::Symbol,
};

const EVERY: Duration = Duration::from_secs(30 * 60);
const HOURS: usize = 6;

/// One forecast as the widget shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Weather {
    pub place: String,
    pub temp: f32,
    pub code: u8,
    pub day: bool,
    pub high: f32,
    pub low: f32,
    /// From this hour on: (hour 0–23, code, day, temperature).
    pub hours: Vec<(u32, u8, bool, f32)>,
    /// When it was fetched (unix seconds).
    pub fetched: i64,
}

impl Weather {
    /// Older than one missed refresh: shown with "Updated … ago".
    pub fn stale(&self, now: i64) -> bool {
        now - self.fetched > 45 * 60
    }

    /// The canned forecast for `--show weather`.
    pub fn sample() -> Weather {
        let hours = [(15, 1, true, 22.0), (16, 2, true, 22.0), (17, 2, true, 21.0), (18, 3, false, 19.0), (19, 61, false, 17.0), (20, 0, false, 16.0)];
        Weather {
            place: "Kathmandu".into(),
            temp: 22.4,
            code: 1,
            day: true,
            high: 24.0,
            low: 13.0,
            hours: hours.to_vec(),
            fetched: chrono::Utc::now().timestamp(),
        }
    }
}

/// WMO weather code → the words and symbol macOS would use.
pub fn condition(code: u8, day: bool) -> (&'static str, Symbol) {
    match code {
        0 if day => ("Sunny", Symbol::Sun),
        0 => ("Clear", Symbol::Moon),
        1 if day => ("Mostly Sunny", Symbol::Sun),
        1 => ("Mostly Clear", Symbol::Moon),
        2 => ("Partly Cloudy", if day { Symbol::CloudSun } else { Symbol::CloudMoon }),
        3 => ("Cloudy", Symbol::Cloud),
        45 | 48 => ("Fog", Symbol::CloudFog),
        51..=57 => ("Drizzle", Symbol::CloudRain),
        61..=67 => ("Rain", Symbol::CloudRain),
        80..=82 => ("Showers", Symbol::CloudRain),
        71..=77 | 85 | 86 => ("Snow", Symbol::CloudSnow),
        95..=99 => ("Thunderstorms", Symbol::CloudLightning),
        _ => ("", Symbol::Cloud),
    }
}

/// Its own thread, like Software Update (curl blocks): the cache at once, then a fetch now and every 30 minutes
/// while enabled. Disabled, it blocks until the settings change (no wakeups). Send it every changed `[weather]`.
pub fn spawn(settings: config::Weather, found: Sender<Option<Weather>>) -> mpsc::Sender<config::Weather> {
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::Builder::new().name("weather".into()).spawn(move || {
        let mut settings = settings;
        // Nothing to say until there is a forecast (so `--show weather`'s sample stays).
        let mut sent = false;
        loop {
            let shown = settings.enabled.then(|| refresh(&settings)).flatten();
            crate::sun::set_place(place(&settings));
            if shown.is_some() || sent {
                sent = true;
                if found.send(shown).is_err() {
                    return;
                }
            }
            let next = if settings.enabled { rx.recv_timeout(EVERY) } else { rx.recv().map_err(|_| RecvTimeoutError::Disconnected) };
            match next {
                Ok(new) => settings = new,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    });
    if let Err(err) = thread {
        tracing::warn!("weather off: {err}");
    }
    tx
}

fn cache_path() -> PathBuf {
    let state = std::env::var("XDG_STATE_HOME").unwrap_or_else(|_| format!("{}/.local/state", std::env::var("HOME").unwrap_or_default()));
    PathBuf::from(state).join("noobsbot-desktop/weather.json")
}

/// A fresh forecast, else the cached one (if it is for these settings).
fn refresh(settings: &config::Weather) -> Option<Weather> {
    let path = cache_path();
    let cached = std::fs::read_to_string(&path).ok().filter(|c| cached_for(c, settings));
    let mtime = |p: &PathBuf| p.metadata().and_then(|m| m.modified()).ok()?.duration_since(SystemTime::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64);
    let now = chrono::Utc::now();
    let place = match cached.as_deref().and_then(|c| section(c, "place")) {
        // Same city: no second geocoding request.
        Some(place) => Some((unquote(scalar(place, "name")?), scalar(place, "latitude")?.parse().ok()?, scalar(place, "longitude")?.parse().ok()?)),
        None if !settings.city.trim().is_empty() => geocode(settings.city.trim()),
        None if (settings.latitude, settings.longitude) != (0.0, 0.0) => Some((String::new(), settings.latitude, settings.longitude)),
        None => None,
    };
    if let Some((name, lat, lon)) = place
        && let Some(json) = forecast(lat, lon, settings.units)
    {
        let query = clean(settings.city.trim());
        let units = units(settings.units);
        let text = format!(r#"{{"place":{{"name":"{}","query":"{query}","units":"{units}","latitude":{lat},"longitude":{lon}}},{}"#, clean(&name), &json[1..]);
        let saved = path.parent().map(std::fs::create_dir_all).transpose().and_then(|_| std::fs::write(&path, &text));
        if let Err(err) = saved {
            tracing::debug!("weather cache: {err}");
        }
        return parse(&text, now.timestamp(), now);
    }
    let cached = cached?;
    parse(&cached, mtime(&path)?, now)
}

/// The typed coordinates, else the city as the cache geocoded it (no request): for sunset and sunrise (sun.rs).
fn place(settings: &config::Weather) -> Option<(f64, f64)> {
    let city = clean(settings.city.trim());
    if city.is_empty() {
        return ((settings.latitude, settings.longitude) != (0.0, 0.0)).then_some((settings.latitude, settings.longitude));
    }
    let cache = std::fs::read_to_string(cache_path()).ok()?;
    let place = section(&cache, "place").filter(|p| scalar(p, "query").map(unquote) == Some(city))?;
    Some((scalar(place, "latitude")?.parse().ok()?, scalar(place, "longitude")?.parse().ok()?))
}

/// Whether the cache answers these settings: same city (or coordinates, without one) and units.
fn cached_for(cache: &str, settings: &config::Weather) -> bool {
    let Some(place) = section(cache, "place") else { return false };
    let num = |key| scalar(place, key).and_then(|v| v.parse::<f64>().ok());
    let city = clean(settings.city.trim());
    let same_place = scalar(place, "query").map(unquote) == Some(city.clone())
        && (!city.is_empty() || (num("latitude") == Some(settings.latitude) && num("longitude") == Some(settings.longitude)));
    same_place && scalar(place, "units").map(unquote).as_deref() == Some(units(settings.units))
}

fn units(units: Units) -> &'static str {
    match units {
        Units::Celsius => "celsius",
        Units::Fahrenheit => "fahrenheit",
    }
}

/// Text we put in the cache's JSON strings: no quotes or backslashes.
fn clean(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, '"' | '\\') && !c.is_control()).collect()
}

fn unquote(s: &str) -> String {
    s.trim_matches('"').to_owned()
}

// ponytail: curl (on every install) instead of an HTTP + TLS crate, as in update.rs.
fn curl(args: &[&str]) -> Option<String> {
    let agent = concat!("noobsbot-desktop/", env!("CARGO_PKG_VERSION"));
    let output = Command::new("curl").args(["-fsSL", "--max-time", "20", "-A", agent]).args(args).output().ok()?;
    if !output.status.success() {
        tracing::debug!("weather: curl exited with {}", output.status);
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// The first Open-Meteo match for `city`: (its name, latitude, longitude).
fn geocode(city: &str) -> Option<(String, f64, f64)> {
    let json =
        curl(&["--get", "--data-urlencode", &format!("name={city}"), "-d", "count=1", "-d", "language=en", "https://geocoding-api.open-meteo.com/v1/search"])?;
    let name = scalar(&json, "name").map_or_else(|| city.to_owned(), unquote);
    Some((name, scalar(&json, "latitude")?.parse().ok()?, scalar(&json, "longitude")?.parse().ok()?))
}

fn forecast(lat: f64, lon: f64, unit: Units) -> Option<String> {
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m,weather_code,is_day\
         &hourly=temperature_2m,weather_code,is_day&daily=temperature_2m_max,temperature_2m_min&timezone=auto&forecast_days=2\
         &temperature_unit={}",
        units(unit)
    );
    curl(&[&url]).filter(|json| json.starts_with('{') && json.contains("\"hourly\":{"))
}

/// The inside of `"name":{…}` (Open-Meteo's sections hold no nested objects).
fn section<'a>(json: &'a str, name: &str) -> Option<&'a str> {
    let start = json.find(&format!("\"{name}\":{{"))? + name.len() + 4;
    Some(&json[start..start + json[start..].find('}')?])
}

/// `"key":value` → value, as written (strings keep their quotes).
fn scalar<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let start = json.find(&format!("\"{key}\":"))? + key.len() + 3;
    let rest = &json[start..];
    let end = if let Some(quoted) = rest.strip_prefix('"') { quoted.find('"')? + 2 } else { rest.find([',', '}', ']']).unwrap_or(rest.len()) };
    Some(&rest[..end])
}

/// `"key":[a,b,…]` → its items, unquoted.
fn array<'a>(json: &'a str, key: &str) -> Vec<&'a str> {
    let Some(start) = json.find(&format!("\"{key}\":[")).map(|i| i + key.len() + 4) else { return Vec::new() };
    let end = json[start..].find(']').map_or(start, |e| start + e);
    json[start..end].split(',').map(|s| s.trim_matches('"')).collect()
}

/// The cache file's text as the widget shows it at `now`: the next hours and today's high and low are the place's own.
fn parse(json: &str, fetched: i64, now: chrono::DateTime<chrono::Utc>) -> Option<Weather> {
    let place = section(json, "place")?;
    let current = section(json, "current")?;
    let (hourly, daily) = (section(json, "hourly")?, section(json, "daily")?);
    let offset = scalar(json, "utc_offset_seconds").and_then(|s| s.parse().ok()).unwrap_or(0);
    let local = (now + chrono::Duration::seconds(offset)).naive_utc();
    let f = |s: &str| s.parse::<f32>().ok();
    let u = |s: &str| s.parse::<u8>().ok();

    let today = local.format("%Y-%m-%d").to_string();
    let day = array(daily, "time").iter().position(|d| *d == today).unwrap_or(0);
    let (highs, lows) = (array(daily, "temperature_2m_max"), array(daily, "temperature_2m_min"));

    let this_hour = local.format("%Y-%m-%dT%H:00").to_string();
    let times = array(hourly, "time");
    let (temps, codes, days) = (array(hourly, "temperature_2m"), array(hourly, "weather_code"), array(hourly, "is_day"));
    let first = times.iter().position(|t| *t >= this_hour.as_str()).unwrap_or(times.len());
    let mut hours: Vec<_> = (first..times.len())
        .take(HOURS)
        .filter_map(|i| Some((times[i].get(11..13)?.parse().ok()?, u(codes.get(i)?)?, *days.get(i)? == "1", f(temps.get(i)?)?)))
        .collect();
    let (temp, code) = (f(scalar(current, "temperature_2m")?)?, u(scalar(current, "weather_code")?)?);
    let is_day = scalar(current, "is_day") != Some("0");
    // "Now" is the current conditions while they are this hour's.
    if let Some(now) = hours.first_mut().filter(|_| scalar(current, "time").and_then(|t| t.get(1..14)) == this_hour.get(..13)) {
        *now = (now.0, code, is_day, temp);
    }

    Some(Weather { place: unquote(scalar(place, "name")?), temp, code, day: is_day, high: f(highs.get(day)?)?, low: f(lows.get(day)?)?, hours, fetched })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CACHE: &str = r#"{"place":{"name":"Kathmandu","query":"Kathmandu","units":"celsius","latitude":27.70169,"longitude":85.3206},"latitude":27.732864,"longitude":85.34831,"utc_offset_seconds":20700,"timezone":"Asia/Kathmandu","current_units":{"time":"iso8601","temperature_2m":"°C"},"current":{"time":"2026-10-18T15:00","interval":900,"temperature_2m":22.6,"weather_code":0,"is_day":1},"hourly_units":{"time":"iso8601","temperature_2m":"°C"},"hourly":{"time":["2026-10-18T14:00","2026-10-18T15:00","2026-10-18T16:00","2026-10-18T17:00","2026-10-18T18:00","2026-10-18T19:00","2026-10-18T20:00","2026-10-18T21:00"],"temperature_2m":[23.0,22.2,21.5,20.1,18.0,17.2,16.5,16.0],"weather_code":[0,0,2,3,61,61,1,0],"is_day":[1,1,1,1,0,0,0,0]},"daily_units":{"time":"iso8601"},"daily":{"time":["2026-10-18","2026-10-19"],"temperature_2m_max":[24.1,25.3],"temperature_2m_min":[13.2,14.0]}}"#;

    fn at(utc: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(utc).unwrap().into()
    }

    #[test]
    fn parses_in_the_places_time() {
        // 09:30 UTC is 15:15 in Kathmandu (+5:45): the row starts at 15:00.
        let w = parse(CACHE, 7, at("2026-10-18T09:30:00Z")).unwrap();
        assert_eq!((w.place.as_str(), w.temp, w.code, w.day, w.high, w.low, w.fetched), ("Kathmandu", 22.6, 0, true, 24.1, 13.2, 7));
        assert_eq!(w.hours.len(), HOURS);
        assert_eq!(w.hours[0], (15, 0, true, 22.6), "Now is the current conditions");
        assert_eq!(w.hours[1], (16, 2, true, 21.5));
        assert_eq!(w.hours[3], (18, 61, false, 18.0));
        // Offline the next morning (local): today is the 19th, and no hours are left.
        let later = parse(CACHE, 7, at("2026-10-19T03:00:00Z")).unwrap();
        assert_eq!((later.high, later.low, later.hours.len()), (25.3, 14.0, 0));
        assert!(later.stale(at("2026-10-19T03:00:00Z").timestamp()));
    }

    #[test]
    fn garbage_is_none() {
        assert_eq!(parse("", 0, at("2026-10-18T09:30:00Z")), None);
        assert_eq!(parse(r#"{"error":true,"reason":"bad"}"#, 0, at("2026-10-18T09:30:00Z")), None);
    }

    #[test]
    fn cache_matches_its_settings() {
        let city = config::Weather { enabled: true, city: "Kathmandu".into(), ..Default::default() };
        assert!(cached_for(CACHE, &city));
        assert!(!cached_for(CACHE, &config::Weather { units: Units::Fahrenheit, ..city.clone() }));
        assert!(!cached_for(CACHE, &config::Weather { city: "Pokhara".into(), ..city }));
        let coords = config::Weather { latitude: 27.70169, longitude: 85.3206, ..Default::default() };
        assert!(!cached_for(CACHE, &coords), "a city's cache isn't the coordinates' (its query is set)");
    }

    #[test]
    fn json_pieces() {
        assert_eq!(scalar(r#"{"a":"x,y","b":2}"#, "a"), Some(r#""x,y""#));
        assert_eq!(scalar(r#"{"a":"x","b":2}"#, "b"), Some("2"));
        assert_eq!(array(r#"{"t":["a","b"]}"#, "t"), vec!["a", "b"]);
        assert_eq!(clean(r#"Ka"th\mandu"#), "Kathmandu");
        assert_eq!(condition(2, false).1, Symbol::CloudMoon);
    }
}
