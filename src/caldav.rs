//! CalDAV calendars (`[calendar] caldav`): one `REPORT` for the events from a month back to a year ahead, through
//! `curl`, with the password from the keyring through `secret-tool` (libsecret). Shared with the Settings app (`#[path]`),
//! which checks an account with the same fetch and stores its password.
// ponytail: no discovery (the calendar's own collection URL is typed) and no sync tokens (the whole window each time).

use std::{
    io::Write,
    process::{Command, Stdio},
};

/// A month back.
const BACK: i64 = 30 * 86_400;
/// A year ahead.
const AHEAD: i64 = 365 * 86_400;

/// Runs `program` with `input` on its stdin (secrets go there, never on the command line): its output, else what it
/// said went wrong.
fn run(program: &str, args: &[&str], input: &str) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("Couldn't run {program}: {err}"))?;
    // A line or two, so the pipe takes it before the program reads (or exits without reading).
    let _ = child.stdin.take().map(|mut stdin| stdin.write_all(input.as_bytes()));
    let out = child.wait_with_output().map_err(|err| format!("{program}: {err}"))?;
    let said = String::from_utf8_lossy(&out.stderr);
    let said = said.trim();
    match out.status.success() {
        true => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        false if said.is_empty() => Err(format!("{program} failed ({})", out.status)),
        false => Err(said.strip_prefix(&format!("{program}: ")).unwrap_or(said).to_owned()),
    }
}

/// `secret-tool command…` for the account's keyring entry: `lookup` (the password), `store` (`input` is the password)
/// or `clear`.
pub fn secret_tool(command: &[&str], url: &str, user: &str, input: &str) -> Result<String, String> {
    let entry = ["service", "noobsbot-desktop-caldav", "url", url, "user", user];
    run("secret-tool", &[command, &entry].concat(), input)
}

/// The `REPORT` body: every VEVENT overlapping the window around `now` (unix seconds).
pub fn query(now: i64) -> String {
    let utc = |t: i64| chrono::DateTime::from_timestamp(t, 0).unwrap_or_default().format("%Y%m%dT%H%M%SZ");
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
<d:prop><c:calendar-data/></d:prop>
<c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT">
<c:time-range start="{}" end="{}"/>
</c:comp-filter></c:comp-filter></c:filter>
</c:calendar-query>"#,
        utc(now - BACK),
        utc(now + AHEAD)
    )
}

/// The calendar's events around `now` as iCalendar text (VCALENDARs one after another); else what went wrong, the
/// HTTP status among it.
pub fn fetch(url: &str, user: &str, password: &str, now: i64) -> Result<String, String> {
    // curl's config syntax: a quoted value with `\` escapes.
    let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r");
    let login = format!("user = \"{}:{}\"\n", quote(user), quote(password));
    let body = query(now);
    let args = ["-sS", "-f", "-L", "--proto", "=http,https", "--max-time", "30", "--anyauth", "--config", "-", "-X", "REPORT"];
    let headers = ["-H", "Depth: 1", "-H", "Content-Type: application/xml; charset=utf-8", "--data-binary", &body, "--url", url];
    // "(22) The requested URL returned error: 401" → the words.
    let xml = run("curl", &[&args[..], &headers].concat(), &login)
        .map_err(|e| e.strip_prefix('(').and_then(|r| r.split_once(") ")).map_or(e.clone(), |(_, words)| words.to_owned()))?;
    if !xml.contains("multistatus") {
        return Err("The server's answer isn't a CalDAV calendar's (no multistatus): is the URL the calendar's own?".into());
    }
    Ok(calendar_data(&xml).join("\n"))
}

/// Each `<…:calendar-data>` in a multistatus reply, unescaped (or out of its CDATA).
pub fn calendar_data(xml: &str) -> Vec<String> {
    const TAG: &str = "calendar-data";
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find(TAG) {
        // The tag from its `<` (a prefix like `C:` before the name) to its `>`.
        let start = rest[..at].rfind('<').unwrap_or(0);
        let Some(close) = rest[at..].find('>') else { break };
        let open = &rest[start..at + close];
        rest = &rest[at + close + 1..];
        if open.starts_with("</") || open.ends_with('/') {
            continue;
        }
        let Some(end) = rest.find(&format!("{TAG}>")) else { break };
        let text = rest[..end].rfind("</").map_or("", |i| &rest[..i]).trim();
        rest = &rest[end + TAG.len() + 1..];
        out.push(match text.strip_prefix("<![CDATA[").and_then(|t| t.strip_suffix("]]>")) {
            // A `]]>` in the text splits it into two sections.
            Some(raw) => raw.replace("]]><![CDATA[", ""),
            None => unescape(text),
        });
    }
    out
}

/// `&lt;`, `&amp;`, `&#13;`, `&#xD;`… as their characters; a stray `&` stays.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let entity = rest.find(';').map(|end| (&rest[1..end], end));
        let c = entity.and_then(|(name, _)| match name {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => {
                let code = name.strip_prefix('#')?;
                let n = match code.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => code.parse().ok()?,
                };
                char::from_u32(n)
            }
        });
        match (c, entity) {
            (Some(c), Some((_, end))) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_data_from_a_multistatus() {
        let xml = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav">
 <d:response><d:href>/cal/a.ics</d:href><d:propstat><d:prop>
  <cal:calendar-data>BEGIN:VCALENDAR&#13;
BEGIN:VEVENT&#13;
SUMMARY:Tom &amp; Jerry &lt;3 &quot;live&quot;&#xD;
DTSTART:20261020T120000Z&#13;
END:VEVENT&#13;
END:VCALENDAR&#13;
</cal:calendar-data>
 </d:prop></d:propstat></d:response>
 <d:response><d:href>/cal/b.ics</d:href><d:propstat><d:prop>
  <C:calendar-data xmlns:C="urn:ietf:params:xml:ns:caldav"><![CDATA[BEGIN:VCALENDAR
BEGIN:VEVENT
SUMMARY:A & B <raw>
DTSTART:20261021T090000Z
END:VEVENT
END:VCALENDAR]]></C:calendar-data>
 </d:prop></d:propstat>
 <d:propstat><d:prop><cal:calendar-data/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response>
</d:multistatus>"#;
        let data = calendar_data(xml);
        assert_eq!(data.len(), 2, "{data:?}");
        assert!(data[0].starts_with("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\n"), "{:?}", data[0]);
        assert!(data[0].contains("SUMMARY:Tom & Jerry <3 \"live\"\r\n"));
        assert!(data[1].contains("SUMMARY:A & B <raw>\n") && data[1].ends_with("END:VCALENDAR"));
        assert!(calendar_data("<d:multistatus xmlns:d=\"DAV:\"/>").is_empty());
    }

    #[test]
    fn stray_ampersands_stay() {
        assert_eq!(unescape("a & b &amp; &#65;&#x42; &bogus; &"), "a & b & AB &bogus; &");
    }

    #[test]
    fn query_asks_for_a_month_back_to_a_year_ahead() {
        // 2026-10-27 12:00 UTC.
        let body = query(1_793_102_400);
        assert!(body.contains(r#"<c:time-range start="20260927T120000Z" end="20271027T120000Z"/>"#), "{body}");
        assert!(body.contains(r#"<c:comp-filter name="VEVENT">"#));
    }
}
