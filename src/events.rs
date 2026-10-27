//! Calendar events (E7.2): the `.ics` files `[calendar] files` names, reread on a worker thread whenever one changes;
//! Notification Center's Up Next and the calendar's event dots; reminders (VALARMs) become our notifications, on one
//! timer set to the next alarm. CalDAV calendars (`caldav.rs`) are fetched by a second worker into cache files that
//! the first reads like any other, so they show offline too.
// ponytail: no Google calendars (OAuth); a Google calendar's CalDAV URL with an app password may work.

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    sync::mpsc::{self, RecvTimeoutError},
    time::Duration,
};

use chrono::{Local, NaiveDate, TimeZone, Weekday};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use smithay::reexports::calloop::{
    RegistrationToken, channel,
    timer::{TimeoutAction, Timer},
};

use crate::{
    config::CalDav,
    ical::{self, Event},
    services::notifications::Notification,
    state::State,
};

/// Reminder notification ids: this plus a counter.
const FIRST_ID: u32 = 0xFFFD_0000;
/// Up Next looks this far ahead.
const AHEAD: i64 = 7 * 86_400;
pub const UP_NEXT: usize = 3;
/// Alarms are looked for this far ahead; with none, the timer wakes at the end to look again.
const HORIZON: i64 = 60 * 86_400;
/// CalDAV calendars are fetched again this often.
const CALDAV_EVERY: Duration = Duration::from_secs(15 * 60);

/// One occurrence, as Up Next and reminders show it.
#[derive(Debug, Clone, PartialEq)]
pub struct Upcoming {
    pub summary: String,
    pub location: String,
    pub all_day: bool,
    /// Unix seconds.
    pub start: i64,
    pub end: i64,
}

impl Upcoming {
    fn new(o: &ical::Occurrence<'_>) -> Self {
        let e = o.event;
        let summary = if e.summary.is_empty() { "New Event".into() } else { e.summary.clone() };
        Upcoming { summary, location: e.location.lines().next().unwrap_or_default().to_owned(), all_day: e.all_day, start: o.start, end: o.end }
    }
}

/// (year, month, week start, minute) → (a dot per grid day, Up Next).
type View = ((i32, u32, Weekday, i64), Vec<bool>, Vec<Upcoming>);

#[derive(Default)]
pub struct Calendar {
    pub events: Vec<Event>,
    /// The entries being watched, the folders watched for them, and the watcher (dropping it ends its worker thread).
    watching: Option<(Vec<String>, Vec<PathBuf>, RecommendedWatcher)>,
    loaded: Option<channel::Sender<Vec<Event>>>,
    /// The CalDAV worker, from the first account on, and the accounts it was last sent.
    caldav: Option<(Vec<CalDav>, mpsc::Sender<Vec<CalDav>>)>,
    timer: Option<RegistrationToken>,
    /// Alarms up to here (unix seconds) have gone off.
    fired: i64,
    next_id: u32,
    view: RefCell<Option<View>>,
}

/// The next `max` occurrences still going on or starting within a week of `now`, soonest first.
pub fn up_next(events: &[Event], now: i64, max: usize) -> Vec<Upcoming> {
    let mut all: Vec<Upcoming> =
        events.iter().flat_map(|e| e.between(now, now + AHEAD)).filter(|o| o.end > now || o.start >= now).map(|o| Upcoming::new(&o)).collect();
    all.sort_by_key(|u| (u.start, !u.all_day));
    all.truncate(max);
    all
}

/// Which of `days` (contiguous, local dates) have an event.
pub fn marked(events: &[Event], days: &[NaiveDate]) -> Vec<bool> {
    let midnight = |d: NaiveDate| ical::Zone::Local.timestamp(d.and_time(chrono::NaiveTime::MIN));
    let (Some(first), Some(last)) = (days.first(), days.last().and_then(|d| d.succ_opt())) else { return Vec::new() };
    let mut out = vec![false; days.len()];
    let day_of = |t: i64| Local.timestamp_opt(t, 0).earliest().map(|d| d.date_naive());
    for o in events.iter().flat_map(|e| e.between(midnight(*first), midnight(last))) {
        // An event ending at midnight doesn't reach the next day.
        let (Some(from), Some(to)) = (day_of(o.start), day_of((o.end - 1).max(o.start))) else { continue };
        for (mark, day) in out.iter_mut().zip(days) {
            *mark |= (from..=to).contains(day);
        }
    }
    out
}

/// The first alarm after `after` within the horizon.
pub fn next_alarm(events: &[Event], after: i64) -> Option<i64> {
    events.iter().flat_map(|e| e.alarms(after + 1, after + HORIZON)).map(|(t, _)| t).min()
}

/// "All day", "9:30 AM – 10:00 AM", with "Tomorrow" or the weekday in front when it isn't today.
pub fn when(u: &Upcoming, now: i64, hour24: bool) -> String {
    let local = |t: i64| Local.timestamp_opt(t, 0).earliest();
    let (Some(start), Some(end), Some(now)) = (local(u.start), local(u.end), local(now)) else { return String::new() };
    let day = match (start.date_naive() - now.date_naive()).num_days() {
        ..=0 => String::new(),
        1 => "Tomorrow ".into(),
        _ => start.format("%A ").to_string(),
    };
    if u.all_day {
        return if day.is_empty() { "All day".into() } else { format!("{day}· All day") };
    }
    let clock = if hour24 { "%H:%M" } else { "%-I:%M %p" };
    match u.end > u.start {
        true => format!("{day}{} – {}", start.format(clock), end.format(clock)),
        false => format!("{day}{}", start.format(clock)),
    }
}

/// The directory to watch for `entry` (see `ical::files`).
fn watch_dir(entry: &str) -> PathBuf {
    let path = ical::expand(entry);
    if path.is_dir() { path } else { path.parent().map(PathBuf::from).unwrap_or_default() }
}

/// What watches `dir`: itself, else its nearest parent that exists, until it appears.
fn nearest(dir: &Path) -> Option<&Path> {
    dir.ancestors().find(|d| d.is_dir())
}

/// A change at `path` matters to the folders `dirs`: it's in one of them, or is one of them (or a parent) appearing
/// or going. A neighbour in a parent watched until a folder appears doesn't.
fn concerns(dirs: &[PathBuf], path: &Path) -> bool {
    dirs.iter().any(|d| path.parent() == Some(d.as_path()) || d.starts_with(path))
}

/// Where an account's last good fetch is kept.
fn caldav_cache(account: &CalDav) -> PathBuf {
    let name: String = format!("{}@{}", account.user, account.url).chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    crate::config::xdg_dir("XDG_CACHE_HOME", ".cache").join(format!("noobsbot-desktop/caldav/{name}.ics"))
}

/// Fetches every account into its cache file (a failed one keeps its last), and drops the files of removed ones.
fn caldav_sync(accounts: &[CalDav]) {
    let files: Vec<PathBuf> = accounts.iter().map(caldav_cache).collect();
    let dir = crate::config::xdg_dir("XDG_CACHE_HOME", ".cache").join("noobsbot-desktop/caldav");
    for old in std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()) {
        if old.extension().is_some_and(|e| e == "ics") && !files.contains(&old) {
            let _ = std::fs::remove_file(old);
        }
    }
    let now = Local::now().timestamp();
    for (account, file) in accounts.iter().zip(&files) {
        let (url, user) = (account.url.as_str(), account.user.as_str());
        let password = crate::caldav::secret_tool(&["lookup"], url, user, "").map_err(|err| format!("no password in the keyring: {err}"));
        let fetched = password.and_then(|p| crate::caldav::fetch(url, user, &p, now));
        if let Err(err) = fetched.and_then(|ics| crate::config::save_atomic(file, &ics).map_err(|e| e.to_string())) {
            tracing::warn!("calendar: CalDAV {url}: {err}");
        }
    }
}

/// The CalDAV worker: fetches now, every 15 minutes, and whenever it is sent the accounts (again, to fetch now).
fn caldav_spawn(accounts: Vec<CalDav>) -> Option<mpsc::Sender<Vec<CalDav>>> {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new().name("caldav".into()).spawn(move || {
        let mut accounts = accounts;
        loop {
            caldav_sync(&accounts);
            // No accounts: no wakeups until there are.
            let next = if accounts.is_empty() { rx.recv().map_err(|_| RecvTimeoutError::Disconnected) } else { rx.recv_timeout(CALDAV_EVERY) };
            match next {
                Ok(new) => accounts = new,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    });
    spawned.inspect_err(|err| tracing::warn!("calendar: no CalDAV: {err}")).ok().map(|_| tx)
}

/// `--show calendar-events`: a sample calendar around today, written to `dir`; returns its file.
pub fn sample(dir: &std::path::Path) -> std::io::Result<PathBuf> {
    let today = Local::now().date_naive();
    let day = |d: u64| (today + chrono::Days::new(d)).format("%Y%m%d").to_string();
    let ics = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
         BEGIN:VEVENT\r\nSUMMARY:Design review\r\nLOCATION:Studio 2\r\nDTSTART:{t}T235800\r\nDTEND:{t}T235900\r\nEND:VEVENT\r\n\
         BEGIN:VEVENT\r\nSUMMARY:Team stand-up\r\nLOCATION:Video call\r\nDTSTART:{t}T093000\r\nDTEND:{t}T094500\r\n\
         RRULE:FREQ=WEEKLY;BYDAY=MO,WE,FR\r\nBEGIN:VALARM\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\nEND:VEVENT\r\n\
         BEGIN:VEVENT\r\nSUMMARY:Dentist\r\nLOCATION:Lakeside Clinic\r\nDTSTART:{t1}T140000\r\nDTEND:{t1}T150000\r\nEND:VEVENT\r\n\
         BEGIN:VEVENT\r\nSUMMARY:Anniversary\r\nDTSTART;VALUE=DATE:{t9}\r\nRRULE:FREQ=YEARLY\r\nEND:VEVENT\r\n\
         END:VCALENDAR\r\n",
        t = day(0),
        t1 = day(1),
        t9 = day(9),
    );
    std::fs::create_dir_all(dir)?;
    let file = dir.join("sample.ics");
    std::fs::write(&file, ics)?;
    Ok(file)
}

impl State {
    /// (Re)starts watching `[calendar] files` and the CalDAV caches when they changed, or a folder they name appeared
    /// or went: a worker loads them now and after every change. A folder that isn't there yet is watched through its
    /// nearest parent. Changed CalDAV accounts go to their worker (started with the first).
    pub fn calendar_watch(&mut self) {
        let accounts = &self.config.calendar.caldav;
        match &mut self.calendar.caldav {
            Some((sent, tx)) if sent != accounts => {
                let _ = tx.send(accounts.clone());
                sent.clone_from(accounts);
            }
            None if !accounts.is_empty() => self.calendar.caldav = caldav_spawn(accounts.clone()).map(|tx| (accounts.clone(), tx)),
            _ => {}
        }
        let mut entries = self.config.calendar.files.clone();
        entries.extend(accounts.iter().map(|a| caldav_cache(a).display().to_string()));
        let dirs: Vec<PathBuf> = entries.iter().map(|e| watch_dir(e)).collect();
        let mut watched: Vec<PathBuf> = dirs.iter().filter_map(|d| nearest(d)).map(Path::to_owned).collect();
        watched.sort();
        watched.dedup();
        if self.calendar.watching.as_ref().is_some_and(|(e, w, _)| *e == entries && *w == watched) {
            return;
        }
        self.calendar.watching = None;
        if self.calendar.fired == 0 {
            self.calendar.fired = Local::now().timestamp();
        }
        let loaded = match self.calendar.loaded.clone() {
            Some(tx) => tx,
            None => {
                let (tx, rx) = channel::channel();
                let source = self.loop_handle.insert_source(rx, |event, _, state| {
                    if let channel::Event::Msg(events) = event {
                        state.calendar_loaded(events);
                    }
                });
                if let Err(err) = source {
                    return tracing::warn!("calendar: {err}");
                }
                self.calendar.loaded = Some(tx.clone());
                tx
            }
        };
        let (tx, rx) = mpsc::channel();
        let mut watcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(err) => return tracing::warn!("calendar: no live updates: {err}"),
        };
        for dir in &watched {
            if let Err(err) = watcher.watch(dir, RecursiveMode::NonRecursive) {
                tracing::warn!("calendar: {}: {err}", dir.display());
            }
        }
        let list = entries.clone();
        let spawned = std::thread::Builder::new().name("calendar".into()).spawn(move || {
            if loaded.send(ical::load(&list)).is_err() {
                return;
            }
            // Ends when the watcher (and its sender) is dropped.
            while let Ok(event) = rx.recv() {
                let changed = |e: &notify::Event| e.kind.is_create() || e.kind.is_modify() || e.kind.is_remove();
                if !event.is_ok_and(|e| changed(&e) && e.paths.iter().any(|p| concerns(&dirs, p))) {
                    continue;
                }
                while rx.recv_timeout(Duration::from_millis(300)).is_ok() {}
                if loaded.send(ical::load(&list)).is_err() {
                    return;
                }
            }
        });
        if let Err(err) = spawned {
            return tracing::warn!("calendar: {err}");
        }
        self.calendar.watching = Some((entries, watched, watcher));
    }

    /// Fetches the CalDAV calendars now (the network is back).
    pub fn caldav_now(&self) {
        if let Some((accounts, tx)) = &self.calendar.caldav {
            let _ = tx.send(accounts.clone());
        }
    }

    fn calendar_loaded(&mut self, events: Vec<Event>) {
        tracing::debug!(count = events.len(), "calendar events loaded");
        self.calendar.events = events;
        self.calendar.view.replace(None);
        self.calendar_arm();
        self.schedule_redraw();
        // A missing folder may have appeared (or a watched one gone): watch where it is now.
        self.calendar_watch();
    }

    /// One timer, to the next alarm (or the horizon, to look again). The timer is monotonic and stops while the machine
    /// sleeps, so waking arms it again.
    pub fn calendar_arm(&mut self) {
        if let Some(token) = self.calendar.timer.take() {
            self.loop_handle.remove(token);
        }
        if self.calendar.events.is_empty() {
            return;
        }
        let fired = self.calendar.fired;
        let at = next_alarm(&self.calendar.events, fired).unwrap_or(fired + HORIZON);
        let wait = Duration::from_secs((at - Local::now().timestamp()).max(0) as u64);
        let timer = self.loop_handle.insert_source(Timer::from_duration(wait), |_, _, state| {
            state.calendar.timer = None;
            state.calendar_alarm();
            TimeoutAction::Drop
        });
        match timer {
            Ok(token) => self.calendar.timer = Some(token),
            Err(err) => tracing::warn!("calendar reminders: {err}"),
        }
    }

    /// Every alarm due since the last one, as a notification.
    fn calendar_alarm(&mut self) {
        let now = Local::now().timestamp();
        let due: Vec<Upcoming> = self.calendar.events.iter().flat_map(|e| e.alarms(self.calendar.fired + 1, now)).map(|(_, o)| Upcoming::new(&o)).collect();
        self.calendar.fired = now;
        let hour24 = self.config.clock.format.hour24;
        for u in due {
            let place = if u.location.is_empty() { String::new() } else { format!(" · {}", u.location) };
            self.calendar.next_id = self.calendar.next_id.wrapping_add(1);
            self.notify(Notification {
                id: FIRST_ID + self.calendar.next_id % 0xFFFF,
                app: "Calendar".into(),
                icon_name: "x-office-calendar".into(),
                icon: crate::services::notifications::load_icon("x-office-calendar").map(std::sync::Arc::new),
                summary: u.summary.clone(),
                body: format!("{}{place}", when(&u, now, hour24)),
                urgency: 1,
                expire: -1,
                time: now,
                ..Default::default()
            });
        }
        self.calendar_arm();
    }

    /// A dot per day of the month grid, and Up Next, cached for the minute.
    pub fn calendar_view(&self, year: i32, month: u32, week_start: Weekday) -> (Vec<bool>, Vec<Upcoming>) {
        let now = Local::now().timestamp();
        let key = (year, month, week_start, now / 60);
        let mut view = self.calendar.view.borrow_mut();
        if view.as_ref().is_none_or(|(k, _, _)| *k != key) {
            let events = &self.calendar.events;
            let days: Vec<NaiveDate> = crate::shell::grid(year, month, week_start).into_iter().flatten().collect();
            *view = Some((key, marked(events, &days), up_next(events, now, UP_NEXT)));
        }
        view.as_ref().map(|(_, dots, next)| (dots.clone(), next.clone())).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
        Local.with_ymd_and_hms(y, m, d, h, min, 0).earliest().unwrap().timestamp()
    }

    fn events() -> Vec<Event> {
        ical::parse(
            "BEGIN:VEVENT\nSUMMARY:Lunch\nLOCATION:Cafe\\nupstairs\nDTSTART:20261020T120000\nDTEND:20261020T130000\nEND:VEVENT\n\
             BEGIN:VEVENT\nSUMMARY:Trip\nDTSTART;VALUE=DATE:20261022\nDTEND;VALUE=DATE:20261024\nEND:VEVENT\n\
             BEGIN:VEVENT\nDTSTART:20261019T090000\nDTEND:20261019T091500\nRRULE:FREQ=DAILY;COUNT=3\n\
             BEGIN:VALARM\nTRIGGER:-PT5M\nEND:VALARM\nEND:VEVENT\n\
             BEGIN:VEVENT\nSUMMARY:Late\nDTSTART:20261025T230000\nDTEND:20261026T000000\nEND:VEVENT\n",
        )
    }

    #[test]
    fn up_next_is_soonest_first() {
        let now = local(2026, 10, 20, 9, 10);
        let next = up_next(&events(), now, 3);
        let names: Vec<&str> = next.iter().map(|u| u.summary.as_str()).collect();
        // The 9:00 stand-in is still on; an untitled event gets a name; the location's first line only.
        assert_eq!(names, ["New Event", "Lunch", "New Event"]);
        assert_eq!(next[1].location, "Cafe");
        // A day later: the trip and the late one, within the week.
        assert_eq!(up_next(&events(), local(2026, 10, 21, 10, 0), 9).len(), 2);
        assert!(up_next(&events(), local(2027, 1, 1, 0, 0), 3).is_empty());
    }

    #[test]
    fn dots_cover_every_day_an_event_touches() {
        let days: Vec<NaiveDate> = (18..=26).map(|d| NaiveDate::from_ymd_opt(2026, 10, d).unwrap()).collect();
        let dots = marked(&events(), &days);
        // 18 nothing; 19–21 the daily; 20 lunch; 22–23 the trip (its DTEND is exclusive); 25 late, ending at midnight.
        assert_eq!(dots, [false, true, true, true, true, true, false, true, false]);
        assert!(marked(&events(), &[]).is_empty());
    }

    #[test]
    fn next_alarm_moves_on() {
        let e = events();
        let first = next_alarm(&e, local(2026, 10, 1, 0, 0));
        assert_eq!(first, Some(local(2026, 10, 19, 8, 55)));
        assert_eq!(next_alarm(&e, local(2026, 10, 19, 8, 55)), Some(local(2026, 10, 20, 8, 55)));
        assert_eq!(next_alarm(&e, local(2026, 10, 21, 8, 55)), None);
    }

    #[test]
    fn when_reads_naturally() {
        let now = local(2026, 10, 20, 8, 0);
        let u = |start: i64, end: i64, all_day: bool| Upcoming { summary: String::new(), location: String::new(), all_day, start, end };
        assert_eq!(when(&u(local(2026, 10, 20, 9, 30), local(2026, 10, 20, 10, 0), false), now, false), "9:30 AM – 10:00 AM");
        assert_eq!(when(&u(local(2026, 10, 21, 14, 0), local(2026, 10, 21, 14, 0), false), now, true), "Tomorrow 14:00");
        assert_eq!(when(&u(local(2026, 10, 23, 0, 0), local(2026, 10, 24, 0, 0), true), now, false), "Friday · All day");
        assert_eq!(when(&u(local(2026, 10, 20, 0, 0), local(2026, 10, 21, 0, 0), true), now, false), "All day");
    }

    #[test]
    fn a_missing_folder_is_watched_through_its_nearest_parent() {
        let dir = std::env::temp_dir().join(format!("noobsbot-desktop-test-calendar-parent-{}", std::process::id()));
        let deep = dir.join("a/b");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(nearest(&deep), Some(dir.as_path()));
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(nearest(&deep), Some(deep.as_path()));
        assert_eq!(nearest(Path::new("missing/relative")), None);
        // In the folder, or a step towards it appearing; not a neighbour in a watched parent.
        let dirs = [deep.clone()];
        assert!(concerns(&dirs, &deep.join("work.ics")));
        assert!(concerns(&dirs, &dir.join("a")));
        assert!(concerns(&dirs, &deep));
        assert!(!concerns(&dirs, &dir.join("notes.txt")));
        assert!(!concerns(&dirs, &deep.join("c/deeper.ics")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sample_parses() {
        let dir = std::env::temp_dir().join(format!("noobsbot-desktop-test-events-{}", std::process::id()));
        let file = sample(&dir).unwrap();
        let events = ical::load(&[file.display().to_string()]);
        assert_eq!(events.len(), 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
