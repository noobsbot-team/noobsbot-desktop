//! The graphical login screen (E7.2): `noobsbot-desktop --greeter`, which greetd starts in place of tuigreet. It is
//! the lock screen (shell/lock.rs, render/lock.rs) with a row of users, a session choice and greetd behind the field
//! instead of PAM: greetd checks the password, and starts the chosen session once we exit. greetd's IPC is a few
//! length-prefixed JSON messages over `$GREETD_SOCK`, spoken here directly.

use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
};

use smithay::{
    input::keyboard::Keysym,
    reexports::calloop::channel,
    utils::{Logical, Point, Rectangle},
};

use crate::{shell::lock::Layout, state::State, wm::Rect};

/// ponytail: more users than fit one row on an 800 px screen are left out; a scrolling row when someone has that many.
const MAX_USERS: usize = 6;
/// One user's column in the row: the picture centred above the name.
const SLOT: i32 = 112;

#[derive(Debug, Clone, PartialEq)]
pub struct User {
    pub login: String,
    pub name: String,
    pub home: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub name: String,
    pub cmd: Vec<String>,
    /// The `.desktop` file's name ("noobsbot-desktop"), for `XDG_SESSION_DESKTOP`.
    pub id: String,
}

#[derive(Debug, Default)]
pub struct Greeter {
    pub users: Vec<User>,
    pub user: usize,
    pub sessions: Vec<Session>,
    pub session: usize,
    /// What greetd said went wrong (other than a wrong password), shown under the field.
    pub error: Option<String>,
}

impl Greeter {
    /// Users from /etc/passwd, sessions from /usr/share/wayland-sessions.
    pub fn load() -> Self {
        let users = users(&std::fs::read_to_string("/etc/passwd").unwrap_or_default());
        Self { users, sessions: sessions(Path::new("/usr/share/wayland-sessions")), ..Self::default() }
    }

    /// Sample users for `--show greeter-users`.
    pub fn sample() -> Self {
        let user = |login: &str, name: &str| User { login: login.into(), name: name.into(), home: PathBuf::new() };
        let users = vec![user("anil", "Anil Shrestha"), user("guest", "Guest"), user("sam", "Sam")];
        Self { users, sessions: sessions(&Path::new(env!("CARGO_MANIFEST_DIR")).join("data/system/usr/share/wayland-sessions")), ..Self::default() }
    }

    pub fn session(&self) -> Option<&Session> {
        self.sessions.get(self.session)
    }
}

/// Login users: uid 1000 to 59999 (login.defs' range) with a shell that lets them in.
pub fn users(passwd: &str) -> Vec<User> {
    let mut users: Vec<(u32, User)> = passwd
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split(':').collect();
            let [login, _, uid, _, gecos, home, shell] = f[..] else { return None };
            let uid: u32 = uid.parse().ok()?;
            let shut = shell.is_empty() || shell.ends_with("/nologin") || shell.ends_with("/false");
            if !(1000..60000).contains(&uid) || shut {
                return None;
            }
            let name = gecos.split(',').next().unwrap_or_default().trim();
            let name = if name.is_empty() { login } else { name };
            Some((uid, User { login: login.into(), name: name.into(), home: home.into() }))
        })
        .collect();
    users.sort_by_key(|(uid, _)| *uid);
    users.into_iter().map(|(_, u)| u).take(MAX_USERS).collect()
}

/// The Wayland sessions in `dir`, Noobsbot Desktop first, then by name; Noobsbot Desktop alone if there are none.
pub fn sessions(dir: &Path) -> Vec<Session> {
    let mut list: Vec<Session> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let id = path.file_stem()?.to_str()?.to_owned();
            (path.extension()? == "desktop").then_some(())?;
            session(&std::fs::read_to_string(&path).ok()?, id)
        })
        .collect();
    list.sort_by(|a, b| (a.id != "noobsbot-desktop", &a.name).cmp(&(b.id != "noobsbot-desktop", &b.name)));
    if list.is_empty() {
        list.push(Session { name: "Noobsbot Desktop".into(), cmd: vec!["noobsbot-desktop-session".into()], id: "noobsbot-desktop".into() });
    }
    list
}

/// `Name` and `Exec` of a session's `[Desktop Entry]`; `None` when it is hidden or has no command.
/// ponytail: Exec is split at spaces and its %-codes dropped; quoted arguments would need the spec's unquoting.
fn session(desktop: &str, id: String) -> Option<Session> {
    let (mut name, mut exec) = (None, None);
    let mut entry = false;
    for line in desktop.lines().map(str::trim) {
        if line.starts_with('[') {
            entry = line == "[Desktop Entry]";
        } else if entry && let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                "Name" => name = name.or(Some(value.trim().to_owned())),
                "Exec" => exec = exec.or(Some(value.trim().to_owned())),
                "Hidden" | "NoDisplay" if value.trim() == "true" => return None,
                _ => {}
            }
        }
    }
    let cmd: Vec<String> = exec?.split_whitespace().filter(|a| !a.starts_with('%')).map(String::from).collect();
    (!cmd.is_empty()).then(|| Session { name: name.unwrap_or_else(|| id.clone()), cmd, id })
}

/// The user row and the session choice, over the lock screen's layout: one user sits exactly where the lock
/// screen's picture and name do; more spread out in `SLOT` columns, centred.
pub struct Row {
    pub avatars: Vec<Rect>,
    pub names: Vec<Rect>,
    /// Bottom left, mirroring the keyboard/battery corner.
    pub session: Rect,
}

pub fn row(layout: &Layout, width: i32, users: usize) -> Row {
    let n = users.max(1) as i32;
    let left = (width - n * SLOT) / 2;
    let x = |i: i32| if n == 1 { layout.avatar.loc.x } else { left + i * SLOT + (SLOT - layout.avatar.size.w) / 2 };
    let avatars = (0..n).map(|i| Rectangle::new((x(i), layout.avatar.loc.y).into(), layout.avatar.size)).collect();
    let names = (0..n)
        .map(|i| if n == 1 { layout.name } else { Rectangle::new((left + i * SLOT + 4, layout.name.loc.y).into(), (SLOT - 8, layout.name.size.h).into()) })
        .collect();
    let s = layout.status;
    let session = Rectangle::new((width - s.loc.x - s.size.w, s.loc.y).into(), s.size);
    Row { avatars, names, session }
}

/// What greetd made of a login.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// The session starts when we exit.
    Started,
    Wrong,
    Failed(String),
}

/// A reply from greetd.
#[derive(Debug, PartialEq)]
enum Reply {
    Success,
    Error {
        auth: bool,
        description: String,
    },
    /// A PAM prompt; `answer` when it wants one (secret or visible), not for info and error messages.
    Auth {
        answer: bool,
        message: String,
    },
}

/// `s` as a JSON string.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The string fields of a flat JSON object (greetd's replies); other values are skipped.
/// ponytail: a lone `\u` surrogate becomes U+FFFD and pairs aren't joined; PAM messages are ASCII in practice.
fn fields(json: &str) -> Option<Vec<(String, String)>> {
    let mut chars = json.trim().strip_prefix('{')?.chars().peekable();
    let string = |chars: &mut std::iter::Peekable<std::str::Chars>| -> Option<String> {
        let mut s = String::new();
        loop {
            match chars.next()? {
                '"' => return Some(s),
                '\\' => s.push(match chars.next()? {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    'u' => char::from_u32(u32::from_str_radix(&chars.by_ref().take(4).collect::<String>(), 16).ok()?).unwrap_or('\u{fffd}'),
                    c => c,
                }),
                c => s.push(c),
            }
        }
    };
    let mut out = Vec::new();
    loop {
        match chars.by_ref().find(|c| !c.is_whitespace() && *c != ',')? {
            '}' => return Some(out),
            '"' => {
                let key = string(&mut chars)?;
                chars.by_ref().find(|c| *c == ':')?;
                while chars.peek()?.is_whitespace() {
                    chars.next();
                }
                if chars.peek() == Some(&'"') {
                    chars.next();
                    out.push((key, string(&mut chars)?));
                } else {
                    // null, true, a number: up to the next separator.
                    while !matches!(chars.peek()?, ',' | '}') {
                        chars.next();
                    }
                }
            }
            _ => return None,
        }
    }
}

fn send(stream: &mut impl Write, json: &[u8]) -> io::Result<()> {
    let len = u32::try_from(json.len()).map_err(|_| io::Error::other("message too long"))?;
    stream.write_all(&len.to_ne_bytes())?;
    stream.write_all(json)
}

fn recv(stream: &mut impl Read) -> io::Result<Reply> {
    let mut len = [0; 4];
    stream.read_exact(&mut len)?;
    let len = u32::from_ne_bytes(len) as usize;
    if len > 1 << 20 {
        return Err(io::Error::other("reply too long"));
    }
    let mut body = vec![0; len];
    stream.read_exact(&mut body)?;
    let bad = || io::Error::other(format!("bad reply: {}", String::from_utf8_lossy(&body)));
    let fields = fields(std::str::from_utf8(&body).map_err(|_| bad())?).ok_or_else(bad)?;
    let get = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap_or_default();
    Ok(match get("type").as_str() {
        "success" => Reply::Success,
        "error" => Reply::Error { auth: get("error_type") == "auth_error", description: get("description") },
        "auth_message" => Reply::Auth { answer: matches!(get("auth_message_type").as_str(), "secret" | "visible"), message: get("auth_message") },
        _ => return Err(bad()),
    })
}

/// Logs `user` in over a greetd connection and asks for `cmd`. Answers the first prompt with the password; a second
/// one (a one-time code, an expired password) isn't supported and fails with its text.
pub fn login(stream: &mut (impl Read + Write), user: &str, password: &str, cmd: &[String], env: &[String]) -> io::Result<Outcome> {
    fn cancel(stream: &mut (impl Read + Write)) -> io::Result<()> {
        send(stream, br#"{"type":"cancel_session"}"#).and_then(|()| recv(stream)).map(drop)
    }
    send(stream, format!(r#"{{"type":"create_session","username":{}}}"#, quote(user)).as_bytes())?;
    let mut answered = false;
    loop {
        match recv(stream)? {
            Reply::Success => break,
            Reply::Auth { answer: true, message } if answered => {
                cancel(stream)?;
                return Ok(Outcome::Failed(message));
            }
            Reply::Auth { answer: true, .. } => {
                let mut json = format!(r#"{{"type":"post_auth_message_response","response":{}}}"#, quote(password)).into_bytes();
                let sent = send(stream, &json);
                crate::pam::zero(&mut json);
                sent?;
                answered = true;
            }
            Reply::Auth { answer: false, .. } => send(stream, br#"{"type":"post_auth_message_response"}"#)?,
            Reply::Error { auth, description } => {
                cancel(stream)?;
                return Ok(if auth { Outcome::Wrong } else { Outcome::Failed(description) });
            }
        }
    }
    let list = |items: &[String]| items.iter().map(|s| quote(s)).collect::<Vec<_>>().join(",");
    send(stream, format!(r#"{{"type":"start_session","cmd":[{}],"env":[{}]}}"#, list(cmd), list(env)).as_bytes())?;
    match recv(stream)? {
        Reply::Success => Ok(Outcome::Started),
        Reply::Error { description, .. } => {
            cancel(stream)?;
            Ok(Outcome::Failed(description))
        }
        Reply::Auth { .. } => Ok(Outcome::Failed("greetd asked again after starting".into())),
    }
}

/// [`login`] on `$GREETD_SOCK`.
fn login_greetd(user: &str, password: &str, session: &Session) -> Outcome {
    let Some(path) = std::env::var_os("GREETD_SOCK") else { return Outcome::Failed("greetd isn't running (no GREETD_SOCK)".into()) };
    let env = ["XDG_SESSION_TYPE=wayland".to_owned(), format!("XDG_SESSION_DESKTOP={}", session.id)];
    UnixStream::connect(path)
        .and_then(|mut stream| {
            // greetd answers within its PAM stack's time; a stuck one shouldn't hold the login screen for good.
            stream.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
            login(&mut stream, user, password, &session.cmd, &env)
        })
        .unwrap_or_else(|err| Outcome::Failed(format!("greetd: {err}")))
}

impl State {
    pub fn greeter(&self) -> Option<&Greeter> {
        self.lock.as_ref()?.greeter.as_ref()
    }

    /// Greeter mode: the login screen is up from the start and never goes away.
    pub fn start_greeter(&mut self, greeter: Greeter) {
        self.lock_screen();
        if let Some(lock) = self.lock.as_mut() {
            lock.greeter = Some(greeter);
        }
    }

    /// Tab and Shift+Tab pick the next or previous user: `true` if the key did.
    pub(super) fn greeter_key(&mut self, keysym: Keysym) -> bool {
        let Some(lock) = self.lock.as_mut().filter(|l| l.checking.is_none()) else { return false };
        let Some(g) = lock.greeter.as_ref().filter(|g| g.users.len() > 1) else { return false };
        let n = g.users.len();
        let user = match keysym {
            Keysym::Tab => (g.user + 1) % n,
            Keysym::ISO_Left_Tab => (g.user + n - 1) % n,
            _ => return false,
        };
        self.greeter_pick(user);
        true
    }

    fn greeter_pick(&mut self, user: usize) {
        if let Some(lock) = self.lock.as_mut()
            && let Some(g) = lock.greeter.as_mut()
        {
            g.user = user;
            g.error = None;
            lock.wrong = None;
            lock.field = crate::ui::field::TextField::secure();
        }
        self.schedule_redraw();
    }

    /// A press on a user picks them; on the session name, the next session. `true` if it hit one.
    pub(super) fn greeter_press(&mut self, layout: &Layout, location: Point<f64, Logical>) -> bool {
        let width = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)).map_or(0, |g| g.size.w);
        let Some(g) = self.greeter().filter(|_| self.lock.as_ref().is_some_and(|l| l.checking.is_none())) else { return false };
        let row = row(layout, width, g.users.len());
        let hit = |r: &Rect| r.to_f64().contains(location);
        if let Some(user) = (0..g.users.len()).find(|&i| hit(&row.avatars[i]) || hit(&row.names[i])) {
            self.greeter_pick(user);
            return true;
        }
        if hit(&row.session) && g.sessions.len() > 1 {
            let next = (g.session + 1) % g.sessions.len();
            if let Some(g) = self.lock.as_mut().and_then(|l| l.greeter.as_mut()) {
                g.session = next;
            }
            self.schedule_redraw();
            return true;
        }
        false
    }

    /// Hands the password to greetd on a worker thread: the session starts (we exit), or the field shakes.
    pub(super) fn greeter_submit(&mut self, password: String) {
        let Some(g) = self.greeter() else { return };
        let (Some(user), Some(session)) = (g.users.get(g.user).map(|u| u.login.clone()), g.session().cloned()) else { return };
        let (tx, rx) = channel::channel();
        let token = self.loop_handle.insert_source(rx, |event, _, state| {
            let channel::Event::Msg(outcome) = event else { return };
            if let Some(token) = state.lock.as_mut().and_then(|l| l.checking.take()) {
                state.loop_handle.insert_idle(move |state| state.loop_handle.remove(token));
            }
            tracing::info!("greeter: {outcome:?}");
            match outcome {
                Outcome::Started => state.loop_signal.stop(),
                Outcome::Wrong => state.lock_checked(false),
                Outcome::Failed(why) => {
                    if let Some(g) = state.lock.as_mut().and_then(|l| l.greeter.as_mut()) {
                        g.error = Some(why);
                    }
                    state.lock_checked(false);
                }
            }
        });
        let Ok(token) = token.inspect_err(|err| tracing::warn!("greeter: {err}")) else { return };
        let spawned = std::thread::Builder::new().name("greetd".into()).spawn(move || {
            let mut password = password.into_bytes();
            let outcome = login_greetd(&user, &String::from_utf8_lossy(&password), &session);
            crate::pam::zero(&mut password);
            let _ = tx.send(outcome);
        });
        match spawned {
            Ok(_) => {
                if let Some(lock) = self.lock.as_mut() {
                    lock.checking = Some(token);
                }
            }
            Err(err) => {
                tracing::warn!("greeter: {err}");
                self.loop_handle.remove(token);
            }
        }
        self.schedule_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::lock::layout;

    #[test]
    fn users_are_people_with_shells() {
        let passwd = "root:x:0:0::/root:/usr/bin/bash\n\
            anil:x:1000:1000:Anil Shrestha,,,:/home/anil:/usr/bin/zsh\n\
            nobody:x:65534:65534:Kernel Overflow User:/:/usr/sbin/nologin\n\
            svc:x:1001:1001::/srv:/usr/bin/nologin\n\
            off:x:1003:1003::/home/off:/bin/false\n\
            bob:x:1002:1002::/home/bob:/bin/bash\n\
            broken line";
        let users = users(passwd);
        let logins: Vec<&str> = users.iter().map(|u| u.login.as_str()).collect();
        assert_eq!(logins, ["anil", "bob"]);
        assert_eq!((users[0].name.as_str(), users[1].name.as_str()), ("Anil Shrestha", "bob"));
        assert_eq!(users[0].home, PathBuf::from("/home/anil"));
    }

    #[test]
    fn session_files() {
        let s = session("[Desktop Entry]\nName=Plasma (Wayland)\nName[de]=X\nExec=/usr/lib/plasma-dbus-run-session-if-needed startplasma-wayland %U\n[Desktop Action x]\nExec=no", "plasma".into()).unwrap();
        assert_eq!(s.name, "Plasma (Wayland)");
        assert_eq!(s.cmd, ["/usr/lib/plasma-dbus-run-session-if-needed", "startplasma-wayland"]);
        assert!(session("[Desktop Entry]\nName=Gone\nExec=x\nHidden=true", "g".into()).is_none());
        assert!(session("[Desktop Entry]\nName=No command", "n".into()).is_none());
        let ours = sessions(Path::new("data/system/usr/share/wayland-sessions"));
        assert_eq!((ours[0].id.as_str(), ours[0].cmd.as_slice()), ("noobsbot-desktop", &["noobsbot-desktop-session".to_owned()][..]));
        assert_eq!(sessions(Path::new("/nonexistent"))[0].cmd, ["noobsbot-desktop-session"]);
    }

    #[test]
    fn row_matches_the_lock_screen_for_one_user() {
        let l = layout((1280, 800).into());
        let one = row(&l, 1280, 1);
        assert_eq!((one.avatars[0], one.names[0]), (l.avatar, l.name));
        let three = row(&l, 1280, 3);
        assert_eq!(three.avatars[1], l.avatar, "the middle of three is centred");
        assert!(three.avatars[0].loc.x + three.avatars[0].size.w < three.avatars[1].loc.x);
        assert!(three.names.windows(2).all(|w| w[0].loc.x + w[0].size.w < w[1].loc.x));
        assert_eq!(three.session.loc.x, 1280 - l.status.loc.x - l.status.size.w);
        let six = row(&layout((800, 600).into()), 800, MAX_USERS);
        assert!(six.avatars[0].loc.x >= 0 && six.names[5].loc.x + six.names[5].size.w <= 800, "six fit 800 px");
    }

    #[test]
    fn json_round_trip() {
        assert_eq!(quote("a\"b\\c\n"), r#""a\"b\\c\u000a""#);
        let f = fields(r#" { "type": "error", "error_type":"auth_error" , "description": "x \"y\" é", "n": null, "b": true } "#).unwrap();
        assert_eq!(f, [("type", "error"), ("error_type", "auth_error"), ("description", "x \"y\" é")].map(|(k, v)| (k.to_owned(), v.to_owned())));
        assert_eq!(fields("{}").unwrap(), []);
        assert!(fields(r#"{"a":"unterminated"#).is_none() && fields("[1]").is_none());
    }

    /// A fake greetd on the other end of a socket pair: it reads each request and answers from a script.
    fn fake(script: Vec<(&'static str, &'static str)>) -> (UnixStream, std::thread::JoinHandle<Vec<String>>) {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (want, reply) in script {
                let mut len = [0; 4];
                theirs.read_exact(&mut len).unwrap();
                let mut body = vec![0; u32::from_ne_bytes(len) as usize];
                theirs.read_exact(&mut body).unwrap();
                let body = String::from_utf8(body).unwrap();
                assert!(body.contains(want), "{body} lacks {want}");
                seen.push(body);
                send(&mut theirs, reply.as_bytes()).unwrap();
            }
            seen
        });
        (ours, server)
    }

    const SECRET: &str = r#"{"type":"auth_message","auth_message_type":"secret","auth_message":"Password: "}"#;
    const OK: &str = r#"{"type":"success"}"#;

    #[test]
    fn greetd_login() {
        let cmd = ["noobsbot-desktop-session".to_owned()];
        let (mut s, server) = fake(vec![("create_session", SECRET), (r#""response":"pw""#, OK), ("start_session", OK)]);
        assert_eq!(login(&mut s, "anil", "pw", &cmd, &["A=b".into()]).unwrap(), Outcome::Started);
        let seen = server.join().unwrap();
        assert_eq!(seen[0], r#"{"type":"create_session","username":"anil"}"#);
        assert_eq!(seen[2], r#"{"type":"start_session","cmd":["noobsbot-desktop-session"],"env":["A=b"]}"#);

        // Wrong password: the session is cancelled so the next try can create one.
        let wrong = r#"{"type":"error","error_type":"auth_error","description":"pam_authenticate: AUTH_ERR"}"#;
        let (mut s, server) = fake(vec![("create_session", SECRET), ("response", wrong), ("cancel_session", OK)]);
        assert_eq!(login(&mut s, "anil", "bad", &cmd, &[]).unwrap(), Outcome::Wrong);
        server.join().unwrap();

        // No password (passwordless PAM): an info message is acknowledged, then straight to the session.
        let info = r#"{"type":"auth_message","auth_message_type":"info","auth_message":"Welcome"}"#;
        let (mut s, server) = fake(vec![("create_session", info), (r#"{"type":"post_auth_message_response"}"#, OK), ("start_session", OK)]);
        assert_eq!(login(&mut s, "guest", "", &cmd, &[]).unwrap(), Outcome::Started);
        server.join().unwrap();

        // A second prompt fails with its text; other errors carry greetd's description.
        let code = r#"{"type":"auth_message","auth_message_type":"visible","auth_message":"Code: "}"#;
        let (mut s, server) = fake(vec![("create_session", SECRET), ("response", code), ("cancel_session", OK)]);
        assert_eq!(login(&mut s, "anil", "pw", &cmd, &[]).unwrap(), Outcome::Failed("Code: ".into()));
        server.join().unwrap();
        let busy = r#"{"type":"error","error_type":"error","description":"a session is already being configured"}"#;
        let (mut s, server) = fake(vec![("create_session", busy), ("cancel_session", OK)]);
        assert_eq!(login(&mut s, "anil", "pw", &cmd, &[]).unwrap(), Outcome::Failed("a session is already being configured".into()));
        server.join().unwrap();
    }
}
