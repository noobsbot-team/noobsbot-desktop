//! Sound (E2.3, E2.7) through PipeWire's own tools, the plan's M2 path until the `pipewire` crate replaces it:
//! `pw-dump --monitor` prints a JSON chunk on every graph change (nothing while idle), and after each chunk `wpctl`
//! reads the default output and input. No polling: a plugged-in headset shows up through the next chunk.

use std::{
    io::{BufRead, BufReader},
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    time::Duration,
};

use smithay::reexports::calloop::channel::Sender;

use super::Update;

const OUTPUT: &str = "@DEFAULT_AUDIO_SINK@";
const INPUT: &str = "@DEFAULT_AUDIO_SOURCE@";

#[derive(Clone, Debug, PartialEq)]
pub struct Sound {
    pub output: Device,
    /// `None`: no microphone.
    pub input: Option<Device>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    /// The short name (`Speaker`), else the full description.
    pub name: String,
    /// Percent; PipeWire allows up to 150.
    pub volume: u8,
    pub muted: bool,
}

/// Runs on its own thread for the life of the desktop; restarts the monitor when PipeWire restarts.
pub fn spawn(updates: Sender<Update>) {
    let thread = std::thread::Builder::new().name("sound".into()).spawn(move || {
        let mut backoff = Duration::from_secs(1);
        loop {
            match monitor(&updates) {
                Ok(true) => backoff = Duration::from_secs(1),
                Ok(false) => return,
                Err(err) => tracing::warn!("sound status off: {err}"),
            }
            let _ = updates.send(Update::Sound(None));
            // Doubling to a minute: a machine without PipeWire costs one wakeup a minute, not a busy loop.
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
    });
    if let Err(err) = thread {
        tracing::warn!("sound status off: {err}");
    }
}

/// Follows one `pw-dump` run. `Ok(true)`: it ended (PipeWire went away), try again; `Ok(false)`: the desktop is
/// shutting down.
fn monitor(updates: &Sender<Update>) -> std::io::Result<bool> {
    let mut command = Command::new("pw-dump");
    command.args(["--monitor", "--no-colors"]).stdout(Stdio::piped()).stderr(Stdio::null());
    // SAFETY: prctl is async-signal-safe. An idle pw-dump writes nothing, so without this it would outlive a crashed
    // desktop until the next graph change; the death signal ends it with this thread.
    unsafe {
        command.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().expect("piped");
    let mut last = None;
    let mut chunk = Chunk::default();
    for line in BufReader::new(stdout).lines() {
        if !chunk.line(&line?) {
            continue;
        }
        let sound = read();
        if sound != last {
            tracing::debug!(?sound, "sound");
            if updates.send(Update::Sound(sound.clone())).is_err() {
                let _ = child.kill();
                return Ok(false);
            }
            last = sound;
        }
    }
    let _ = child.wait();
    Ok(true)
}

/// Which `pw-dump` chunks matter. Every `wpctl` run is a PipeWire client whose arrival and departure is itself a
/// chunk; reacting to those would loop forever. Only nodes (volume, mute), devices (plugging) and metadata (the
/// default output) count, including their removal, which pw-dump prints as `"info": null` under a known id.
#[derive(Default)]
struct Chunk {
    watched: std::collections::HashSet<u32>,
    id: Option<u32>,
    relevant: bool,
}

impl Chunk {
    /// Feeds one line; `true` when it closed a chunk that touched a watched object.
    fn line(&mut self, line: &str) -> bool {
        // Objects sit at two spaces of indentation, their top-level keys at four.
        if let Some(id) = line.strip_prefix("    \"id\": ") {
            self.id = id.trim_end_matches(',').parse().ok();
        } else if let Some(kind) = line.strip_prefix("    \"type\": \"PipeWire:Interface:") {
            if ["Node\"", "Device\"", "Metadata\""].iter().any(|k| kind.starts_with(k)) {
                self.relevant = true;
                self.watched.extend(self.id);
            }
        } else if line.starts_with("    \"info\": null") {
            self.relevant |= self.id.is_some_and(|id| self.watched.remove(&id));
        } else if line == "]" {
            return std::mem::take(&mut self.relevant);
        }
        false
    }
}

fn wpctl(args: &[&str]) -> Option<String> {
    let out = Command::new("wpctl").args(args).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `None` when there is no output device.
fn read() -> Option<Sound> {
    Some(Sound { output: device(OUTPUT)?, input: device(INPUT) })
}

fn device(target: &str) -> Option<Device> {
    let (volume, muted) = parse_volume(&wpctl(&["get-volume", target])?)?;
    let name = parse_name(&wpctl(&["inspect", target]).unwrap_or_default());
    Some(Device { name, volume, muted })
}

/// `Volume: 0.89` or `Volume: 0.89 [MUTED]`.
fn parse_volume(text: &str) -> Option<(u8, bool)> {
    let mut words = text.strip_prefix("Volume:")?.split_whitespace();
    let volume: f32 = words.next()?.parse().ok()?;
    Some(((volume * 100.0).round().clamp(0.0, 150.0) as u8, words.any(|w| w == "[MUTED]")))
}

/// `node.nick` (short, like `Speaker`) if set, else `node.description`.
fn parse_name(inspect: &str) -> String {
    let value = |key: &str| {
        inspect.lines().find_map(|line| {
            let rest = line.trim_start_matches([' ', '*']).strip_prefix(key)?.trim_start().strip_prefix('=')?;
            Some(rest.trim().trim_matches('"').to_owned())
        })
    };
    value("node.nick").or_else(|| value("node.description")).unwrap_or_default()
}

/// Volume and mute go through `wpctl`; the change comes back through the monitor like any other.
pub fn set_volume(output: bool, percent: u8) {
    let target = if output { OUTPUT } else { INPUT };
    let _ = wpctl(&["set-volume", target, &format!("{}%", percent.min(100))]);
}

pub fn set_muted(output: bool, muted: bool) {
    let target = if output { OUTPUT } else { INPUT };
    let _ = wpctl(&["set-mute", target, if muted { "1" } else { "0" }]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume() {
        assert_eq!(parse_volume("Volume: 0.89\n"), Some((89, false)));
        assert_eq!(parse_volume("Volume: 0.40 [MUTED]\n"), Some((40, true)));
        assert_eq!(parse_volume("Volume: 1.50"), Some((150, false)));
        assert_eq!(parse_volume("nonsense"), None);
    }

    #[test]
    fn only_audio_objects_count() {
        let feed = |chunk: &mut Chunk, text: &str| text.lines().filter(|l| chunk.line(l)).count() > 0;
        let mut chunk = Chunk::default();
        let node = "[\n  {\n    \"id\": 52,\n    \"type\": \"PipeWire:Interface:Node\",\n    \"info\": {\n    }\n  }\n]\n";
        let client = "[\n  {\n    \"id\": 90,\n    \"type\": \"PipeWire:Interface:Client\",\n    \"info\": {\n    }\n  }\n]\n";
        assert!(feed(&mut chunk, node));
        // A wpctl run coming and going: ignored.
        assert!(!feed(&mut chunk, client));
        assert!(!feed(&mut chunk, "[\n  {\n    \"id\": 90,\n    \"info\": null\n  }\n]\n"));
        // The node going away (unplugged): counts, once.
        assert!(feed(&mut chunk, "[\n  {\n    \"id\": 52,\n    \"info\": null\n  }\n]\n"));
        assert!(!feed(&mut chunk, "[\n  {\n    \"id\": 52,\n    \"info\": null\n  }\n]\n"));
    }

    #[test]
    fn name() {
        let inspect = "id 52, type PipeWire:Interface:Node\n  * node.description = \"Raptor Lake-P/U/H cAVS Speaker\"\n  * node.nick = \"Speaker\"\n";
        assert_eq!(parse_name(inspect), "Speaker");
        assert_eq!(parse_name("    node.description = \"USB Headset\"\n"), "USB Headset");
        assert_eq!(parse_name(""), "");
    }
}
