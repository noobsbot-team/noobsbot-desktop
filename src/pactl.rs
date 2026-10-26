//! The default output's left/right balance through `pactl` (PipeWire's pulse server), since `wpctl` sets every
//! channel alike. WirePlumber keeps it per device. Shared with the Settings app.

use std::process::{Command, Stdio};

const SINK: &str = "@DEFAULT_SINK@";

fn pactl(args: &[&str]) -> Option<String> {
    let out = Command::new("pactl").args(args).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The loudest channel in percent and the balance (-1 left … 1 right); `None` without `pactl`.
pub fn read() -> Option<(u8, f32)> {
    parse(&pactl(&["get-sink-volume", SINK])?)
}

/// The output at `percent` with `balance`; `false` when `pactl` couldn't (a mono output, no pulse server).
pub fn set(percent: u8, balance: f32) -> bool {
    let (left, right) = channels(percent, balance);
    pactl(&["set-sink-volume", SINK, &format!("{left}%"), &format!("{right}%")]).is_some()
}

/// The pulse server's rule: the side the balance leans to keeps `percent`, the other drops by the balance.
pub fn channels(percent: u8, balance: f32) -> (u8, u8) {
    let balance = balance.clamp(-1.0, 1.0);
    let of = |share: f32| (f32::from(percent) * share).round() as u8;
    (of(1.0 - balance.max(0.0)), of(1.0 + balance.min(0.0)))
}

/// `Volume: front-left: 52428 /  80% / -5.81 dB,   front-right: 65536 / 100% / 0.00 dB` then `balance 0.20`.
fn parse(text: &str) -> Option<(u8, f32)> {
    let loudest = text.split('/').filter_map(|part| part.trim().strip_suffix('%')?.parse::<u8>().ok()).max()?;
    let balance = text.split_whitespace().skip_while(|w| *w != "balance").nth(1)?.parse().ok()?;
    Some((loudest, balance))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance() {
        assert_eq!(channels(80, 0.0), (80, 80));
        assert_eq!(channels(80, 0.25), (60, 80));
        assert_eq!(channels(80, -0.5), (80, 40));
        assert_eq!(channels(100, 1.0), (0, 100));
        assert_eq!(channels(100, -3.0), (100, 0));
        let text = "Volume: front-left: 52428 /  80% / -5.81 dB,   front-right: 65536 / 100% / 0.00 dB\n        balance 0.20\n";
        assert_eq!(parse(text), Some((100, 0.2)));
        assert_eq!(parse("Volume: mono: 65536 / 100% / 0.00 dB\n        balance 0.00\n"), Some((100, 0.0)));
        assert_eq!(parse("Connection failure"), None);
    }
}
