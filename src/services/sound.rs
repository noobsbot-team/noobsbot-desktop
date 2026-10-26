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
    /// Every output, for the picker (E2.3).
    pub outputs: Vec<Output>,
    /// Apps playing sound, for their own sliders.
    pub streams: Vec<Stream>,
}

/// An app's playback stream.
#[derive(Clone, Debug, PartialEq)]
pub struct Stream {
    /// PipeWire's node id, for `set_stream_volume`.
    pub id: u32,
    /// `application.name`, else `node.name`.
    pub name: String,
    /// `application.icon-name`, when the app sets one.
    pub icon: Option<String>,
    pub volume: u8,
    pub muted: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    /// PipeWire's object id, for `set_output`.
    pub id: u32,
    pub name: String,
    /// The default output, which `Sound::output` describes.
    pub current: bool,
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
    let status = wpctl(&["status"]).unwrap_or_default();
    let outputs = parse_outputs(&status);
    let mut output = device(OUTPUT)?;
    // wpctl reads one channel; with the balance off centre the louder one is the volume.
    if let Some((loudest, _)) = crate::pactl::read().filter(|(_, balance)| *balance != 0.0) {
        output.volume = loudest;
    }
    // ponytail: two wpctl runs per stream per graph change; one `pw-dump` read if many streams make that slow.
    let streams = parse_streams(&status).into_iter().filter_map(stream).collect();
    Some(Sound { output, input: device(INPUT), outputs, streams })
}

/// The ids in the audio `Streams:` section: `        89. Firefox`, its ports indented further under it.
fn parse_streams(status: &str) -> Vec<u32> {
    let audio = status.split("\nVideo\n").next().unwrap_or_default();
    let mut lines = audio.lines().skip_while(|line| !line.ends_with("Streams:")).skip(1).filter(|l| !l.trim().is_empty()).peekable();
    let indent = |line: &str| line.len() - line.trim_start().len();
    let Some(top) = lines.peek().map(|l| indent(l)) else { return Vec::new() };
    lines.filter(|l| indent(l) == top).filter_map(|l| l.trim_start().split_once(". ")?.0.parse().ok()).collect()
}

/// An app's playback stream by node id; `None` for a recording one.
fn stream(id: u32) -> Option<Stream> {
    let id = id.to_string();
    let inspect = wpctl(&["inspect", &id])?;
    let prop = |key| prop(&inspect, key);
    (prop("media.class")?.as_str() == "Stream/Output/Audio").then_some(())?;
    let (volume, muted) = parse_volume(&wpctl(&["get-volume", &id])?)?;
    Some(Stream { id: id.parse().ok()?, name: prop("application.name").or_else(|| prop("node.name"))?, icon: prop("application.icon-name"), volume, muted })
}

/// The audio `Sinks:` section of `wpctl status`: `│  *   63. Raptor Lake-P/U/H cAVS Speaker      [vol: 0.89]`.
fn parse_outputs(status: &str) -> Vec<Output> {
    let lines = status.lines().skip_while(|line| !line.ends_with("Sinks:")).skip(1);
    // The section ends at its blank `│` line.
    lines
        .map_while(|line| {
            let line = line.trim_start_matches([' ', '│']);
            let (id, rest) = line.trim_start_matches(['*', ' ']).split_once(". ")?;
            let name = rest.rsplit_once(" [vol:").map_or(rest, |(name, _)| name).trim();
            Some(Output { id: id.parse().ok()?, name: name.to_owned(), current: line.starts_with('*') })
        })
        .collect()
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

/// `key = "value"` from `wpctl inspect`.
fn prop(inspect: &str, key: &str) -> Option<String> {
    inspect.lines().find_map(|line| {
        let rest = line.trim_start_matches([' ', '*']).strip_prefix(key)?.trim_start().strip_prefix('=')?;
        Some(rest.trim().trim_matches('"').to_owned())
    })
}

/// `node.nick` (short, like `Speaker`) if set, else `node.description`.
fn parse_name(inspect: &str) -> String {
    prop(inspect, "node.nick").or_else(|| prop(inspect, "node.description")).unwrap_or_default()
}

/// Volume and mute go through `wpctl`; the change comes back through the monitor like any other.
pub fn set_volume(output: bool, percent: u8) {
    // wpctl would centre the balance; pactl keeps it.
    if output && crate::pactl::read().is_some_and(|(_, balance)| balance != 0.0 && crate::pactl::set(percent.min(100), balance)) {
        return;
    }
    let target = if output { OUTPUT } else { INPUT };
    let _ = wpctl(&["set-volume", target, &format!("{}%", percent.min(100))]);
}

/// An app's stream volume, by node id.
pub fn set_stream_volume(id: u32, percent: u8) {
    let _ = wpctl(&["set-volume", &id.to_string(), &format!("{}%", percent.min(100))]);
}

/// Makes output `id` the default; the change comes back through the monitor.
pub fn set_output(id: u32) {
    let _ = wpctl(&["set-default", &id.to_string()]);
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
    fn outputs() {
        let status = "Audio\n ├─ Sinks:\n │      60. HDMI / DisplayPort 3 Output (Stereo) [vol: 1.00]\n │  *   63. Raptor Lake Speaker      [vol: 0.89]\n │      70. AirPods\n │  \n ├─ Sources:\n │  *   65. Digital Microphone [vol: 1.00]\n";
        let names: Vec<_> = parse_outputs(status).into_iter().map(|o| (o.id, o.name, o.current)).collect();
        assert_eq!(
            names,
            [(60, "HDMI / DisplayPort 3 Output (Stereo)".into(), false), (63, "Raptor Lake Speaker".into(), true), (70, "AirPods".into(), false)]
        );
        assert_eq!(parse_outputs(""), []);
    }

    #[test]
    fn streams() {
        let status = "Audio\n ├─ Sinks:\n │  *   63. Speaker      [vol: 0.96]\n │  \n └─ Streams:\n        89. pw-play                                                     \n             90. output_FL       > Speaker:playback_FL\t[active]\n        95. Firefox\n             96. output_FR       > Speaker:playback_FR\t[active]\n\nVideo\n └─ Streams:\n        120. Camera\n";
        assert_eq!(parse_streams(status), [89, 95]);
        assert_eq!(parse_streams("Audio\n └─ Streams:\n\nVideo\n └─ Streams:\n        120. Camera\n"), Vec::<u32>::new());
        let inspect = "id 89, type PipeWire:Interface:Node\n  * application.name = \"pw-play\"\n  * media.class = \"Stream/Output/Audio\"\n";
        assert_eq!(prop(inspect, "media.class").as_deref(), Some("Stream/Output/Audio"));
        assert_eq!(prop(inspect, "application.icon-name"), None);
    }

    #[test]
    fn name() {
        let inspect = "id 52, type PipeWire:Interface:Node\n  * node.description = \"Raptor Lake-P/U/H cAVS Speaker\"\n  * node.nick = \"Speaker\"\n";
        assert_eq!(parse_name(inspect), "Speaker");
        assert_eq!(parse_name("    node.description = \"USB Headset\"\n"), "USB Headset");
        assert_eq!(parse_name(""), "");
    }
}
