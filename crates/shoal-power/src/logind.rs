//! systemd-logind's manager over the system bus, whose delay locks and
//! announcements hold off a shutdown or a sleep until the daemon is ready,
//! and which says whether the lid is closed and the machine docked.
use std::time::Duration;

use zbus::OwnedFd;

use shoal_daemon_base::log;

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
pub(crate) trait Login {
    fn inhibit(&self, what: &str, who: &str, why: &str, mode: &str) -> zbus::Result<OwnedFd>;

    #[zbus(property, name = "InhibitDelayMaxUSec")]
    fn inhibit_delay_max_usec(&self) -> zbus::Result<u64>;

    #[zbus(property)]
    fn lid_closed(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn docked(&self) -> zbus::Result<bool>;

    fn get_user(&self, uid: u32) -> zbus::Result<zbus::OwnedObjectPath>;

    #[zbus(signal)]
    fn prepare_for_shutdown(&self, start: bool) -> zbus::Result<()>;

    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

/// logind's own default for how long a delay lock holds a shutdown or a
/// sleep off, for when it does not say.
const DEFAULT_DELAY_LIMIT: Duration = Duration::from_secs(5);

/// How long logind holds a shutdown or a sleep off for a delay lock, as
/// `proxy` reads it, logging under `lock` when it cannot.
pub(crate) async fn delay_limit(proxy: &LoginProxy<'_>, lock: &str) -> Duration {
    match proxy.inhibit_delay_max_usec().await {
        Ok(limit) => Duration::from_micros(limit),
        Err(error) => {
            log!("{lock}: logind's delay limit unreadable: {error:#}");
            DEFAULT_DELAY_LIMIT
        }
    }
}
