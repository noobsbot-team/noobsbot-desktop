//! Printers: CUPS through its own tools (`lpstat`, `lpoptions`, `lpadmin`, `lpinfo`, `cancel`, `lp`).
// ponytail: shells out (LC_ALL=C, parsing their English) instead of speaking IPP to cupsd; IPP if a tool's wording
// changes or the page needs more than names, states and jobs.

use std::process::Command;

/// Add Printer ▸ Address: the `texts` key of the typed URI.
pub const URI: &str = "printers.uri";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Printing,
    Disabled,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Idle => "Idle",
            State::Printing => "Printing",
            State::Disabled => "Stopped",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Printer {
    pub name: String,
    pub state: State,
    pub default: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    /// `Office-12`: what `cancel` takes.
    pub id: String,
    pub printer: String,
    pub user: String,
    pub size: u64,
    pub when: String,
}

#[derive(Clone, Debug, Default)]
pub struct Printers {
    pub printers: Vec<Printer>,
    pub jobs: Vec<Job>,
}

/// `lpstat -p -d`: each printer's state, and the default (`system default destination: …`).
pub fn parse_printers(text: &str) -> Vec<Printer> {
    let default = text.lines().find_map(|l| l.strip_prefix("system default destination: ")).map(str::trim);
    text.lines()
        .filter_map(|l| l.strip_prefix("printer "))
        .filter_map(|l| {
            let (name, rest) = l.split_once(' ')?;
            let state = if rest.starts_with("disabled") {
                State::Disabled
            } else if rest.starts_with("now printing") {
                State::Printing
            } else {
                State::Idle
            };
            Some(Printer { name: name.to_owned(), state, default: default == Some(name) })
        })
        .collect()
}

/// `lpstat -o`: `Office-12  anil  1024  Mon 27 Oct 2026 10:00:00`.
pub fn parse_jobs(text: &str) -> Vec<Job> {
    text.lines()
        .filter_map(|l| {
            let mut words = l.split_whitespace();
            let id = words.next()?;
            let (printer, number) = id.rsplit_once('-')?;
            number.parse::<u32>().ok()?;
            let user = words.next()?.to_owned();
            let size = words.next()?.parse().ok()?;
            Some(Job { id: id.to_owned(), printer: printer.to_owned(), user, size, when: words.collect::<Vec<_>>().join(" ") })
        })
        .collect()
}

/// `lpinfo -v`: the devices found (`network ipp` and the like, bare schemes, are skipped).
pub fn parse_devices(text: &str) -> Vec<String> {
    text.lines().filter_map(|l| l.split_once(' ')).map(|(_, uri)| uri.trim()).filter(|uri| uri.contains("://")).map(String::from).collect()
}

/// A queue name for a device: the DNS-SD service, the USB model or the host, in letters CUPS takes.
pub fn suggest_name(uri: &str) -> String {
    let (scheme, rest) = uri.split_once("://").unwrap_or(("", uri));
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let raw = match scheme {
        "dnssd" => rest.split("._").next().unwrap_or(rest),
        "usb" => rest.trim_end_matches('/').rsplit('/').next().unwrap_or(rest),
        _ => rest.split(['/', ':']).next().unwrap_or(rest),
    };
    let name: String = decode(raw).chars().map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '_' }).collect();
    if name.is_empty() { "Printer".into() } else { name }
}

/// `%20` and friends.
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], text.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Runs a CUPS tool: its output, or what it said went wrong.
fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program).args(args).env("LC_ALL", "C").output().map_err(|err| format!("Couldn't run {program}: {err}"))?;
    let said = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    match out.status.success() {
        true => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        false if said.is_empty() => Err(format!("{program} failed")),
        false => Err(said),
    }
}

/// `lpadmin`, again through pkexec when cupsd turns the user away (not in its admin group).
fn admin(args: &[&str]) -> Result<String, String> {
    run("lpadmin", args).or_else(|said| {
        let refused = ["Forbidden", "Unauthorized", "not authorized", "Permission denied"].iter().any(|w| said.contains(w));
        if refused { run("pkexec", &[["lpadmin"].as_slice(), args].concat()) } else { Err(said) }
    })
}

/// The printers and every job; `None` without CUPS.
pub async fn read() -> Option<Printers> {
    if !crate::ibus::on_path("lpstat") {
        return None;
    }
    // Both fail with no printers ("No destinations added"): nothing to list.
    let printers = run("lpstat", &["-p", "-d"]).map(|t| parse_printers(&t)).unwrap_or_default();
    let jobs = run("lpstat", &["-o"]).map(|t| parse_jobs(&t)).unwrap_or_default();
    Some(Printers { printers, jobs })
}

#[derive(Clone, Debug)]
pub enum Act {
    Default(String),
    TestPage(String),
    Remove(String),
    Cancel(String),
    /// A queue (name, device URI), driverless.
    Add(String, String),
}

/// Does `act`, then the printers again and what went wrong.
pub async fn act(act: Act) -> (Option<Printers>, Option<String>) {
    let done = match &act {
        Act::Default(name) => run("lpoptions", &["-d", name]),
        Act::TestPage(name) => run("lp", &["-d", name, "-t", "Test Page", "/usr/share/cups/data/default-testpage.pdf"]),
        Act::Remove(name) => admin(&["-x", name]),
        Act::Cancel(job) => run("cancel", &[job]),
        // ponytail: IPP Everywhere only (`-m everywhere`); printers that need a PPD or vendor driver are left to
        // the CUPS web page.
        Act::Add(name, uri) => admin(&["-p", name, "-E", "-v", uri, "-m", "everywhere"]),
    };
    (read().await, done.err())
}

/// Add Printer…: the devices `lpinfo -v` finds (it looks around the network for a while).
pub async fn devices() -> Result<Vec<String>, String> {
    run("lpinfo", &["-v"]).map(|t| parse_devices(&t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lpstat_printers() {
        let text = "printer Deli-D511W is idle.  enabled since Mon Oct 12 18:34:06 2026\n\
                    printer Office now printing Office-12.  enabled since Mon Oct 12 11:31:19 2026\n\
                    printer Old disabled since Mon Oct 12 11:31:19 2026 -\n\treason unknown\n\
                    system default destination: Office\n";
        let printers = parse_printers(text);
        let got: Vec<_> = printers.iter().map(|p| (p.name.as_str(), p.state, p.default)).collect();
        assert_eq!(got, [("Deli-D511W", State::Idle, false), ("Office", State::Printing, true), ("Old", State::Disabled, false)]);
        assert!(parse_printers("no system default destination\n").is_empty());
    }

    #[test]
    fn lpstat_jobs() {
        let text = "Deli-D511W-12          anil            123904   Mon 27 Oct 2026 10:00:00 AM CET\nnonsense\n";
        let jobs = parse_jobs(text);
        assert_eq!(jobs.len(), 1);
        assert_eq!((jobs[0].id.as_str(), jobs[0].printer.as_str(), jobs[0].user.as_str(), jobs[0].size), ("Deli-D511W-12", "Deli-D511W", "anil", 123904));
        assert_eq!(jobs[0].when, "Mon 27 Oct 2026 10:00:00 AM CET");
    }

    #[test]
    fn lpinfo_devices() {
        let text = "network ipp\nnetwork socket\ndirect usb://Kyocera/MA2000w?serial=W714902146\n\
                    network dnssd://Deli%20D511W._ipp._tcp.local/?uuid=e3248000\nnetwork ipp://192.168.1.146:631/ipp/print\n";
        let devices = parse_devices(text);
        assert_eq!(devices.len(), 3);
        let names: Vec<_> = devices.iter().map(|d| suggest_name(d)).collect();
        assert_eq!(names, ["MA2000w", "Deli_D511W", "192.168.1.146"]);
    }
}
