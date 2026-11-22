//! systemd-logind as the login manager, over the system bus.
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use zbus::OwnedFd;

use super::LoginManager;
use crate::daemon::{
    log,
    logind::{LoginProxy, PrepareForShutdownStream, delay_limit},
};

pub(super) struct Logind {
    proxy: LoginProxy<'static>,
    announcements: PrepareForShutdownStream,
}

/// logind, when the system bus reaches it.
pub(super) async fn connect() -> Option<Logind> {
    match Logind::connect().await {
        Ok(logind) => Some(logind),
        Err(error) => {
            log!("shutdown lock: logind unavailable: {error:#}");
            None
        }
    }
}

impl Logind {
    /// Subscribe to shutdown announcements before any lock is taken, so
    /// none passes unseen while the daemon holds one.
    async fn connect() -> Result<Self> {
        let connection = zbus::Connection::system()
            .await
            .context("connect to the system bus")?;
        let proxy = LoginProxy::new(&connection).await?;
        let announcements = proxy.receive_prepare_for_shutdown().await?;
        Ok(Self {
            proxy,
            announcements,
        })
    }
}

impl LoginManager for Logind {
    /// The lock lasts while logind's descriptor stays open.
    type Lock = OwnedFd;

    async fn delay_shutdown(&self) -> Result<OwnedFd> {
        self.proxy
            .inhibit(
                "shutdown",
                "Shoal",
                "Stop agents before shutting down",
                "delay",
            )
            .await
            .context("take logind's shutdown delay lock")
    }

    async fn delay_limit(&self) -> Duration {
        delay_limit(&self.proxy, "shutdown lock").await
    }

    async fn announcement(&mut self) -> Option<bool> {
        loop {
            match self.announcements.next().await?.args() {
                Ok(args) => return Some(args.start),
                Err(error) => log!("shutdown lock: unreadable announcement: {error:#}"),
            }
        }
    }
}
