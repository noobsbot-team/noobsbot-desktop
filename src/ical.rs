//! A small iCalendar (`.ics`, RFC 5545) reader for calendar events (E7.2): VEVENTs with SUMMARY, LOCATION,
//! DTSTART/DTEND/DURATION (UTC, floating, TZID or all-day), a simple RRULE (FREQ DAILY/WEEKLY/MONTHLY/YEARLY with
//! INTERVAL, COUNT, UNTIL, BYDAY), EXDATE, RECURRENCE-ID overrides (moved, renamed or cancelled occurrences) and VALARM
//! TRIGGERs, and the occurrences and alarms they make.
// ponytail: no RDATE, RANGE=THISANDFUTURE, BYMONTH/BYMONTHDAY/BYSETPOS or YEARLY BYDAY, and an override only finds its
// event in the same file (CalDAV's cache is one file); add them when real calendars need them.

use std::{collections::HashMap, path::Path, sync::Arc};

use chrono::{Datelike, Days, Local, Months, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, TimeZone, Weekday};

/// Days of periods a rule is followed for, at most: 110 years of one that repeats every period.
const MAX_DAYS: u32 = 40_000;

/// Where a wall-clock time is.
#[derive(Debug, Clone, PartialEq)]
pub enum Zone {
    Utc,
    /// Floating times, all-day events and zones we can't find: the desktop's own zone.
    Local,
    Tz(Arc<Tzif>),
}

impl Zone {
    /// The zone a TZID names, from the system's zoneinfo; unknown names fall back to `Local`.
    pub fn named(tzid: &str) -> Zone {
        let tzid = tzid.trim_matches('"');
        if matches!(tzid, "UTC" | "Etc/UTC" | "GMT" | "Z") {
            return Zone::Utc;
        }
        if tzid.is_empty() || tzid.split('/').any(|part| part.is_empty() || part.starts_with('.')) {
            return Zone::Local;
        }
        std::fs::read(Path::new("/usr/share/zoneinfo").join(tzid)).ok().and_then(|b| Tzif::parse(&b)).map_or(Zone::Local, |z| Zone::Tz(Arc::new(z)))
    }

    /// Unix seconds of wall time `t` here. Times in a spring-forward gap move on by the gap.
    pub fn timestamp(&self, t: NaiveDateTime) -> i64 {
        match self {
            Zone::Utc => t.and_utc().timestamp(),
            Zone::Local => Local
                .from_local_datetime(&t)
                .earliest()
                .or_else(|| Local.from_local_datetime(&t.checked_add_signed(TimeDelta::hours(1))?).earliest())
                .map_or_else(|| t.and_utc().timestamp(), |d| d.timestamp()),
            Zone::Tz(tz) => {
                let wall = t.and_utc().timestamp();
                wall - i64::from(tz.offset(wall - i64::from(tz.offset(wall))))
            }
        }
    }

    /// Seconds east of UTC at unix time `t`.
    pub fn offset(&self, t: i64) -> i32 {
        match self {
            Zone::Utc => 0,
            Zone::Local => Local.timestamp_opt(t, 0).single().map_or(0, |d| d.offset().local_minus_utc()),
            Zone::Tz(tz) => tz.offset(t),
        }
    }
}

/// A compiled zoneinfo (TZif) file: UTC offsets and when they start.
// ponytail: ignores the footer TZ rule, so times past the last transition (2037 in fat files) keep the last
// offset; Arch ships fat files. Parse the footer if a distro ships slim ones.
#[derive(Debug, Clone, PartialEq)]
pub struct Tzif {
    times: Vec<i64>,
    offsets: Vec<i32>,
    initial: i32,
}

impl Tzif {
    pub fn parse(b: &[u8]) -> Option<Tzif> {
        let be32 = |at: usize| Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?));
        let counts = |at: usize| -> Option<[usize; 6]> {
            (b.get(at..at + 4)? == b"TZif").then_some(())?;
            let mut c = [0; 6];
            for (i, n) in c.iter_mut().enumerate() {
                *n = be32(at + 20 + 4 * i)? as usize;
            }
            Some(c)
        };
        let [isut, isstd, leap, time, types, chars] = counts(0)?;
        // Version 2+ repeats the data with 64-bit times after the 32-bit block.
        let (at, wide) = match b.get(4)? {
            b'2'..=b'9' => (44 + time * 5 + types * 6 + chars + leap * 8 + isstd + isut, true),
            _ => (0, false),
        };
        let [_, _, _, time, types, _] = counts(at)?;
        let size = if wide { 8 } else { 4 };
        let data = at + 44;
        let index_at = data + time * size;
        let types_at = index_at + time;
        let utoff = |i: usize| (i < types).then(|| be32(types_at + i * 6).map(|v| v as i32)).flatten();
        // No capacity from the header: a forged count would allocate before the bounds checks below fail.
        let mut times = Vec::new();
        let mut offsets = Vec::new();
        for i in 0..time {
            let raw = b.get(data + i * size..data + (i + 1) * size)?;
            times.push(if wide { i64::from_be_bytes(raw.try_into().ok()?) } else { i64::from(i32::from_be_bytes(raw.try_into().ok()?)) });
            offsets.push(utoff(usize::from(*b.get(index_at + i)?))?);
        }
        Some(Tzif { times, offsets, initial: utoff(0)? })
    }

    /// Seconds east of UTC at unix time `t`.
    pub fn offset(&self, t: i64) -> i32 {
        match self.times.partition_point(|s| *s <= t) {
            0 => self.initial,
            i => self.offsets[i - 1],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone, PartialEq)]
struct Rule {
    freq: Freq,
    interval: u32,
    count: Option<u32>,
    /// Unix seconds, inclusive.
    until: Option<i64>,
    /// (ordinal, day): `2MO` is (2, Mon), `-1FR` (-1, Fri), plain `TU` (0, Tue).
    by_day: Vec<(i32, Weekday)>,
    /// 1 to 12.
    by_month: Vec<u32>,
    /// 1 to 31, or from the month's end when negative (-1 the last day).
    by_month_day: Vec<i32>,
    /// Which of each period's days are kept: 1 the first, -1 the last.
    by_set_pos: Vec<i32>,
}

/// When an alarm goes off.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Trigger {
    /// Seconds from the start (negative: before).
    Start(i64),
    End(i64),
    /// Unix seconds.
    At(i64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub summary: String,
    pub location: String,
    pub all_day: bool,
    /// Wall-clock start in `zone`.
    start: NaiveDateTime,
    zone: Zone,
    /// End minus start, in wall time.
    length: TimeDelta,
    rule: Option<Rule>,
    alarms: Vec<Trigger>,
    uid: String,
    /// An override's RECURRENCE-ID: the start (unix seconds) of its event's occurrence it replaces.
    replaces: Option<i64>,
    /// Starts (unix seconds) that don't happen: EXDATEs and the occurrences overrides replace.
    skip: Vec<i64>,
    /// STATUS:CANCELLED.
    cancelled: bool,
}

/// One time an event happens, in unix seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Occurrence<'a> {
    pub event: &'a Event,
    pub start: i64,
    pub end: i64,
}

impl Event {
    /// Calls `f` with each start (wall time) from about `near` on, in order, until it returns false or the rule ends.
    /// Without a COUNT, the periods before `near` are skipped; with one they must be counted.
    fn each(&self, near: NaiveDate, mut f: impl FnMut(NaiveDateTime) -> bool) {
        let Some(rule) = &self.rule else {
            f(self.start);
            return;
        };
        let (date, time) = (self.start.date(), self.start.time());
        let mut n = 0;
        let skip = match (rule.count, rule.freq) {
            (Some(_), _) => 0,
            (None, Freq::Daily) => (near - date).num_days(),
            (None, Freq::Weekly) => (near - date).num_days() / 7,
            (None, Freq::Monthly) => i64::from(near.year() - date.year()) * 12 + i64::from(near.month()) - i64::from(date.month()),
            (None, Freq::Yearly) => i64::from(near.year() - date.year()),
        };
        // A period is looked at day by day, so each frequency gets the same span.
        let periods = MAX_DAYS / period_days(rule.freq);
        // One period early, so nothing at the edge is missed.
        let first = u32::try_from((skip / i64::from(rule.interval) - 1).max(0)).unwrap_or(u32::MAX - periods);
        for k in first..first + periods {
            let Some(days) = k.checked_mul(rule.interval).and_then(|step| period(rule, date, step)) else { return };
            for day in days.into_iter().filter(|d| *d >= date) {
                let t = day.and_time(time);
                if rule.count.is_some_and(|c| n >= c) || rule.until.is_some_and(|u| self.zone.timestamp(t) > u) {
                    return;
                }
                n += 1;
                if !f(t) {
                    return;
                }
            }
        }
    }

    fn occurrence(&self, t: NaiveDateTime) -> Occurrence<'_> {
        // A length past the end of time (a hostile DURATION) makes a zero-length occurrence rather than a panic.
        let end = t.checked_add_signed(self.length).unwrap_or(t);
        Occurrence { event: self, start: self.zone.timestamp(t), end: self.zone.timestamp(end) }
    }

    /// Occurrences overlapping `from..to` (unix seconds); a zero-length one counts at its start.
    pub fn between(&self, from: i64, to: i64) -> Vec<Occurrence<'_>> {
        let mut out = Vec::new();
        // Two days early covers any zone offset.
        let near =
            chrono::DateTime::from_timestamp(from.saturating_sub(self.length.num_seconds() + 2 * 86_400), 0).map_or(self.start.date(), |d| d.date_naive());
        self.each(near, |t| {
            let o = self.occurrence(t);
            if o.start >= to {
                return false;
            }
            if (o.end > from || o.start >= from) && !self.skip.contains(&o.start) {
                out.push(o);
            }
            true
        });
        out
    }

    /// Alarm times in `from..=to` (unix seconds), with the occurrence each is for.
    pub fn alarms(&self, from: i64, to: i64) -> Vec<(i64, Occurrence<'_>)> {
        // Saturating: a TRIGGER near i64's ends comes from a file, not a calendar.
        let lead = self.alarms.iter().map(|a| match a {
            Trigger::Start(s) | Trigger::End(s) => s.saturating_abs(),
            Trigger::At(t) => t.saturating_sub(self.zone.timestamp(self.start)).saturating_abs(),
        });
        let lead = lead.max().unwrap_or(0).saturating_add(self.length.num_seconds().saturating_abs());
        let mut out = Vec::new();
        for o in self.between(from.saturating_sub(lead), to.saturating_add(lead).saturating_add(1)) {
            for a in &self.alarms {
                let at = match *a {
                    Trigger::Start(s) => o.start.saturating_add(s),
                    Trigger::End(s) => o.end.saturating_add(s),
                    Trigger::At(t) => t,
                };
                if (from..=to).contains(&at) && !out.contains(&(at, o)) {
                    out.push((at, o));
                }
            }
        }
        out
    }
}

/// Days in a period, at most.
fn period_days(freq: Freq) -> u32 {
    match freq {
        Freq::Daily => 1,
        Freq::Weekly => 7,
        Freq::Monthly => 31,
        Freq::Yearly => 366,
    }
}

/// The days the rule picks in period `step` (in units of its frequency, from `date`'s), in order; `None` past chrono's
/// range.
fn period(rule: &Rule, date: NaiveDate, step: u32) -> Option<Vec<NaiveDate>> {
    let first = match rule.freq {
        Freq::Daily => date.checked_add_days(Days::new(step.into()))?,
        Freq::Weekly => {
            let monday = date.checked_sub_days(Days::new(date.weekday().num_days_from_monday().into()))?;
            monday.checked_add_days(Days::new(u64::from(step) * 7))?
        }
        Freq::Monthly => date.with_day(1)?.checked_add_months(Months::new(step))?,
        Freq::Yearly => NaiveDate::from_ymd_opt(date.year().checked_add(i32::try_from(step).ok()?)?, 1, 1)?,
    };
    let inside = |d: &NaiveDate| match rule.freq {
        Freq::Daily | Freq::Weekly => true,
        Freq::Monthly => d.month() == first.month(),
        Freq::Yearly => d.year() == first.year(),
    };
    let days: Vec<NaiveDate> = first.iter_days().take(period_days(rule.freq) as usize).take_while(inside).filter(|d| rule.matches(*d, date)).collect();
    if rule.by_set_pos.is_empty() {
        return Some(days);
    }
    let at = |n: i32| if n > 0 { days.get(n as usize - 1) } else { days.len().checked_sub(n.unsigned_abs() as usize).and_then(|i| days.get(i)) };
    let mut kept: Vec<NaiveDate> = rule.by_set_pos.iter().filter_map(|n| at(*n)).copied().collect();
    kept.sort();
    kept.dedup();
    Some(kept)
}

impl Rule {
    /// Whether the rule picks day `d` of one of its periods; `start` is DTSTART's date, for what the rule leaves out.
    fn matches(&self, d: NaiveDate, start: NaiveDate) -> bool {
        // Without BYMONTHDAY or BYDAY a rule keeps DTSTART's weekday (weekly), day of the month (monthly) or date
        // (yearly).
        let plain = self.by_day.is_empty() && self.by_month_day.is_empty();
        let month = match self.freq {
            _ if !self.by_month.is_empty() => self.by_month.contains(&d.month()),
            Freq::Yearly if plain => d.month() == start.month(),
            _ => true,
        };
        let (first, last) = month_of(d);
        let month_day = match self.freq {
            _ if !self.by_month_day.is_empty() => {
                let (day, days) = (d.day() as i32, last.day() as i32);
                self.by_month_day.iter().any(|&n| n == day || n == day - days - 1)
            }
            Freq::Monthly | Freq::Yearly if plain => d.day() == start.day(),
            _ => true,
        };
        // An ordinal counts in the month, or in the year for a yearly rule without BYMONTH: `2MO` is the second
        // Monday, `-1FR` the last Friday. Daily and weekly rules have none.
        let (first, last) = match self.freq {
            Freq::Yearly if self.by_month.is_empty() => (d.with_ordinal(1).unwrap_or(d), NaiveDate::from_ymd_opt(d.year(), 12, 31).unwrap_or(d)),
            _ => (first, last),
        };
        let nth = |n: i32| match n {
            _ if n == 0 || matches!(self.freq, Freq::Daily | Freq::Weekly) => true,
            n if n > 0 => (d - first).num_days() / 7 + 1 == i64::from(n),
            n => (last - d).num_days() / 7 + 1 == -i64::from(n),
        };
        let weekday = match self.freq {
            _ if !self.by_day.is_empty() => self.by_day.iter().any(|&(n, wd)| d.weekday() == wd && nth(n)),
            Freq::Weekly if plain => d.weekday() == start.weekday(),
            _ => true,
        };
        month && month_day && weekday
    }
}

/// The first and last day of `d`'s month.
fn month_of(d: NaiveDate) -> (NaiveDate, NaiveDate) {
    let first = d.with_day(1).unwrap_or(d);
    (first, first.checked_add_months(Months::new(1)).and_then(|n| n.pred_opt()).unwrap_or(NaiveDate::MAX))
}

/// Every VEVENT in `text`; anything malformed is skipped.
pub fn parse(text: &str) -> Vec<Event> {
    // Unfold: a line starting with a space or tab continues the one before.
    let mut lines: Vec<String> = Vec::new();
    for line in text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)) {
        match (line.strip_prefix([' ', '\t']), lines.last_mut()) {
            (Some(rest), Some(last)) => last.push_str(rest),
            _ => lines.push(line.to_owned()),
        }
    }
    let mut zones: HashMap<String, Zone> = HashMap::new();
    let mut events = Vec::new();
    let mut props: Option<Vec<(String, Params, String)>> = None;
    let mut alarm = false;
    for line in &lines {
        let Some((name, params, value)) = content_line(line) else { continue };
        match (name.as_str(), value.as_str()) {
            ("BEGIN", "VEVENT") => props = Some(Vec::new()),
            ("END", "VEVENT") => events.extend(props.take().and_then(|p| event(&p, &mut zones))),
            ("BEGIN", "VALARM") => alarm = true,
            ("END", "VALARM") => alarm = false,
            _ => {
                if let Some(p) = props.as_mut() {
                    let name = if alarm { format!("VALARM.{name}") } else { name };
                    p.push((name, params, value));
                }
            }
        }
    }
    // An override replaces its occurrence of the event with the same UID; a cancelled one only removes it.
    let replaced: Vec<(String, i64)> = events.iter().filter(|e| !e.uid.is_empty()).filter_map(|e| Some((e.uid.clone(), e.replaces?))).collect();
    for e in events.iter_mut().filter(|e| e.replaces.is_none()) {
        e.skip.extend(replaced.iter().filter(|(uid, _)| *uid == e.uid).map(|(_, t)| t));
    }
    events.retain(|e| !e.cancelled);
    events
}

type Params = Vec<(String, String)>;

/// `NAME;PARAM=V;…:value`, the name and parameter names upper-cased.
fn content_line(line: &str) -> Option<(String, Params, String)> {
    // The value starts at the first colon outside a quoted parameter value.
    let mut quoted = false;
    let colon = line.char_indices().find(|&(_, c)| {
        quoted ^= c == '"';
        c == ':' && !quoted
    })?;
    let (head, value) = (&line[..colon.0], &line[colon.0 + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.trim().to_ascii_uppercase();
    let params = parts.filter_map(|p| p.split_once('=')).map(|(k, v)| (k.to_ascii_uppercase(), v.trim_matches('"').to_owned())).collect();
    Some((name, params, value.to_owned()))
}

fn param<'a>(params: &'a Params, key: &str) -> Option<&'a str> {
    params.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match (c, chars.clone().next()) {
            ('\\', Some('n' | 'N')) => {
                chars.next();
                out.push('\n');
            }
            ('\\', Some(other)) => {
                chars.next();
                out.push(other);
            }
            (c, _) => out.push(c),
        }
    }
    out
}

/// A DATE or DATE-TIME value: (wall time, zone, date only).
fn date_time(value: &str, params: &Params, zones: &mut HashMap<String, Zone>) -> Option<(NaiveDateTime, Zone, bool)> {
    let value = value.trim();
    if value.len() == 8 || param(params, "VALUE") == Some("DATE") {
        let date = NaiveDate::parse_from_str(value.get(..8)?, "%Y%m%d").ok()?;
        return Some((date.and_time(NaiveTime::MIN), Zone::Local, true));
    }
    let (wall, utc) = match value.strip_suffix(['Z', 'z']) {
        Some(v) => (v, true),
        None => (value, false),
    };
    let t = NaiveDateTime::parse_from_str(wall, "%Y%m%dT%H%M%S").ok()?;
    let zone = match (utc, param(params, "TZID")) {
        (true, _) => Zone::Utc,
        (false, Some(tzid)) => zones.entry(tzid.to_owned()).or_insert_with(|| Zone::named(tzid)).clone(),
        (false, None) => Zone::Local,
    };
    Some((t, zone, false))
}

/// An EXDATE or RECURRENCE-ID value in unix seconds; a floating time is in `zone`, the event's.
fn instant(value: &str, params: &Params, zone: &Zone, zones: &mut HashMap<String, Zone>) -> Option<i64> {
    let (t, z, date) = date_time(value, params, zones)?;
    let floating = !date && z != Zone::Utc && param(params, "TZID").is_none();
    Some(if floating { zone } else { &z }.timestamp(t))
}

/// `[+-]P[nW][nD][T[nH][nM][nS]]` in seconds.
fn duration(value: &str) -> Option<i64> {
    let value = value.trim();
    let (sign, rest) = match value.as_bytes().first()? {
        b'-' => (-1, &value[1..]),
        b'+' => (1, &value[1..]),
        _ => (1, value),
    };
    let rest = rest.strip_prefix(['P', 'p'])?;
    let (mut total, mut n, mut time, mut any) = (0i64, None::<i64>, false, false);
    for c in rest.chars() {
        let unit = match c.to_ascii_uppercase() {
            d if d.is_ascii_digit() => {
                n = Some(n.unwrap_or(0).checked_mul(10)?.checked_add(i64::from(d.to_digit(10)?))?);
                continue;
            }
            'T' => {
                time = true;
                continue;
            }
            'W' if !time => 604_800,
            'D' if !time => 86_400,
            'H' if time => 3600,
            'M' if time => 60,
            'S' if time => 1,
            _ => return None,
        };
        total = total.checked_add(n.take()?.checked_mul(unit)?)?;
        any = true;
    }
    (any && n.is_none()).then_some(sign * total)
}

fn rule(value: &str, zone: &Zone, zones: &mut HashMap<String, Zone>) -> Option<Rule> {
    let mut rule = Rule {
        freq: Freq::Daily,
        interval: 1,
        count: None,
        until: None,
        by_day: Vec::new(),
        by_month: Vec::new(),
        by_month_day: Vec::new(),
        by_set_pos: Vec::new(),
    };
    let mut freq = None;
    for (key, v) in value.split(';').filter_map(|p| p.split_once('=')) {
        match key.to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = Some(match v.to_ascii_uppercase().as_str() {
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    "YEARLY" => Freq::Yearly,
                    // HOURLY and finer: not a calendar's kind of event; show the first one only.
                    _ => return None,
                })
            }
            "INTERVAL" => rule.interval = v.parse().ok().filter(|i| *i > 0)?,
            "COUNT" => rule.count = v.parse().ok(),
            "UNTIL" => {
                let (t, z, date) = date_time(v, &Vec::new(), zones)?;
                // A date-only UNTIL includes that whole day; a floating one is in the event's zone.
                let (t, z) = if date { (t.checked_add_signed(TimeDelta::seconds(86_399))?, zone) } else { (t, if z == Zone::Utc { &z } else { zone }) };
                rule.until = Some(z.timestamp(t));
            }
            "BYMONTHDAY" => rule.by_month_day = numbers(v, 31)?,
            "BYSETPOS" => rule.by_set_pos = numbers(v, 366)?,
            "BYMONTH" => rule.by_month = numbers(v, 12)?.into_iter().map(u32::try_from).collect::<Result<_, _>>().ok()?,
            "BYDAY" => {
                for day in v.split(',') {
                    let day = day.trim().to_ascii_uppercase();
                    let split = day.len().checked_sub(2)?;
                    // `get`: a non-ASCII day ("ÄM") must not split a char.
                    let wd = match day.get(split..)? {
                        "MO" => Weekday::Mon,
                        "TU" => Weekday::Tue,
                        "WE" => Weekday::Wed,
                        "TH" => Weekday::Thu,
                        "FR" => Weekday::Fri,
                        "SA" => Weekday::Sat,
                        "SU" => Weekday::Sun,
                        _ => return None,
                    };
                    let n = match day.get(..split)? {
                        "" => 0,
                        n => n.trim_start_matches('+').parse().ok()?,
                    };
                    rule.by_day.push((n, wd));
                }
            }
            _ => {}
        }
    }
    rule.freq = freq?;
    Some(rule)
}

/// A comma list of `[+-]n`, each nonzero and at most `max` either way; `None` if one isn't.
fn numbers(v: &str, max: i32) -> Option<Vec<i32>> {
    v.split(',').map(|n| n.trim().trim_start_matches('+').parse::<i32>().ok().filter(|n| *n != 0 && (-max..=max).contains(n))).collect()
}

fn event(props: &[(String, Params, String)], zones: &mut HashMap<String, Zone>) -> Option<Event> {
    let get = |name: &str| props.iter().find(|(n, _, _)| n == name);
    let (_, params, value) = get("DTSTART")?;
    let (start, zone, all_day) = date_time(value, params, zones)?;
    let end = get("DTEND").and_then(|(_, p, v)| date_time(v, p, zones)).map(|(t, _, _)| t);
    let length = match (end, get("DURATION").and_then(|(_, _, v)| duration(v))) {
        (Some(end), _) => end - start,
        (None, Some(secs)) => TimeDelta::try_seconds(secs)?,
        (None, None) if all_day => TimeDelta::days(1),
        (None, None) => TimeDelta::zero(),
    };
    let rule = get("RRULE").and_then(|(_, _, v)| rule(v, &zone, zones));
    let alarms = props
        .iter()
        .filter(|(n, _, _)| n == "VALARM.TRIGGER")
        .filter_map(|(_, p, v)| {
            if param(p, "VALUE") == Some("DATE-TIME") {
                let (t, z, _) = date_time(v, p, zones)?;
                return Some(Trigger::At(z.timestamp(t)));
            }
            let secs = duration(v)?;
            Some(if param(p, "RELATED") == Some("END") { Trigger::End(secs) } else { Trigger::Start(secs) })
        })
        .collect();
    let mut skip = Vec::new();
    for (_, p, v) in props.iter().filter(|(n, _, _)| n == "EXDATE") {
        skip.extend(v.split(',').filter_map(|v| instant(v, p, &zone, zones)));
    }
    let replaces = get("RECURRENCE-ID").and_then(|(_, p, v)| instant(v, p, &zone, zones));
    let cancelled = get("STATUS").is_some_and(|(_, _, v)| v.trim().eq_ignore_ascii_case("CANCELLED"));
    let text = |name: &str| get(name).map(|(_, _, v)| unescape(v)).unwrap_or_default();
    Some(Event {
        summary: text("SUMMARY"),
        location: text("LOCATION"),
        all_day,
        start,
        zone,
        length: length.max(TimeDelta::zero()),
        rule,
        alarms,
        uid: text("UID"),
        replaces,
        skip,
        cancelled,
    })
}

/// The `.ics` files `entry` names: a file, a directory (its `.ics` files), or a `dir/*.ics`-style pattern with one `*`
/// in the file name. `~/` is the home directory.
pub fn files(entry: &str) -> Vec<std::path::PathBuf> {
    let path = expand(entry);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
    let (dir, pre, post) = match name.split_once('*') {
        Some((pre, post)) => (path.parent().map(Path::to_owned).unwrap_or_default(), pre, post),
        None if path.is_dir() => (path.clone(), "", ".ics"),
        None => return vec![path],
    };
    let mut out: Vec<_> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.len() >= pre.len() + post.len() && n.starts_with(pre) && n.ends_with(post)))
        .collect();
    out.sort();
    out
}

/// `~/x` → `$HOME/x`.
pub fn expand(entry: &str) -> std::path::PathBuf {
    match (entry.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest),
        _ => entry.into(),
    }
}

/// Every event in the files `entries` name; unreadable files are skipped.
pub fn load(entries: &[String]) -> Vec<Event> {
    entries.iter().flat_map(|e| files(e)).filter_map(|f| std::fs::read_to_string(f).ok()).flat_map(|t| parse(&t)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> i64 {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap().and_utc().timestamp()
    }

    fn one(body: &str) -> Event {
        let text = format!("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\n{body}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n");
        let mut events = parse(&text);
        assert_eq!(events.len(), 1, "{body}");
        events.remove(0)
    }

    fn starts(e: &Event, from: &str, to: &str) -> Vec<i64> {
        e.between(utc(from), utc(to)).iter().map(|o| o.start).collect()
    }

    #[test]
    fn plain_utc_event() {
        let e = one("SUMMARY:Stand-up\\, daily\r\nLOCATION:Room 4\\nFloor 2\r\nDTSTART:20261020T090000Z\r\nDTEND:20261020T093000Z");
        assert_eq!((e.summary.as_str(), e.location.as_str(), e.all_day), ("Stand-up, daily", "Room 4\nFloor 2", false));
        let o = e.between(utc("2026-10-01 00:00"), utc("2026-11-01 00:00"));
        assert_eq!(o.len(), 1);
        assert_eq!((o[0].start, o[0].end), (utc("2026-10-20 09:00"), utc("2026-10-20 09:30")));
        // Outside the window, or ended before it: nothing.
        assert!(e.between(utc("2026-10-20 09:30"), utc("2026-10-21 00:00")).is_empty());
        assert!(e.between(utc("2026-10-19 00:00"), utc("2026-10-20 09:00")).is_empty());
    }

    #[test]
    fn folded_lines_and_quoted_params() {
        let text = "BEGIN:VEVENT\nSUMMARY:A very long\n  title\nDTSTART;TZID=\"Etc/UTC\":20261020T090000\nDURATION:PT1H30M\nEND:VEVENT\n";
        let e = &parse(text)[0];
        assert_eq!(e.summary, "A very long title");
        let o = e.between(utc("2026-10-20 00:00"), utc("2026-10-21 00:00"));
        assert_eq!((o[0].start, o[0].end), (utc("2026-10-20 09:00"), utc("2026-10-20 10:30")));
    }

    #[test]
    fn all_day_events_are_local_dates() {
        let e = one("SUMMARY:Holiday\r\nDTSTART;VALUE=DATE:20261021");
        assert!(e.all_day);
        let o = e.between(i64::MIN / 2, i64::MAX / 2);
        let start = Local.from_local_datetime(&NaiveDate::from_ymd_opt(2026, 10, 21).unwrap().and_time(NaiveTime::MIN)).unwrap();
        assert_eq!(o[0].start, start.timestamp());
        assert_eq!(o[0].end - o[0].start, 86_400);
    }

    #[test]
    fn tzid_uses_zoneinfo() {
        if !Path::new("/usr/share/zoneinfo/America/New_York").exists() {
            return;
        }
        // 9:00 in New York: EDT (UTC-4) in October, EST (UTC-5) after the November change.
        let e = one("DTSTART;TZID=America/New_York:20261030T090000\r\nRRULE:FREQ=WEEKLY;COUNT=2");
        assert_eq!(starts(&e, "2026-10-01 00:00", "2026-12-01 00:00"), [utc("2026-10-30 13:00"), utc("2026-11-06 14:00")]);
        // Kathmandu is +5:45 all year.
        let k = one("DTSTART;TZID=Asia/Kathmandu:20260101T120000");
        assert_eq!(starts(&k, "2025-01-01 00:00", "2027-01-01 00:00"), [utc("2026-01-01 06:15")]);
        // An unknown zone doesn't drop the event, and paths can't escape zoneinfo.
        assert_eq!(Zone::named("Mars/Olympus"), Zone::Local);
        assert_eq!(Zone::named("../../etc/passwd"), Zone::Local);
    }

    #[test]
    fn tzif_offsets() {
        let Ok(bytes) = std::fs::read("/usr/share/zoneinfo/Europe/Berlin") else { return };
        let tz = Tzif::parse(&bytes).unwrap();
        assert_eq!(tz.offset(utc("2026-01-15 12:00")), 3600);
        assert_eq!(tz.offset(utc("2026-07-15 12:00")), 7200);
        // The switch on 29 March 2026 at 01:00 UTC.
        assert_eq!(tz.offset(utc("2026-03-29 00:59")), 3600);
        assert_eq!(tz.offset(utc("2026-03-29 01:00")), 7200);
        assert!(Tzif::parse(b"not a tzif file at all, definitely not").is_none());
        assert!(Tzif::parse(&bytes[..60]).is_none());
    }

    #[test]
    fn daily_with_count_and_interval() {
        let e = one("DTSTART:20261001T080000Z\r\nRRULE:FREQ=DAILY;INTERVAL=2;COUNT=3");
        assert_eq!(starts(&e, "2026-09-01 00:00", "2027-01-01 00:00"), [utc("2026-10-01 08:00"), utc("2026-10-03 08:00"), utc("2026-10-05 08:00")]);
        // A window in the middle only sees what it overlaps.
        assert_eq!(starts(&e, "2026-10-02 00:00", "2026-10-04 00:00"), [utc("2026-10-03 08:00")]);
    }

    #[test]
    fn daily_byday_filters_weekdays() {
        // Thursday 1 October 2026: weekdays only.
        let e = one("DTSTART:20261001T080000Z\r\nRRULE:FREQ=DAILY;BYDAY=MO,TU,WE,TH,FR");
        let s = starts(&e, "2026-10-01 00:00", "2026-10-08 00:00");
        assert_eq!(s, [utc("2026-10-01 08:00"), utc("2026-10-02 08:00"), utc("2026-10-05 08:00"), utc("2026-10-06 08:00"), utc("2026-10-07 08:00")]);
    }

    #[test]
    fn weekly_byday_and_until() {
        // From Tuesday 6 October: Tuesdays and Thursdays, every other week, up to the 22nd inclusive.
        let e = one("DTSTART:20261006T170000Z\r\nRRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=TU,TH;UNTIL=20261022T170000Z");
        assert_eq!(
            starts(&e, "2026-10-01 00:00", "2027-01-01 00:00"),
            [utc("2026-10-06 17:00"), utc("2026-10-08 17:00"), utc("2026-10-20 17:00"), utc("2026-10-22 17:00")]
        );
        // BYDAY days before DTSTART in its first week don't count: Monday the 5th is skipped.
        let e = one("DTSTART:20261006T170000Z\r\nRRULE:FREQ=WEEKLY;BYDAY=MO,TU;COUNT=3");
        assert_eq!(starts(&e, "2026-10-01 00:00", "2027-01-01 00:00"), [utc("2026-10-06 17:00"), utc("2026-10-12 17:00"), utc("2026-10-13 17:00")]);
        // No BYDAY: DTSTART's weekday.
        let e = one("DTSTART:20261006T170000Z\r\nRRULE:FREQ=WEEKLY;COUNT=2");
        assert_eq!(starts(&e, "2026-10-01 00:00", "2027-01-01 00:00"), [utc("2026-10-06 17:00"), utc("2026-10-13 17:00")]);
    }

    #[test]
    fn monthly_by_date_and_by_weekday() {
        // The 31st skips months without one.
        let e = one("DTSTART:20260131T100000Z\r\nRRULE:FREQ=MONTHLY;COUNT=3");
        assert_eq!(starts(&e, "2026-01-01 00:00", "2027-01-01 00:00"), [utc("2026-01-31 10:00"), utc("2026-03-31 10:00"), utc("2026-05-31 10:00")]);
        // Second Monday and last Friday.
        let e = one("DTSTART:20261001T100000Z\r\nRRULE:FREQ=MONTHLY;BYDAY=2MO,-1FR;COUNT=4");
        assert_eq!(
            starts(&e, "2026-10-01 00:00", "2027-01-01 00:00"),
            [utc("2026-10-12 10:00"), utc("2026-10-30 10:00"), utc("2026-11-09 10:00"), utc("2026-11-27 10:00")]
        );
        // Every Wednesday of the month, every third month.
        let e = one("DTSTART:20261001T100000Z\r\nRRULE:FREQ=MONTHLY;INTERVAL=3;BYDAY=WE;COUNT=5");
        let s = starts(&e, "2026-10-01 00:00", "2027-06-01 00:00");
        assert_eq!(s.len(), 5);
        assert_eq!(s[4], utc("2027-01-06 10:00"));
    }

    #[test]
    fn bymonth_expands_yearly_rules_and_limits_others() {
        // Yearly in January and July, on DTSTART's day.
        let e = one("DTSTART:20260110T100000Z\r\nRRULE:FREQ=YEARLY;BYMONTH=1,7;COUNT=4");
        assert_eq!(
            starts(&e, "2026-01-01 00:00", "2028-01-01 00:00"),
            [utc("2026-01-10 10:00"), utc("2026-07-10 10:00"), utc("2027-01-10 10:00"), utc("2027-07-10 10:00")]
        );
        // Monthly on the 5th, in summer only.
        let e = one("DTSTART:20260505T100000Z\r\nRRULE:FREQ=MONTHLY;BYMONTH=6,7,8;COUNT=4");
        assert_eq!(
            starts(&e, "2026-01-01 00:00", "2028-01-01 00:00"),
            [utc("2026-06-05 10:00"), utc("2026-07-05 10:00"), utc("2026-08-05 10:00"), utc("2027-06-05 10:00")]
        );
        // Daily in December only.
        let e = one("DTSTART:20261130T080000Z\r\nRRULE:FREQ=DAILY;BYMONTH=12");
        assert_eq!(starts(&e, "2026-11-29 00:00", "2026-12-03 00:00"), [utc("2026-12-01 08:00"), utc("2026-12-02 08:00")]);
        assert!(starts(&e, "2027-01-01 00:00", "2027-02-01 00:00").is_empty());
        // No 13th or -1st month: the rule is unusable, the event stays.
        for bad in ["13", "-1", "0", "x"] {
            let e = one(&format!("DTSTART:20261020T090000Z\r\nRRULE:FREQ=YEARLY;BYMONTH={bad}"));
            assert_eq!(starts(&e, "2026-01-01 00:00", "2030-01-01 00:00").len(), 1, "{bad}");
        }
    }

    #[test]
    fn bymonthday_expands_monthly_and_yearly_rules_and_limits_daily_ones() {
        // The 15th of each month, from a DTSTART on another day.
        let e = one("DTSTART:20261001T100000Z\r\nRRULE:FREQ=MONTHLY;BYMONTHDAY=15;COUNT=3");
        assert_eq!(starts(&e, "2026-10-01 00:00", "2027-06-01 00:00"), [utc("2026-10-15 10:00"), utc("2026-11-15 10:00"), utc("2026-12-15 10:00")]);
        // The first and the last day: a negative day counts from the month's end, whatever its length.
        let e = one("DTSTART:20260101T100000Z\r\nRRULE:FREQ=MONTHLY;BYMONTHDAY=1,-1;COUNT=6");
        let s = starts(&e, "2026-01-01 00:00", "2027-01-01 00:00");
        let days = ["2026-01-01", "2026-01-31", "2026-02-01", "2026-02-28", "2026-03-01", "2026-03-31"];
        assert_eq!(s, days.map(|d| utc(&format!("{d} 10:00"))));
        // Yearly on February's last day, leap years included.
        let e = one("DTSTART:20270228T100000Z\r\nRRULE:FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=-1;COUNT=2");
        assert_eq!(starts(&e, "2027-01-01 00:00", "2030-01-01 00:00"), [utc("2027-02-28 10:00"), utc("2028-02-29 10:00")]);
        // A daily rule only keeps those days: the 31st skips short months.
        let e = one("DTSTART:20261001T080000Z\r\nRRULE:FREQ=DAILY;BYMONTHDAY=31;COUNT=3");
        assert_eq!(starts(&e, "2026-10-01 00:00", "2027-06-01 00:00"), [utc("2026-10-31 08:00"), utc("2026-12-31 08:00"), utc("2027-01-31 08:00")]);
        for bad in ["32", "-32", "0"] {
            let e = one(&format!("DTSTART:20261020T090000Z\r\nRRULE:FREQ=MONTHLY;BYMONTHDAY={bad}"));
            assert_eq!(starts(&e, "2026-01-01 00:00", "2030-01-01 00:00").len(), 1, "{bad}");
        }
    }

    #[test]
    fn byday_expands_yearly_rules_and_limits_with_bymonthday() {
        // The last Sunday of March.
        let e = one("DTSTART:20260329T010000Z\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU;COUNT=3");
        assert_eq!(starts(&e, "2026-01-01 00:00", "2030-01-01 00:00"), [utc("2026-03-29 01:00"), utc("2027-03-28 01:00"), utc("2028-03-26 01:00")]);
        // Without BYMONTH an ordinal counts in the year: the 20th Monday, the last Friday.
        let e = one("DTSTART:19970519T090000Z\r\nRRULE:FREQ=YEARLY;BYDAY=20MO;COUNT=3");
        assert_eq!(starts(&e, "1997-01-01 00:00", "2000-01-01 00:00"), [utc("1997-05-19 09:00"), utc("1998-05-18 09:00"), utc("1999-05-17 09:00")]);
        let e = one("DTSTART:20260101T090000Z\r\nRRULE:FREQ=YEARLY;BYDAY=-1FR;COUNT=2");
        assert_eq!(starts(&e, "2026-01-01 00:00", "2028-06-01 00:00"), [utc("2026-12-25 09:00"), utc("2027-12-31 09:00")]);
        // Every Thursday in March.
        let e = one("DTSTART:20270304T090000Z\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=TH");
        assert_eq!(starts(&e, "2027-01-01 00:00", "2028-01-01 00:00").len(), 4);
        // With BYMONTHDAY, BYDAY only limits: every Friday the 13th.
        let e = one("DTSTART:20260101T090000Z\r\nRRULE:FREQ=MONTHLY;BYDAY=FR;BYMONTHDAY=13;COUNT=3");
        assert_eq!(starts(&e, "2026-01-01 00:00", "2028-01-01 00:00"), [utc("2026-02-13 09:00"), utc("2026-03-13 09:00"), utc("2026-11-13 09:00")]);
        let e = one("DTSTART:20260101T090000Z\r\nRRULE:FREQ=YEARLY;BYDAY=FR;BYMONTHDAY=13;COUNT=3");
        assert_eq!(starts(&e, "2026-01-01 00:00", "2028-01-01 00:00"), [utc("2026-02-13 09:00"), utc("2026-03-13 09:00"), utc("2026-11-13 09:00")]);
    }

    #[test]
    fn bysetpos_picks_from_each_period() {
        // The last weekday of the month.
        let e = one("DTSTART:20261001T090000Z\r\nRRULE:FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1;COUNT=3");
        assert_eq!(starts(&e, "2026-10-01 00:00", "2027-06-01 00:00"), [utc("2026-10-30 09:00"), utc("2026-11-30 09:00"), utc("2026-12-31 09:00")]);
        // The second Tuesday, written with BYSETPOS.
        let e = one("DTSTART:20261001T090000Z\r\nRRULE:FREQ=MONTHLY;BYDAY=TU;BYSETPOS=2;COUNT=2");
        assert_eq!(starts(&e, "2026-10-01 00:00", "2027-06-01 00:00"), [utc("2026-10-13 09:00"), utc("2026-11-10 09:00")]);
        // RFC 5545's: the third Tuesday, Wednesday or Thursday of the month.
        let e = one("DTSTART:19970904T090000Z\r\nRRULE:FREQ=MONTHLY;COUNT=3;BYDAY=TU,WE,TH;BYSETPOS=3");
        assert_eq!(starts(&e, "1997-01-01 00:00", "1998-01-01 00:00"), [utc("1997-09-04 09:00"), utc("1997-10-07 09:00"), utc("1997-11-06 09:00")]);
        // The first and last weekday, in order; a position past the set picks nothing.
        let e = one("DTSTART:20261001T090000Z\r\nRRULE:FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1,1,40;COUNT=4");
        let s = starts(&e, "2026-10-01 00:00", "2027-06-01 00:00");
        assert_eq!(s, [utc("2026-10-01 09:00"), utc("2026-10-30 09:00"), utc("2026-11-02 09:00"), utc("2026-11-30 09:00")]);
        let e = one("DTSTART:20261001T090000Z\r\nRRULE:FREQ=MONTHLY;BYDAY=MO;BYSETPOS=0");
        assert_eq!(starts(&e, "2026-01-01 00:00", "2030-01-01 00:00").len(), 1, "0 is no position");
    }

    #[test]
    fn yearly_birthday_and_leap_day() {
        let e = one("SUMMARY:Birthday\r\nDTSTART;VALUE=DATE:20000229\r\nRRULE:FREQ=YEARLY");
        let s = e.between(Zone::Utc.timestamp(date(2023, 1, 1)), Zone::Utc.timestamp(date(2029, 1, 1)));
        // Only leap years have a 29 February.
        assert_eq!(s.len(), 2);
        let all = one("DTSTART:20261018T000000Z\r\nRRULE:FREQ=YEARLY;UNTIL=20281018");
        assert_eq!(starts(&all, "2020-01-01 00:00", "2040-01-01 00:00").len(), 3);
    }

    /// Every occurrence's start and summary in `events`, in order.
    fn occurrences(events: &[Event]) -> Vec<(i64, &str)> {
        let mut out: Vec<(i64, &str)> = events.iter().flat_map(|e| e.between(0, i64::MAX / 2)).map(|o| (o.start, o.event.summary.as_str())).collect();
        out.sort();
        out
    }

    #[test]
    fn exdates_skip_occurrences() {
        // Weekly from Tuesday 6 October; the 13th is skipped but still counts towards COUNT.
        let e = one("DTSTART:20261006T170000Z\r\nRRULE:FREQ=WEEKLY;COUNT=3\r\nEXDATE:20261013T170000Z");
        assert_eq!(starts(&e, "2026-10-01 00:00", "2027-01-01 00:00"), [utc("2026-10-06 17:00"), utc("2026-10-20 17:00")]);
        // A comma list and a second EXDATE.
        let e = one("DTSTART:20261006T170000Z\r\nRRULE:FREQ=WEEKLY;COUNT=5\r\nEXDATE:20261013T170000Z,20261020T170000Z\r\nEXDATE:20261103T170000Z");
        assert_eq!(starts(&e, "2026-10-01 00:00", "2027-01-01 00:00"), [utc("2026-10-06 17:00"), utc("2026-10-27 17:00")]);
    }

    #[test]
    fn all_day_exdate() {
        let e = one("DTSTART;VALUE=DATE:20261019\r\nRRULE:FREQ=DAILY;COUNT=3\r\nEXDATE;VALUE=DATE:20261020");
        let midnight = |d| Zone::Local.timestamp(date(2026, 10, d));
        let s: Vec<i64> = e.between(0, i64::MAX / 2).iter().map(|o| o.start).collect();
        assert_eq!(s, [midnight(19), midnight(21)]);
    }

    #[test]
    fn exdate_with_tzid() {
        if !Path::new("/usr/share/zoneinfo/America/New_York").exists() {
            return;
        }
        // 9:00 New York each Friday: the 6 November one (14:00 UTC, after the change) is skipped, given in its own
        // zone; a floating EXDATE is in the event's zone too.
        let e = one(
            "DTSTART;TZID=America/New_York:20261030T090000\r\nRRULE:FREQ=WEEKLY;COUNT=4\r\nEXDATE;TZID=America/New_York:20261106T090000\r\nEXDATE:20261120T090000",
        );
        assert_eq!(starts(&e, "2026-10-01 00:00", "2027-01-01 00:00"), [utc("2026-10-30 13:00"), utc("2026-11-13 14:00")]);
    }

    #[test]
    fn overrides_move_and_cancel_occurrences() {
        let master = "BEGIN:VEVENT\nUID:sync@x\nSUMMARY:Sync\nDTSTART:20261006T170000Z\nDTEND:20261006T173000Z\nRRULE:FREQ=WEEKLY;COUNT=3\nEND:VEVENT\n";
        // Before its event in the file: the 13th moves to Wednesday morning, renamed.
        let moved = "BEGIN:VEVENT\nUID:sync@x\nRECURRENCE-ID:20261013T170000Z\nSUMMARY:Sync (moved)\nDTSTART:20261014T090000Z\nEND:VEVENT\n";
        let events = parse(&format!("{moved}{master}"));
        assert_eq!(occurrences(&events), [(utc("2026-10-06 17:00"), "Sync"), (utc("2026-10-14 09:00"), "Sync (moved)"), (utc("2026-10-20 17:00"), "Sync")]);
        // A cancelled one only removes it; another UID's override doesn't touch it.
        let cancelled = "BEGIN:VEVENT\nUID:sync@x\nRECURRENCE-ID:20261020T170000Z\nSTATUS:CANCELLED\nDTSTART:20261020T170000Z\nEND:VEVENT\n";
        let other = "BEGIN:VEVENT\nUID:other@x\nRECURRENCE-ID:20261006T170000Z\nSUMMARY:Other\nDTSTART:20261007T170000Z\nEND:VEVENT\n";
        let events = parse(&format!("{master}{cancelled}{other}"));
        assert_eq!(occurrences(&events), [(utc("2026-10-06 17:00"), "Sync"), (utc("2026-10-07 17:00"), "Other"), (utc("2026-10-13 17:00"), "Sync")]);
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d).unwrap().and_time(NaiveTime::MIN)
    }

    #[test]
    fn endless_rules_stop_at_the_window() {
        let e = one("DTSTART:19700101T000000Z\r\nRRULE:FREQ=DAILY");
        assert_eq!(starts(&e, "2026-10-18 00:00", "2026-10-20 00:00"), [utc("2026-10-18 00:00"), utc("2026-10-19 00:00")]);
        // Old endless rules jump to the window, keeping their phase.
        let e = one("DTSTART:20200115T100000Z\r\nRRULE:FREQ=MONTHLY;INTERVAL=2");
        assert_eq!(starts(&e, "2026-10-01 00:00", "2026-12-01 00:00"), [utc("2026-11-15 10:00")]);
        let e = one("DTSTART:19900301T100000Z\r\nRRULE:FREQ=WEEKLY;INTERVAL=3;BYDAY=TH");
        let s = starts(&e, "2026-10-01 00:00", "2026-11-01 00:00");
        assert!(s.windows(2).all(|w| w[1] - w[0] == 21 * 86_400) && !s.is_empty());
        // A rule that never matches ends at the period cap instead of spinning.
        let e = one("DTSTART:20260131T000000Z\r\nRRULE:FREQ=MONTHLY;BYDAY=6MO");
        assert!(e.between(0, i64::MAX / 2).is_empty());
    }

    #[test]
    fn bad_input_is_skipped() {
        assert!(parse("").is_empty());
        assert!(parse("BEGIN:VEVENT\nSUMMARY:no start\nEND:VEVENT").is_empty());
        assert!(parse("BEGIN:VEVENT\nDTSTART:garbage\nEND:VEVENT").is_empty());
        // An unusable RRULE leaves the single event.
        let e = one("DTSTART:20261020T090000Z\r\nRRULE:FREQ=SECONDLY");
        assert_eq!(starts(&e, "2026-01-01 00:00", "2027-01-01 00:00").len(), 1);
        // Two events, one broken.
        let text = "BEGIN:VEVENT\nDTSTART:2026\nEND:VEVENT\nBEGIN:VEVENT\nSUMMARY:ok\nDTSTART:20261020T090000Z\nEND:VEVENT";
        assert_eq!(parse(text).len(), 1);
    }

    /// A calendar is anyone's file: values past the end of time or splitting a char drop the event, never the session.
    #[test]
    fn hostile_values_drop_the_event() {
        // Past TimeDelta's range (i64::MAX ms) but within i64 seconds: dropped by `try_seconds`.
        let text = "BEGIN:VEVENT\nDTSTART:20261020T090000Z\nDURATION:PT2562047788015215H\nEND:VEVENT";
        assert!(parse(text).is_empty());
        // Past i64 seconds: the duration itself is refused, so the event has no length.
        let text = "BEGIN:VEVENT\nDTSTART:20261020T090000Z\nDURATION:P9223372036854775807S\nEND:VEVENT";
        for e in parse(text) {
            assert_eq!(e.between(0, i64::MAX / 2).len(), 1);
        }
        let text = "BEGIN:VEVENT\nDTSTART:20261020T090000Z\nRRULE:FREQ=WEEKLY;BYDAY=ÄM\nEND:VEVENT";
        for e in parse(text) {
            assert!(e.between(0, i64::MAX / 2).len() <= 1);
        }
    }

    #[test]
    fn durations() {
        assert_eq!(duration("PT15M"), Some(900));
        assert_eq!(duration("-PT15M"), Some(-900));
        assert_eq!(duration("-P1DT2H"), Some(-93_600));
        assert_eq!(duration("P2W"), Some(1_209_600));
        assert_eq!(duration("+PT0S"), Some(0));
        assert_eq!(duration("PT"), None);
        assert_eq!(duration("P1H"), None);
        assert_eq!(duration("15M"), None);
        assert_eq!(duration("PT99999999999999999999H"), None);
    }

    #[test]
    fn alarms_relative_and_absolute() {
        let e = one("DTSTART:20261020T090000Z\r\nDTEND:20261020T100000Z\r\nRRULE:FREQ=DAILY;COUNT=2\r\n\
             BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\n\
             BEGIN:VALARM\r\nTRIGGER;RELATED=END:PT0S\r\nEND:VALARM");
        let at: Vec<i64> = e.alarms(utc("2026-10-01 00:00"), utc("2026-11-01 00:00")).iter().map(|(t, _)| *t).collect();
        assert_eq!(at, [utc("2026-10-20 08:45"), utc("2026-10-20 10:00"), utc("2026-10-21 08:45"), utc("2026-10-21 10:00")]);
        // The window is inclusive and narrow windows still find alarms of occurrences outside them.
        let hit = e.alarms(utc("2026-10-21 08:45"), utc("2026-10-21 08:45"));
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].1.start, utc("2026-10-21 09:00"));
        let abs = one("DTSTART:20261020T090000Z\r\nBEGIN:VALARM\r\nTRIGGER;VALUE=DATE-TIME:20261019T120000Z\r\nEND:VALARM");
        assert_eq!(abs.alarms(utc("2026-10-19 00:00"), utc("2026-10-20 00:00"))[0].0, utc("2026-10-19 12:00"));
        // A TRIGGER outside a VALARM isn't an alarm.
        assert!(one("DTSTART:20261020T090000Z\r\nTRIGGER:-PT5M").alarms(0, i64::MAX / 2).is_empty());
        // A trigger at the end of time saturates instead of overflowing.
        let far = one("DTSTART:20261020T090000Z\r\nBEGIN:VALARM\r\nTRIGGER;VALUE=DATE-TIME:99991231T235959Z\r\nEND:VALARM");
        assert!(far.alarms(i64::MIN / 2, i64::MAX / 2).len() <= 1);
    }

    #[test]
    fn file_patterns() {
        let dir = std::env::temp_dir().join(format!("noobsbot-desktop-test-ics-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.ics", "b.ics", "notes.txt"] {
            std::fs::write(dir.join(name), "BEGIN:VEVENT\nDTSTART:20261020T090000Z\nEND:VEVENT\n").unwrap();
        }
        let glob = format!("{}/*.ics", dir.display());
        assert_eq!(files(&glob), [dir.join("a.ics"), dir.join("b.ics")]);
        assert_eq!(files(&dir.display().to_string()).len(), 2);
        assert_eq!(load(&[glob, dir.join("notes.txt").display().to_string(), "/nonexistent/x.ics".into()]).len(), 3);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
