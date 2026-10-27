//! CalDAV calendars (`[calendar] caldav`): an account's calendars, found from its server's address or given as one
//! calendar's URL, then a `REPORT` each for the events from a month back to a year ahead, all through `curl`, with the
//! password from the keyring through `secret-tool` (libsecret). Shared with the Settings app (`#[path]`), which checks
//! an account with the same fetch and stores its password.
// ponytail: no sync tokens (the whole window each time). A `/.well-known/caldav` redirect to another host gets no
// password (curl's rule), so such a server's own CalDAV address is typed.

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

/// The events around `now` in every calendar at `url` (`calendars`) as iCalendar text (VCALENDARs one after another);
/// else what went wrong, the HTTP status among it.
pub fn fetch(url: &str, user: &str, password: &str, now: i64) -> Result<String, String> {
    // curl's config syntax: a quoted value with `\` escapes.
    let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r");
    let login = format!("user = \"{}:{}\"\n", quote(user), quote(password));
    let body = query(now);
    let mut events = Vec::new();
    for calendar in calendars(url, &login)? {
        let (xml, _) = dav("REPORT", &calendar, "1", &body, &login).map_err(|err| format!("{calendar}: {err}"))?;
        events.extend(calendar_data(&xml));
    }
    Ok(events.join("\n"))
}

/// The calendars that can hold events at `url` (RFC 4791, RFC 6764): itself when it is one, else those in the calendar
/// home of the principal found at `url` or at its server's `/.well-known/caldav`.
// ponytail: found again on each fetch (one PROPFIND for a calendar's own URL, three or so for a server's), no cache, so
// calendars added on the server show at the next fetch.
fn calendars(url: &str, login: &str) -> Result<Vec<String>, String> {
    let propfind = |at: &str, depth: &str, props: &str| {
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop>{props}</d:prop></d:propfind>"#
        );
        dav("PROPFIND", at, depth, &body, login)
    };
    // Last in, tried first: the URL, a principal it names, then the well-known one (and its principal).
    let mut next = vec![resolve(url, "/.well-known/caldav"), url.to_owned()];
    let mut said = Vec::new();
    let mut home = None;
    for _ in 0..4 {
        let Some(at) = next.pop() else { break };
        match propfind(&at, "0", "<d:resourcetype/><d:current-user-principal/><c:calendar-home-set/>") {
            Ok((xml, _)) if is_calendar(&xml) => return Ok(vec![at]),
            Ok((xml, base)) => match (href_in(&xml, "calendar-home-set", &base), href_in(&xml, "current-user-principal", &base)) {
                (Some(found), _) => {
                    home = Some(found);
                    break;
                }
                (None, Some(principal)) if principal != at => next.push(principal),
                (None, _) => said.push(format!("{at}: no calendar home")),
            },
            Err(err) => said.push(format!("{at}: {err}")),
        }
    }
    let home = home.ok_or_else(|| format!("No CalDAV calendars found ({})", said.join("; ")))?;
    let (xml, base) = propfind(&home, "1", "<d:resourcetype/><c:supported-calendar-component-set/>").map_err(|err| format!("{home}: {err}"))?;
    let found = event_calendars(&xml, &base);
    if found.is_empty() {
        return Err(format!("No calendars with events at {home}"));
    }
    Ok(found)
}

/// One WebDAV request through curl, `login` its config on stdin: the multistatus reply and the URL that answered it
/// (after redirects); else what went wrong, the HTTP status among it.
fn dav(method: &str, url: &str, depth: &str, body: &str, login: &str) -> Result<(String, String), String> {
    // Redirects keep the method and body; a URL typed without a scheme is https.
    let args = ["-sS", "-f", "-L", "--post301", "--post302", "--post303", "--proto", "=http,https", "--proto-default", "https"];
    let depth = format!("Depth: {depth}");
    let request = ["--max-time", "30", "--anyauth", "--config", "-", "-X", method, "-H", &depth, "-H", "Content-Type: application/xml; charset=utf-8"];
    let rest = ["--data-binary", body, "--write-out", "\n%{url_effective}", "--url", url];
    // "(22) The requested URL returned error: 401" → the words.
    let out = run("curl", &[&args[..], &request, &rest].concat(), login)
        .map_err(|e| e.strip_prefix('(').and_then(|r| r.split_once(") ")).map_or(e.clone(), |(_, words)| words.to_owned()))?;
    let (xml, answered) = out.rsplit_once('\n').unwrap_or((&out, url));
    match xml.contains("multistatus") {
        true => Ok((xml.to_owned(), answered.to_owned())),
        false => Err("not a CalDAV answer (no multistatus)".into()),
    }
}

/// Each `<…:calendar-data>` in a multistatus reply, unescaped (or out of its CDATA).
pub fn calendar_data(xml: &str) -> Vec<String> {
    let texts = elements(xml, "calendar-data").into_iter().map(str::trim).filter(|t| !t.is_empty());
    texts
        .map(|text| match text.strip_prefix("<![CDATA[").and_then(|t| t.strip_suffix("]]>")) {
            // A `]]>` in the text splits it into two sections.
            Some(raw) => raw.replace("]]><![CDATA[", ""),
            None => unescape(text),
        })
        .collect()
}

/// What's inside each `<name>` element in `xml`, whatever its namespace prefix (`d:href`, `D:href`, `href`); empty
/// for `<name/>`. Not nested in itself.
fn elements<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(lt) = rest.find('<') {
        rest = &rest[lt + 1..];
        let qname = &rest[..rest.find(|c: char| c.is_whitespace() || c == '/' || c == '>').unwrap_or(rest.len())];
        if qname.rsplit(':').next() != Some(name) {
            continue;
        }
        let Some(gt) = rest.find('>') else { break };
        let empty = rest[..gt].ends_with('/');
        rest = &rest[gt + 1..];
        if empty {
            out.push("");
            continue;
        }
        let close = format!("</{qname}>");
        let Some(end) = rest.find(&close) else { break };
        out.push(&rest[..end]);
        rest = &rest[end + close.len()..];
    }
    out
}

/// A reply's (or one `<response>`'s) resource is a calendar collection.
fn is_calendar(xml: &str) -> bool {
    elements(xml, "resourcetype").iter().any(|kinds| !elements(kinds, "calendar").is_empty())
}

/// The first `<href>` inside a `<name>` (`current-user-principal`, `calendar-home-set`) in a reply to `base`.
fn href_in(xml: &str, name: &str, base: &str) -> Option<String> {
    elements(xml, name).iter().find_map(|e| elements(e, "href").first().map(|href| resolve(base, &unescape(href.trim()))))
}

/// The calendars that can hold events in a Depth 1 listing of a calendar home at `base` (tasks-only lists, inboxes,
/// subscriptions and the home itself aren't).
fn event_calendars(xml: &str, base: &str) -> Vec<String> {
    let holds_events = |response: &&str| {
        // No component set (or not found): every kind.
        let kinds = elements(response, "supported-calendar-component-set");
        is_calendar(response) && kinds.iter().all(|k| k.trim().is_empty() || k.contains("VEVENT"))
    };
    let own_href = |response: &str| elements(response, "href").first().map(|href| resolve(base, &unescape(href.trim())));
    elements(xml, "response").into_iter().filter(holds_events).filter_map(own_href).collect()
}

/// An `<href>` from a reply to `base`: as it is when it's a whole URL, else a path on `base`'s server.
fn resolve(base: &str, href: &str) -> String {
    if href.contains("://") {
        return href.to_owned();
    }
    let host = base.find("://").map_or(0, |i| i + 3);
    let origin = &base[..base[host..].find('/').map_or(base.len(), |i| host + i)];
    format!("{origin}/{}", href.trim_start_matches('/'))
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
    fn discovery_with_lowercase_prefixes() {
        // The server's DAV root, Depth 0: only the principal.
        let root = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:s="http://sabredav.org/ns" xmlns:cal="urn:ietf:params:xml:ns:caldav" xmlns:cs="http://calendarserver.org/ns/" xmlns:nc="http://nextcloud.org/ns">
 <d:response>
  <d:href>/remote.php/dav/</d:href>
  <d:propstat>
   <d:prop>
    <d:resourcetype><d:collection/></d:resourcetype>
    <d:current-user-principal>
     <d:href>/remote.php/dav/principals/users/me/</d:href>
    </d:current-user-principal>
   </d:prop>
   <d:status>HTTP/1.1 200 OK</d:status>
  </d:propstat>
  <d:propstat>
   <d:prop><cal:calendar-home-set/></d:prop>
   <d:status>HTTP/1.1 404 Not Found</d:status>
  </d:propstat>
 </d:response>
</d:multistatus>"#;
        let base = "https://cloud.example.com/remote.php/dav/";
        assert!(!is_calendar(root));
        assert_eq!(href_in(root, "calendar-home-set", base), None);
        assert_eq!(href_in(root, "current-user-principal", base).as_deref(), Some("https://cloud.example.com/remote.php/dav/principals/users/me/"));
        // The principal: its calendar home.
        let principal = r#"<d:multistatus xmlns:d="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav"><d:response>
<d:href>/remote.php/dav/principals/users/me/</d:href><d:propstat><d:prop>
<d:resourcetype><d:principal/></d:resourcetype>
<d:current-user-principal><d:href>/remote.php/dav/principals/users/me/</d:href></d:current-user-principal>
<cal:calendar-home-set><d:href>/remote.php/dav/calendars/me/</d:href></cal:calendar-home-set>
</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
        assert_eq!(href_in(principal, "calendar-home-set", base).as_deref(), Some("https://cloud.example.com/remote.php/dav/calendars/me/"));
        // The home, Depth 1.
        let home = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav" xmlns:cs="http://calendarserver.org/ns/" xmlns:nc="http://nextcloud.org/ns">
 <d:response><d:href>/remote.php/dav/calendars/me/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  <d:propstat><d:prop><cal:supported-calendar-component-set/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>
 </d:response>
 <d:response><d:href>/remote.php/dav/calendars/me/personal/</d:href>
  <d:propstat><d:prop>
   <d:resourcetype><d:collection/><cal:calendar/></d:resourcetype>
   <cal:supported-calendar-component-set><cal:comp name="VEVENT"/></cal:supported-calendar-component-set>
  </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 </d:response>
 <d:response><d:href>/remote.php/dav/calendars/me/tasks/</d:href>
  <d:propstat><d:prop>
   <d:resourcetype><d:collection/><cal:calendar/></d:resourcetype>
   <cal:supported-calendar-component-set><cal:comp name="VTODO"/></cal:supported-calendar-component-set>
  </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 </d:response>
 <d:response><d:href>/remote.php/dav/calendars/me/work%20%26%20play/</d:href>
  <d:propstat><d:prop>
   <d:resourcetype><d:collection/><cal:calendar/></d:resourcetype>
   <cal:supported-calendar-component-set><cal:comp name="VEVENT"/><cal:comp name="VTODO"/></cal:supported-calendar-component-set>
  </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 </d:response>
 <d:response><d:href>/remote.php/dav/calendars/me/inbox/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/><cal:schedule-inbox/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 </d:response>
 <d:response><d:href>/remote.php/dav/calendars/me/holidays/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/><cs:subscribed/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 </d:response>
 <d:response><d:href>/remote.php/dav/calendars/me/trashbin/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/><nc:trash-bin/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 </d:response>
</d:multistatus>"#;
        let base = "https://cloud.example.com/remote.php/dav/calendars/me/";
        assert_eq!(
            event_calendars(home, base),
            ["https://cloud.example.com/remote.php/dav/calendars/me/personal/", "https://cloud.example.com/remote.php/dav/calendars/me/work%20%26%20play/"]
        );
    }

    #[test]
    fn discovery_with_other_prefixes() {
        // A calendar's own URL, Depth 0: it is one.
        let own = r#"<?xml version="1.0" encoding="UTF-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:response>
<D:href>/dav/calendars/user/me@example.com/Default/</D:href>
<D:propstat><D:prop><D:resourcetype><D:collection/><C:calendar/></D:resourcetype>
<D:current-user-principal><D:href>/dav/principals/user/me@example.com/</D:href></D:current-user-principal></D:prop>
<D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>"#;
        assert!(is_calendar(own));
        // A principal whose home is on another host.
        let principal = r#"<D:multistatus xmlns:D="DAV:"><D:response><D:href>/123/principal/</D:href><D:propstat><D:prop>
<C:calendar-home-set xmlns:C="urn:ietf:params:xml:ns:caldav"><D:href>https://p42-caldav.example.net:443/123/calendars/</D:href></C:calendar-home-set>
</D:prop></D:propstat></D:response></D:multistatus>"#;
        assert_eq!(
            href_in(principal, "calendar-home-set", "https://caldav.example.net/").as_deref(),
            Some("https://p42-caldav.example.net:443/123/calendars/")
        );
        // The DAV namespace as the default one, spaces before `/>`, an address book, a calendar without a component set.
        let home = r#"<?xml version='1.0' encoding='utf-8'?>
<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:CR="urn:ietf:params:xml:ns:carddav"><response><href>/me/</href><propstat><prop><resourcetype><principal /><collection /></resourcetype></prop><status>HTTP/1.1 200 OK</status></propstat><propstat><prop><C:supported-calendar-component-set /></prop><status>HTTP/1.1 404 Not Found</status></propstat></response><response><href>/me/2b8f6b6e-a0f1/</href><propstat><prop><resourcetype><C:calendar /><collection /></resourcetype><C:supported-calendar-component-set><C:comp name="VEVENT" /><C:comp name="VJOURNAL" /><C:comp name="VTODO" /></C:supported-calendar-component-set></prop><status>HTTP/1.1 200 OK</status></propstat></response><response><href>/me/contacts/</href><propstat><prop><resourcetype><CR:addressbook /><collection /></resourcetype></prop><status>HTTP/1.1 200 OK</status></propstat></response><response><href>/me/journal/</href><propstat><prop><resourcetype><C:calendar /><collection /></resourcetype><C:supported-calendar-component-set><C:comp name="VJOURNAL" /></C:supported-calendar-component-set></prop><status>HTTP/1.1 200 OK</status></propstat></response><response><href>/me/old/</href><propstat><prop><resourcetype><C:calendar /><collection /></resourcetype></prop><status>HTTP/1.1 200 OK</status></propstat><propstat><prop><C:supported-calendar-component-set /></prop><status>HTTP/1.1 404 Not Found</status></propstat></response></multistatus>"#;
        assert_eq!(event_calendars(home, "http://localhost:5232/me/"), ["http://localhost:5232/me/2b8f6b6e-a0f1/", "http://localhost:5232/me/old/"]);
    }

    #[test]
    fn hrefs_resolve_on_the_server() {
        let base = "https://cloud.example.com:8443/remote.php/dav/";
        assert_eq!(resolve(base, "/remote.php/dav/principals/users/me/"), "https://cloud.example.com:8443/remote.php/dav/principals/users/me/");
        assert_eq!(resolve(base, "calendars/me/"), "https://cloud.example.com:8443/calendars/me/");
        assert_eq!(resolve(base, "https://p42-caldav.example.net:443/123/calendars/"), "https://p42-caldav.example.net:443/123/calendars/");
        assert_eq!(resolve("https://dav.example.org", "/.well-known/caldav"), "https://dav.example.org/.well-known/caldav");
        // Typed without a scheme: curl's `--proto-default https` adds it.
        assert_eq!(resolve("dav.example.org/me/", "/.well-known/caldav"), "dav.example.org/.well-known/caldav");
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
