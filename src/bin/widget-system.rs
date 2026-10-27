//! `noobsbot-desktop-widget-system` (E8.8): the "System" widget, CPU, memory and disk use and network speed from
//! `/proc` and statvfs, set on `org.noobsbot.Desktop.Widgets` every 5 s. The shell starts it at login; it waits up to
//! 30 s for the shell's bus name and exits once a `Set` fails after that (the shell went away, or another copy owns
//! the id).
// ponytail: 5 s whether or not anyone sees it: the widget protocol doesn't say when a widget is shown (and it may sit
// on the desktop); add a shown/hidden signal to it if this ever costs.

use std::{
    collections::HashMap,
    ffi::CString,
    fs,
    process::ExitCode,
    thread::sleep,
    time::{Duration, Instant},
};

use zbus::{blocking::Connection, zvariant::Value};

const EVERY: Duration = Duration::from_secs(5);
/// Tries before the shell's first answer.
const WAIT: u32 = 6;

/// (busy, total) jiffies from the `cpu` line of `/proc/stat`; idle and iowait are the idle part.
fn cpu_times(stat: &str) -> Option<(u64, u64)> {
    let fields: Vec<u64> = stat.lines().next()?.strip_prefix("cpu ")?.split_whitespace().map(|f| f.parse().ok()).collect::<Option<_>>()?;
    // guest and guest_nice are already counted in user and nice.
    let total: u64 = fields.iter().take(8).sum();
    let idle = fields.get(3)? + fields.get(4).unwrap_or(&0);
    Some((total - idle, total))
}

/// Percent busy between two `cpu_times` samples.
fn cpu_percent((busy0, total0): (u64, u64), (busy1, total1): (u64, u64)) -> u64 {
    (busy1.saturating_sub(busy0) * 100).checked_div(total1.saturating_sub(total0)).unwrap_or(0)
}

/// (used, total) bytes from `/proc/meminfo`: used is MemTotal − MemAvailable.
fn memory(meminfo: &str) -> Option<(u64, u64)> {
    let kb = |key: &str| meminfo.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix(':')?.trim().strip_suffix("kB")?.trim().parse::<u64>().ok());
    let (total, available) = (kb("MemTotal")?, kb("MemAvailable")?);
    Some((total.saturating_sub(available) * 1024, total * 1024))
}

/// (used, total) bytes of the file system holding `path`.
fn disk(path: &str) -> Option<(u64, u64)> {
    let path = CString::new(path).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a valid C string and a statvfs to fill.
    if unsafe { libc::statvfs(path.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let block = s.f_frsize as u64;
    Some(((s.f_blocks - s.f_bfree) as u64 * block, s.f_blocks as u64 * block))
}

/// (received, sent) bytes from `/proc/net/dev`, summed over every interface but loopback.
fn net_bytes(dev: &str) -> (u64, u64) {
    dev.lines().filter_map(|l| l.split_once(':')).filter(|(name, _)| name.trim() != "lo").fold((0, 0), |(rx, tx), (_, counters)| {
        let f: Vec<u64> = counters.split_whitespace().filter_map(|f| f.parse().ok()).collect();
        (rx + f.first().unwrap_or(&0), tx + f.get(8).unwrap_or(&0))
    })
}

/// Bytes per second from `before` to `now`, `secs` apart; 0 after a counter went back (an interface went away).
fn rate(before: u64, now: u64, secs: f64) -> f64 {
    if secs > 0.0 { now.saturating_sub(before) as f64 / secs } else { 0.0 }
}

fn speed(bytes_per_s: f64) -> String {
    if bytes_per_s >= 1e6 { format!("{:.1} MB/s", bytes_per_s / 1e6) } else { format!("{:.0} KB/s", bytes_per_s / 1e3) }
}

fn gb((used, total): (u64, u64)) -> String {
    let g = |b: u64| b as f64 / 1e9;
    format!("{:.1} of {:.0} GB", g(used), g(total))
}

/// `net`: (down, up) bytes per second.
fn lines(cpu: u64, net: Option<(f64, f64)>) -> Vec<String> {
    let mut lines = vec![format!("CPU {cpu}%")];
    lines.extend(fs::read_to_string("/proc/meminfo").ok().as_deref().and_then(memory).map(|m| format!("Memory {}", gb(m))));
    lines.extend(disk("/").map(|d| format!("Disk {}", gb(d))));
    lines.extend(net.map(|(down, up)| format!("Network ↓ {} ↑ {}", speed(down), speed(up))));
    lines
}

fn stat() -> Option<(u64, u64)> {
    cpu_times(&fs::read_to_string("/proc/stat").ok()?)
}

fn net() -> Option<(u64, u64)> {
    Some(net_bytes(&fs::read_to_string("/proc/net/dev").ok()?))
}

fn main() -> ExitCode {
    let bus = match Connection::session() {
        Ok(bus) => bus,
        Err(err) => {
            eprintln!("noobsbot-desktop-widget-system: {err}");
            return ExitCode::FAILURE;
        }
    };
    let (mut before, mut tries, mut answered) = (stat(), 0, false);
    let (mut net_before, mut at) = (net(), Instant::now());
    loop {
        sleep(EVERY);
        let now = stat();
        let cpu = before.zip(now).map_or(0, |(a, b)| cpu_percent(a, b));
        before = now;
        let (net_now, secs) = (net(), at.elapsed().as_secs_f64());
        let speeds = net_before.zip(net_now).map(|((rx0, tx0), (rx1, tx1))| (rate(rx0, rx1, secs), rate(tx0, tx1, secs)));
        (net_before, at) = (net_now, Instant::now());
        let content: HashMap<&str, Value<'_>> =
            HashMap::from([("title", "System".into()), ("icon", "cpu".into()), ("size", "medium".into()), ("lines", lines(cpu, speeds).into())]);
        let set = bus.call_method(Some("org.noobsbot.Desktop"), "/org/noobsbot/Desktop", Some("org.noobsbot.Desktop.Widgets"), "Set", &("system", content));
        match set {
            Ok(_) => answered = true,
            Err(_) if !answered && tries < WAIT => tries += 1,
            Err(err) => {
                // After the first answer, a failure means the shell is gone.
                eprintln!("noobsbot-desktop-widget-system: {err}");
                return ExitCode::from(u8::from(!answered));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_from_proc_stat() {
        let a = cpu_times("cpu  100 0 100 700 100 0 0 0 0 0\ncpu0 1 2 3 4\n").unwrap();
        let b = cpu_times("cpu  150 0 150 800 100 0 0 0 0 0\n").unwrap();
        assert_eq!((a, b), ((200, 1000), (300, 1200)));
        assert_eq!(cpu_percent(a, b), 50);
        assert_eq!(cpu_percent(a, a), 0, "no time passed");
        assert_eq!(cpu_times("intr 1 2"), None);
    }

    #[test]
    fn memory_from_meminfo() {
        let info = "MemTotal:       16000000 kB\nMemFree:  1000 kB\nMemAvailable:    4000000 kB\n";
        assert_eq!(memory(info), Some((12_000_000 * 1024, 16_000_000 * 1024)));
        assert_eq!(memory("MemTotal: 5 kB\n"), None);
    }

    #[test]
    fn network_from_proc_net_dev() {
        let dev = "Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 999999      10    0    0    0     0          0         0   999999      10    0    0    0     0       0          0
  eth0: 1000      10    0    0    0     0          0         0     200       2    0    0    0     0       0          0
wlan0:500 5 0 0 0 0 0 0 300 3 0 0 0 0 0 0
";
        assert_eq!(net_bytes(dev), (1500, 500), "loopback left out");
        assert_eq!(net_bytes(""), (0, 0));
        assert_eq!(rate(1000, 6000, 5.0), 1000.0);
        assert_eq!(rate(6000, 1000, 5.0), 0.0, "a counter that went back");
        assert_eq!(rate(0, 10, 0.0), 0.0);
        assert_eq!((speed(1000.0).as_str(), speed(2_500_000.0).as_str()), ("1 KB/s", "2.5 MB/s"));
    }
}
