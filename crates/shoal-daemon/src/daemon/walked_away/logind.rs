//! systemd-logind's view of the user's presence, over the system bus: its
//! `LidClosed` on a machine with a lid, and otherwise the idle hint of the
//! user's sessions, which their desktop sets once it reads them as idle.
//! The user's hint, unlike the manager's, ignores the idle inhibitor that
//! keep-awake holds.
use anyhow::{Context, Result};
use zbus::proxy::CacheProperties;

use super::Platform;
use crate::daemon::{log, logind::LoginProxy};

#[zbus::proxy(
    interface = "org.freedesktop.login1.User",
    default_service = "org.freedesktop.login1"
)]
pub(super) trait User {
    #[zbus(property)]
    fn idle_hint(&self) -> zbus::Result<bool>;
}

/// The input devices the kernel lists, a lid switch among them where the
/// machine has one.
const INPUT_DEVICES: &str = "/proc/bus/input/devices";

pub(super) enum Logind {
    Lid(LoginProxy<'static>),
    Idle(UserProxy<'static>),
}

/// logind, when the system bus reaches it.
pub(super) async fn connect() -> Option<Logind> {
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
        if has_lid(&std::fs::read_to_string(INPUT_DEVICES).unwrap_or_default()) {
            return Ok(Self::Lid(manager));
        }
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        let path = manager.get_user(uid).await.context("find the user")?;
        let user = (UserProxy::builder(&connection))
            .path(path)
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        Ok(Self::Idle(user))
    }
}

impl Platform for Logind {
    async fn walked_away(&mut self) -> Result<bool> {
        Ok(match self {
            Self::Lid(manager) => manager.lid_closed().await?,
            Self::Idle(user) => user.idle_hint().await?,
        })
    }
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
