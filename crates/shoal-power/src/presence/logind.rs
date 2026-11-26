//! systemd-logind's view of the user's presence, over the system bus.
//! It reads the idle hint of the user's sessions, not the manager's, which
//! the idle inhibitor that keep-awake holds would keep from idling. A user
//! without a session that takes input, as on a headless machine, reads as
//! idle to logind but gives no input to wait for, so counts as present.
use anyhow::{Context, Result};
use zbus::proxy::CacheProperties;

use super::{Lid, Platform};
use crate::logind::LoginProxy;
use shoal_daemon_base::log;

#[zbus::proxy(
    interface = "org.freedesktop.login1.User",
    default_service = "org.freedesktop.login1"
)]
pub(super) trait User {
    #[zbus(property)]
    fn idle_hint(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn sessions(&self) -> zbus::Result<Vec<(String, zbus::OwnedObjectPath)>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
pub(super) trait Session {
    #[zbus(property)]
    fn class(&self) -> zbus::Result<String>;
}

/// The input devices the kernel lists, a lid switch among them where the
/// machine has one.
const INPUT_DEVICES: &str = "/proc/bus/input/devices";

pub struct Logind {
    connection: zbus::Connection,
    manager: LoginProxy<'static>,
    user: UserProxy<'static>,
    /// Whether the machine has a lid, which the kernel lists once.
    lid: bool,
}

/// logind, when the system bus reaches it.
pub async fn connect() -> Option<Logind> {
    match Logind::connect().await {
        Ok(logind) => Some(logind),
        Err(error) => {
            log!("walked away: logind unavailable: {error:#}");
            None
        }
    }
}

impl Logind {
    /// Its properties are read afresh each time, since logind does not
    /// announce every change of them.
    async fn connect() -> Result<Self> {
        let connection = zbus::Connection::system()
            .await
            .context("connect to the system bus")?;
        let manager = (LoginProxy::builder(&connection))
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        let lid = has_lid(&std::fs::read_to_string(INPUT_DEVICES).unwrap_or_default());
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        let path = manager.get_user(uid).await.context("find the user")?;
        let user = (UserProxy::builder(&connection))
            .path(path)
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        Ok(Self {
            connection,
            manager,
            user,
            lid,
        })
    }
}

impl Platform for Logind {
    async fn lid(&mut self) -> Result<Option<Lid>> {
        if !self.lid {
            return Ok(None);
        }
        Ok(Some(Lid {
            closed: self.manager.lid_closed().await?,
            docked: self.manager.docked().await?,
        }))
    }

    async fn idle(&mut self) -> Result<bool> {
        if !self.takes_input().await? {
            return Ok(false);
        }
        Ok(self.user.idle_hint().await?)
    }
}

impl Logind {
    /// Whether one of the user's sessions takes the user's input, so its
    /// idleness says whether they are there.
    async fn takes_input(&self) -> Result<bool> {
        for (_, path) in self.user.sessions().await? {
            let session = (SessionProxy::builder(&self.connection))
                .path(path)
                .cache_properties(CacheProperties::No)
                .build()
                .await?;
            if can_idle(&session.class().await?) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Whether a session of logind's `class` can idle, as one the user works
/// in does; a service manager's or a background session cannot.
fn can_idle(class: &str) -> bool {
    matches!(
        class,
        "user" | "user-early" | "user-incomplete" | "greeter" | "lock-screen"
    )
}

/// Whether the kernel's input `devices` include a lid switch: one whose
/// switch bitmap sets `SW_LID`, its lowest bit.
fn has_lid(devices: &str) -> bool {
    (devices.lines())
        .filter_map(|line| line.strip_prefix("B: SW="))
        .filter_map(|bitmap| bitmap.split_whitespace().last())
        .filter_map(|word| u64::from_str_radix(word, 16).ok())
        .any(|switches| switches & 1 == 1)
}

#[cfg(test)]
mod tests;
