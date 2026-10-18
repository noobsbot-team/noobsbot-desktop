//! fcitx5's input methods (E6.12) through its D-Bus controller, for the menu bar's input menu: the current group's
//! methods (m17n Nepali, Pinyin…) and which one is active. Read when fcitx5 appears or the menu opens: its
//! controller sends no signal when the method changes.

use smithay::reexports::calloop::channel::Sender;
use zbus::Connection;

use super::Update;

const SERVICE: &str = "org.fcitx.Fcitx5";
const PATH: &str = "/controller";

/// An available method: (unique name, name, native name, icon, label, language, configurable).
type Method = (String, String, String, String, String, String, bool);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fcitx {
    /// The current group's methods: (unique name, display name), e.g. ("m17n_ne_rom", "Nepali (Romanized)").
    pub methods: Vec<(String, String)>,
    /// The active method's unique name.
    pub current: String,
}

#[zbus::proxy(interface = "org.fcitx.Fcitx.Controller1", default_service = "org.fcitx.Fcitx5", default_path = "/controller")]
trait Controller {
    fn current_input_method(&self) -> zbus::Result<String>;
    #[zbus(name = "SetCurrentIM")]
    fn set_current_im(&self, name: &str) -> zbus::Result<()>;
    fn current_input_method_group(&self) -> zbus::Result<String>;
    /// (keyboard layout, [(method, layout)]).
    fn input_method_group_info(&self, name: &str) -> zbus::Result<(String, Vec<(String, String)>)>;
    fn available_input_methods(&self) -> zbus::Result<Vec<Method>>;
}

pub async fn watch(bus: &Connection, updates: &Sender<Update>) {
    super::follow(bus, SERVICE, PATH, updates, Update::Fcitx, read).await;
}

/// `None` while fcitx5 isn't on the bus.
pub async fn read(bus: &Connection) -> Option<Fcitx> {
    let proxy = ControllerProxy::new(bus).await.ok()?;
    let group = proxy.current_input_method_group().await.ok()?;
    let (_, items) = proxy.input_method_group_info(&group).await.ok()?;
    let names = proxy.available_input_methods().await.ok()?;
    Some(Fcitx { methods: label(items, &names), current: proxy.current_input_method().await.ok()? })
}

/// Each group item with its display name (the unique name if fcitx5 doesn't list it).
fn label(items: Vec<(String, String)>, available: &[Method]) -> Vec<(String, String)> {
    items
        .into_iter()
        .map(|(id, _)| {
            let name = available.iter().find(|a| a.0 == id).map_or_else(|| id.clone(), |a| a.1.clone());
            (id, name)
        })
        .collect()
}

/// Switches to method `name`, then reports the new state.
pub async fn set(bus: &Connection, name: &str, updates: &Sender<Update>) {
    let result = match ControllerProxy::new(bus).await {
        Ok(proxy) => proxy.set_current_im(name).await,
        Err(err) => Err(err),
    };
    if let Err(err) = result {
        tracing::warn!("input method {name}: {err}");
    }
    let _ = updates.send(Update::Fcitx(read(bus).await));
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader},
        os::unix::net::UnixStream,
        process::{Command, Stdio},
        sync::{Arc, Mutex},
    };

    use futures_lite::future;

    use super::*;

    #[test]
    fn labels_follow_available_methods() {
        let available = vec![("pinyin".into(), "Pinyin".into(), "拼音".into(), String::new(), String::new(), "zh_CN".into(), true)];
        let items = vec![("pinyin".into(), String::new()), ("keyboard-us".into(), String::new())];
        assert_eq!(label(items, &available), vec![("pinyin".into(), "Pinyin".into()), ("keyboard-us".into(), "keyboard-us".into())]);
    }

    /// fcitx5's controller as documented, enough for the menu.
    struct Mock(Arc<Mutex<String>>);

    #[zbus::interface(name = "org.fcitx.Fcitx.Controller1")]
    impl Mock {
        fn current_input_method(&self) -> String {
            self.0.lock().unwrap().clone()
        }
        #[zbus(name = "SetCurrentIM")]
        fn set_current_im(&self, name: String) {
            *self.0.lock().unwrap() = name;
        }
        fn current_input_method_group(&self) -> String {
            "Default".into()
        }
        fn input_method_group_info(&self, _name: String) -> (String, Vec<(String, String)>) {
            ("us".into(), vec![("keyboard-us".into(), String::new()), ("m17n_ne_rom".into(), String::new()), ("pinyin".into(), String::new())])
        }
        fn available_input_methods(&self) -> Vec<Method> {
            [("keyboard-us", "English (US)"), ("m17n_ne_rom", "Nepali (Romanized)"), ("pinyin", "Pinyin")]
                .map(|(id, name)| (id.into(), name.into(), String::new(), String::new(), String::new(), String::new(), false))
                .to_vec()
        }
    }

    /// Against a mock on a private bus of its own (never the session's); skipped without dbus-daemon.
    #[test]
    fn reads_and_switches_on_a_private_bus() {
        let Ok(mut daemon) = Command::new("dbus-daemon").args(["--session", "--nofork", "--print-address=1"]).stdout(Stdio::piped()).spawn() else {
            eprintln!("no dbus-daemon, skipped");
            return;
        };
        let mut address = String::new();
        BufReader::new(daemon.stdout.take().unwrap()).read_line(&mut address).unwrap();
        let path = address.trim().strip_prefix("unix:path=").and_then(|a| a.split(',').next()).unwrap().to_owned();
        let connect = || zbus::connection::Builder::async_io_unix_stream(UnixStream::connect(&path).unwrap());
        let current = Arc::new(Mutex::new("keyboard-us".to_owned()));
        let result = future::block_on(async {
            let _fcitx = connect().serve_at(PATH, Mock(current.clone()))?.name(SERVICE)?.build().await?;
            let bus = connect().build().await?;
            let before = read(&bus).await;
            let (tx, rx) = smithay::reexports::calloop::channel::channel();
            set(&bus, "m17n_ne_rom", &tx).await;
            zbus::Result::Ok((before, rx))
        });
        let _ = daemon.kill();
        let (before, rx) = result.unwrap();
        let before = before.unwrap();
        assert_eq!(before.current, "keyboard-us");
        assert_eq!(before.methods[1], ("m17n_ne_rom".into(), "Nepali (Romanized)".into()));
        assert_eq!(*current.lock().unwrap(), "m17n_ne_rom");
        match rx.try_recv() {
            Ok(Update::Fcitx(Some(after))) => assert_eq!(after.current, "m17n_ne_rom"),
            other => panic!("no update after switching: {other:?}"),
        }
    }
}
