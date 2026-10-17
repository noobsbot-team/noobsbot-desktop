//! The power mode from power-profiles-daemon (E2.3): power saver, balanced or performance.

use smithay::reexports::calloop::channel::Sender;
use zbus::{Connection, proxy::CacheProperties};

use super::Update;

const SERVICE: &str = "org.freedesktop.UPower.PowerProfiles";
const PATH: &str = "/org/freedesktop/UPower/PowerProfiles";

#[derive(Clone, Debug, PartialEq)]
pub struct Profiles {
    /// `power-saver`, `balanced` or `performance`.
    pub active: String,
    /// What this machine offers (performance needs hardware support).
    pub available: Vec<String>,
}

#[zbus::proxy(interface = "org.freedesktop.UPower.PowerProfiles", default_service = "org.freedesktop.UPower.PowerProfiles", default_path = "/org/freedesktop/UPower/PowerProfiles")]
trait PowerProfiles {
    #[zbus(property)]
    fn active_profile(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn set_active_profile(&self, profile: &str) -> zbus::Result<()>;
    #[zbus(property)]
    fn profiles(&self) -> zbus::Result<Vec<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>>;
}

pub async fn watch(bus: &Connection, updates: &Sender<Update>) {
    super::follow(bus, SERVICE, PATH, updates, Update::Profiles, read).await;
}

/// `None` when power-profiles-daemon isn't installed or running.
async fn read(bus: &Connection) -> Option<Profiles> {
    let proxy = PowerProfilesProxy::builder(bus).cache_properties(CacheProperties::No).build().await.ok()?;
    let available = proxy
        .profiles()
        .await
        .ok()?
        .iter()
        .filter_map(|p| p.get("Profile")?.downcast_ref::<&str>().ok().map(str::to_owned))
        .collect();
    Some(Profiles { active: proxy.active_profile().await.ok()?, available })
}

pub async fn set(bus: &Connection, profile: &str) {
    let result = match PowerProfilesProxy::builder(bus).cache_properties(CacheProperties::No).build().await {
        Ok(proxy) => proxy.set_active_profile(profile).await,
        Err(err) => Err(err),
    };
    if let Err(err) = result {
        tracing::warn!("power mode {profile}: {err}");
    }
}
