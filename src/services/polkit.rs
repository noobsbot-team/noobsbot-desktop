//! Our polkit authentication agent (plan.md E7.2): admin password prompts in our alert instead of the fallback agent's dialog.
//! polkitd calls `BeginAuthentication`; the alert asks for the password, and polkit's own helper checks it with PAM
//! and tells polkitd (we never see whether a password is right other than by its SUCCESS line). Registered only in
//! a real session (not nested), for our login session; if that fails, the standard Qt agent is started instead.

use std::{
    collections::HashMap,
    ffi::{CStr, c_char},
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Mutex,
};

use smithay::reexports::calloop::channel::Sender;
use zbus::{
    Connection,
    zvariant::{OwnedValue, Value},
};

use super::{Answer, Update};

/// Where polkitd calls us.
pub const PATH: &str = "/org/noobsbot/desktop/PolkitAgent";
/// polkit ≥ 126's systemd socket for the (then not setuid) helper; the setuid helper is the fallback.
const SOCKET: &str = "/run/polkit/agent-helper.socket";
const HELPER: &str = "/usr/lib/polkit-1/polkit-agent-helper-1";

/// A request for the alert.
#[derive(Debug, Clone, PartialEq)]
pub struct Auth {
    /// polkitd's id for this request: the answer and the close name it.
    pub cookie: String,
    /// The action's message, e.g. "Authentication is required to run … as the super user".
    pub message: String,
    /// Who may authenticate, the desktop's user first.
    pub users: Vec<String>,
}

#[derive(Debug)]
pub enum Event {
    Ask(Auth),
    /// That password was wrong: ask again.
    Wrong(String),
    /// Done (allowed, cancelled or given up by polkitd): close its alert.
    Done(String),
}

/// The alert's answer: (user, password), or `None`: cancelled.
pub type Reply = Option<(String, String)>;

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.PolicyKit1.Error")]
enum PolkitError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Failed(String),
    Cancelled(String),
}

type Identity = (String, HashMap<String, OwnedValue>);

struct Agent {
    updates: Sender<Update>,
    answer: Answer<Reply>,
    /// The request being asked about; a cancel for any other cookie is ignored.
    current: Mutex<Option<String>>,
    socket: PathBuf,
    helper: PathBuf,
}

#[zbus::interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl Agent {
    // ponytail: one request at a time; a second one cancels the first (polkitd tells its app "not authorized").
    async fn begin_authentication(
        &self,
        _action_id: String,
        message: String,
        _icon_name: String,
        _details: HashMap<String, String>,
        cookie: String,
        identities: Vec<Identity>,
    ) -> Result<(), PolkitError> {
        // SAFETY: getuid has no failure mode.
        let users = users(&identities, unsafe { libc::getuid() });
        if users.is_empty() {
            return Err(PolkitError::Failed("no user to authenticate as".into()));
        }
        if let Ok(mut c) = self.current.lock() {
            *c = Some(cookie.clone());
        }
        let mut rx = self.answer.wait();
        let _ = self.updates.send(Update::Polkit(Event::Ask(Auth { cookie: cookie.clone(), message, users })));
        loop {
            let Ok(Some((user, password))) = rx.recv().await else { break };
            let (socket, helper, c) = (self.socket.clone(), self.helper.clone(), cookie.clone());
            let ok = blocking(move || check(&socket, &helper, &user, &c, password)).await;
            // Cancelled, or a newer request took over.
            if self.current.lock().is_ok_and(|c| c.as_deref() != Some(cookie.as_str())) {
                break;
            }
            if ok {
                let _ = self.updates.send(Update::Polkit(Event::Done(cookie)));
                return Ok(());
            }
            rx = self.answer.wait();
            let _ = self.updates.send(Update::Polkit(Event::Wrong(cookie.clone())));
        }
        let _ = self.updates.send(Update::Polkit(Event::Done(cookie)));
        Err(PolkitError::Cancelled("cancelled".into()))
    }

    fn cancel_authentication(&self, cookie: String) {
        if !self.current.lock().is_ok_and(|mut c| c.take_if(|c| *c == cookie).is_some()) {
            return;
        }
        self.answer.cancel();
        let _ = self.updates.send(Update::Polkit(Event::Done(cookie)));
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.PolicyKit1.Authority",
    default_service = "org.freedesktop.PolicyKit1",
    default_path = "/org/freedesktop/PolicyKit1/Authority"
)]
trait Authority {
    fn register_authentication_agent(&self, subject: &(&str, HashMap<&str, Value<'_>>), locale: &str, object_path: &str) -> zbus::Result<()>;
}

#[zbus::proxy(interface = "org.freedesktop.login1.Session", default_service = "org.freedesktop.login1", default_path = "/org/freedesktop/login1/session/auto")]
trait Session {
    #[zbus(property)]
    fn id(&self) -> zbus::Result<String>;
}

/// Serves the agent and registers it for our login session; `false` if polkitd said no (or isn't there).
pub async fn serve(bus: &Connection, updates: &Sender<Update>, answer: &Answer<Reply>) -> bool {
    serve_with(bus, updates, answer, SOCKET.into(), HELPER.into()).await
}

async fn serve_with(bus: &Connection, updates: &Sender<Update>, answer: &Answer<Reply>, socket: PathBuf, helper: PathBuf) -> bool {
    let result = async {
        let agent = Agent { updates: updates.clone(), answer: answer.clone(), current: Mutex::default(), socket, helper };
        bus.object_server().at(PATH, agent).await?;
        // pam_systemd sets it; logind's "auto" session is the caller's own otherwise.
        let session = match std::env::var("XDG_SESSION_ID") {
            Ok(id) if !id.is_empty() => id,
            _ => SessionProxy::new(bus).await?.id().await?,
        };
        let subject = ("unix-session", HashMap::from([("session-id", Value::from(session.as_str()))]));
        let locale = std::env::var("LANG").unwrap_or_else(|_| "en_US.UTF-8".into());
        AuthorityProxy::new(bus).await?.register_authentication_agent(&subject, &locale, PATH).await
    };
    match result.await {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!("polkit agent off: {err}");
            false
        }
    }
}

/// The standard Qt agent, when ours couldn't register (kept installed as the fallback).
pub fn fallback() {
    let started = std::process::Command::new("systemctl").args(["--user", "start", "--no-block", "plasma-polkit-agent.service"]).status();
    if let Err(err) = started {
        tracing::warn!("polkit-kde-agent: {err}");
    }
}

/// The unix-user identities' login names, `me` first (polkitd has already expanded admin groups to users).
fn users(identities: &[Identity], me: u32) -> Vec<String> {
    let mut uids: Vec<u32> = identities.iter().filter(|(kind, _)| kind == "unix-user").filter_map(|(_, d)| d.get("uid")?.downcast_ref::<u32>().ok()).collect();
    // Duplicates are only adjacent once sorted; the partial sort after that is stable.
    uids.sort_unstable();
    uids.dedup();
    uids.sort_by_key(|&uid| uid != me);
    uids.into_iter().filter_map(user_name).collect()
}

fn user_name(uid: u32) -> Option<String> {
    let mut buf = vec![0 as c_char; 4096];
    // SAFETY: getpwuid_r fills `pw` with pointers into `buf`, which outlives the read below.
    unsafe {
        let mut pw: libc::passwd = std::mem::zeroed();
        let mut found = std::ptr::null_mut();
        if libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr(), buf.len(), &mut found) != 0 || found.is_null() {
            return None;
        }
        Some(CStr::from_ptr(pw.pw_name).to_string_lossy().into_owned())
    }
}

/// Runs `work` on its own thread (PAM takes seconds on a wrong password), so this thread's other services go on.
async fn blocking<T: Default + Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = async_channel::bounded(1);
    if std::thread::Builder::new().name("polkit".into()).spawn(move || tx.send_blocking(work())).is_err() {
        return T::default();
    }
    rx.recv().await.unwrap_or_default()
}

/// One try of `password` for `user` through polkit's helper: the systemd socket when it exists (polkit ≥ 126), else
/// the setuid helper. The helper tells polkitd itself; `true` when it said SUCCESS.
fn check(socket: &Path, helper: &Path, user: &str, cookie: &str, password: String) -> bool {
    let mut password = password.into_bytes();
    let ok = (|| {
        if socket.exists() {
            let stream = UnixStream::connect(socket).ok()?;
            let mut out = stream.try_clone().ok()?;
            writeln!(out, "{user}\n{cookie}").ok()?;
            return Some(converse(BufReader::new(stream), out, &password));
        }
        let mut child = std::process::Command::new(helper).arg(user).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().ok()?;
        let (mut stdin, stdout) = (child.stdin.take()?, child.stdout.take()?);
        writeln!(stdin, "{cookie}").ok()?;
        let ok = converse(BufReader::new(stdout), stdin, &password);
        let _ = child.wait();
        Some(ok)
    })()
    .unwrap_or(false);
    password.fill(0);
    ok
}

/// The helper's line protocol: every PAM prompt gets the password; SUCCESS or FAILURE ends it.
// ponytail: every prompt is answered with the password (as the lock screen does); a second factor that asks for
// something else fails, which shows as a wrong password.
fn converse(lines: impl BufRead, mut out: impl Write, password: &[u8]) -> bool {
    // The helper reads a line per prompt: a newline inside the password would answer the next one.
    if password.contains(&b'\n') {
        return false;
    }
    for line in lines.lines() {
        let Ok(line) = line else { return false };
        if line.starts_with("PAM_PROMPT_ECHO_OFF") || line.starts_with("PAM_PROMPT_ECHO_ON") {
            if out.write_all(password).and_then(|()| out.write_all(b"\n")).and_then(|()| out.flush()).is_err() {
                return false;
            }
        } else if line.starts_with("SUCCESS") {
            return true;
        } else if line.starts_with("FAILURE") {
            return false;
        } else {
            tracing::debug!("polkit helper: {line}");
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("noobsbot-polkit-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fake helper: checks it got the user and cookie, asks once, says SUCCESS only for "right".
    fn fake_helper(dir: &Path) -> PathBuf {
        let path = dir.join("helper");
        let script = "#!/bin/sh\n[ \"$1\" = alice ] || exit 3\nread cookie\n[ \"$cookie\" = c00kie ] || { echo FAILURE; exit 1; }\n\
            echo 'PAM_TEXT_INFO hello'\necho 'PAM_PROMPT_ECHO_OFF Password: '\nread pw\n\
            if [ \"$pw\" = right ]; then echo SUCCESS; else echo FAILURE; fi\n";
        std::fs::write(&path, script).unwrap();
        std::process::Command::new("chmod").arg("+x").arg(&path).status().unwrap();
        path
    }

    #[test]
    fn helper_protocol() {
        let dir = scratch("helper");
        let (socket, helper) = (dir.join("none.socket"), fake_helper(&dir));
        assert!(check(&socket, &helper, "alice", "c00kie", "right".into()));
        assert!(!check(&socket, &helper, "alice", "c00kie", "wrong".into()));
        assert!(!check(&socket, &helper, "alice", "other", "right".into()));
        assert!(!check(&socket, &dir.join("missing"), "alice", "c00kie", "right".into()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn socket_protocol() {
        let dir = scratch("socket");
        let socket = dir.join("agent-helper.socket");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut out = stream.try_clone().unwrap();
            let mut lines = BufReader::new(stream).lines();
            let (user, cookie) = (lines.next().unwrap().unwrap(), lines.next().unwrap().unwrap());
            writeln!(out, "PAM_PROMPT_ECHO_OFF Password: ").unwrap();
            let password = lines.next().unwrap().unwrap();
            writeln!(out, "{}", if (user.as_str(), cookie.as_str(), password.as_str()) == ("bob", "k", "pw") { "SUCCESS" } else { "FAILURE" }).unwrap();
        });
        assert!(check(&socket, Path::new("/nonexistent"), "bob", "k", "pw".into()));
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn identities_to_users() {
        let user = |uid: u32| ("unix-user".to_string(), HashMap::from([("uid".to_string(), OwnedValue::from(uid))]));
        let group = ("unix-group".to_string(), HashMap::from([("gid".to_string(), OwnedValue::from(0u32))]));
        // SAFETY: getuid has no failure mode.
        let me = unsafe { libc::getuid() };
        let (login, _) = crate::pam::user();
        // Logged-in user first, root once (just root when the tests run as root, as in CI).
        let want = if me == 0 { vec![login] } else { vec![login, "root".to_string()] };
        assert_eq!(users(&[user(0), group, user(me), user(0)], me), want);
        assert!(users(&[user(u32::MAX - 7)], me).is_empty(), "an unknown uid is left out");
        assert_eq!(users(&[user(me), user(0), user(me)], me).len(), want.len(), "a duplicate that isn't adjacent is dropped too");
    }

    /// The interface on a private dbus-daemon, with a mock Authority: registration, a wrong then a right
    /// password, and polkitd's CancelAuthentication.
    #[test]
    fn agent_on_private_bus() {
        use smithay::reexports::calloop::{EventLoop, channel};

        let dir = scratch("bus");
        let address = dir.join("bus");
        let mut daemon = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--nopidfile"])
            .arg(format!("--address=unix:path={}", address.display()))
            .spawn()
            .expect("dbus-daemon");
        while !address.exists() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let connect = || async {
            let socket = UnixStream::connect(&address).unwrap();
            zbus::connection::Builder::async_io_unix_stream(socket).build().await.unwrap()
        };

        struct Authority(async_channel::Sender<(String, String, String)>);
        #[zbus::interface(name = "org.freedesktop.PolicyKit1.Authority")]
        impl Authority {
            fn register_authentication_agent(&self, subject: (String, HashMap<String, OwnedValue>), _locale: String, path: String) {
                let session = subject.1.get("session-id").and_then(|v| v.downcast_ref::<String>().ok()).unwrap_or_default();
                let _ = self.0.try_send((subject.0, session, path));
            }
        }

        let (updates, rx) = channel::channel();
        let mut event_loop: EventLoop<Vec<Update>> = EventLoop::try_new().unwrap();
        event_loop
            .handle()
            .insert_source(rx, |event, _, seen| {
                if let channel::Event::Msg(update) = event {
                    seen.push(update);
                }
            })
            .unwrap();
        let mut seen = Vec::new();
        let mut next = |seen: &mut Vec<Update>| {
            while seen.is_empty() {
                event_loop.dispatch(std::time::Duration::from_secs(5), seen).unwrap();
            }
            seen.remove(0)
        };

        let helper = fake_helper(&dir);
        let answer = Answer::default();
        let (registered_tx, registered) = async_channel::unbounded();
        let (polkitd, agent) = futures_lite::future::block_on(async {
            let polkitd = connect().await;
            polkitd.object_server().at("/org/freedesktop/PolicyKit1/Authority", Authority(registered_tx)).await.unwrap();
            polkitd.request_name("org.freedesktop.PolicyKit1").await.unwrap();
            let agent = connect().await;
            // SAFETY: tests run single-threaded on this variable (only this test reads it).
            unsafe { std::env::set_var("XDG_SESSION_ID", "c7") };
            assert!(serve_with(&agent, &updates, &answer, dir.join("none.socket"), helper).await);
            (polkitd, agent)
        });
        let got = futures_lite::future::block_on(registered.recv()).unwrap();
        assert_eq!(got, ("unix-session".into(), "c7".into(), PATH.into()));

        // polkitd's side: BeginAuthentication blocks until the alert is answered, so it runs on its own thread.
        let begin = |cookie: &'static str| {
            let (polkitd, name) = (polkitd.clone(), agent.unique_name().unwrap().to_owned());
            std::thread::spawn(move || {
                futures_lite::future::block_on(async {
                    // SAFETY: getuid has no failure mode.
                    let me = unsafe { libc::getuid() };
                    let ids = vec![("unix-user", HashMap::from([("uid", Value::from(me))]))];
                    let body = ("act", "Authentication is required.", "", HashMap::<&str, &str>::new(), cookie, ids);
                    let reply =
                        polkitd.call_method(Some(name), PATH, Some("org.freedesktop.PolicyKit1.AuthenticationAgent"), "BeginAuthentication", &body).await;
                    reply.map(drop).map_err(|e| e.to_string())
                })
            })
        };
        let (login, _) = crate::pam::user();
        let ask = |update| match update {
            Update::Polkit(Event::Ask(auth)) => auth,
            other => panic!("{other:?}"),
        };

        // A wrong password, then the right one.
        let call = begin("c00kie");
        let auth = ask(next(&mut seen));
        assert_eq!((auth.cookie.as_str(), auth.users.clone()), ("c00kie", vec![login.clone()]));
        // The fake helper takes "alice" only; the user is passed through as the alert chose it.
        answer.give(Some(("alice".into(), "wrong".into())));
        assert!(matches!(next(&mut seen), Update::Polkit(Event::Wrong(c)) if c == "c00kie"));
        answer.give(Some(("alice".into(), "right".into())));
        assert!(matches!(next(&mut seen), Update::Polkit(Event::Done(c)) if c == "c00kie"));
        assert_eq!(call.join().unwrap(), Ok(()));

        // Cancel in the alert.
        let call = begin("c2");
        ask(next(&mut seen));
        answer.give(None);
        assert!(matches!(next(&mut seen), Update::Polkit(Event::Done(c)) if c == "c2"));
        assert!(call.join().unwrap().unwrap_err().contains("org.freedesktop.PolicyKit1.Error.Cancelled"));

        // polkitd gives up.
        let call = begin("c3");
        ask(next(&mut seen));
        futures_lite::future::block_on(polkitd.call_method(
            Some(agent.unique_name().unwrap().to_owned()),
            PATH,
            Some("org.freedesktop.PolicyKit1.AuthenticationAgent"),
            "CancelAuthentication",
            &("c3",),
        ))
        .unwrap();
        assert!(call.join().unwrap().unwrap_err().contains("Cancelled"));

        let _ = daemon.kill();
        let _ = daemon.wait();
        let _ = std::fs::remove_dir_all(dir);
    }
}
