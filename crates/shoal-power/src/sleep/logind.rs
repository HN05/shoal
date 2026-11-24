//! systemd-logind's sleep announcements, over the system bus: the daemon
//! holds a sleep delay lock while the machine is awake, which logind waits
//! for up to its delay limit once it announces a sleep.
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use zbus::OwnedFd;

use super::{PowerEvent, PowerEvents};
use crate::logind::{LoginProxy, PrepareForSleepStream, delay_limit};
use shoal_daemon_base::log;

pub struct Logind {
    proxy: LoginProxy<'static>,
    announcements: PrepareForSleepStream,
    /// The delay lock, while the machine is awake.
    lock: Option<OwnedFd>,
}

/// logind, when the system bus reaches it.
pub async fn connect() -> Option<Logind> {
    match Logind::connect().await {
        Ok(logind) => Some(logind),
        Err(error) => {
            log!("sleep lock: logind unavailable: {error:#}");
            None
        }
    }
}

impl Logind {
    /// Subscribe to sleep announcements before the lock is taken, so none
    /// passes unseen while the daemon holds it.
    async fn connect() -> Result<Self> {
        let connection = zbus::Connection::system()
            .await
            .context("connect to the system bus")?;
        let proxy = LoginProxy::new(&connection).await?;
        let announcements = proxy.receive_prepare_for_sleep().await?;
        let mut logind = Self {
            proxy,
            announcements,
            lock: None,
        };
        logind.take_lock().await;
        Ok(logind)
    }

    /// Take the sleep delay lock, unless the daemon holds it; a refusal
    /// leaves sleeps to go on at once, still announced.
    async fn take_lock(&mut self) {
        if self.lock.is_some() {
            return;
        }
        let taken = self
            .proxy
            .inhibit("sleep", "Shoal", "Tell the hub before sleeping", "delay")
            .await;
        match taken {
            Ok(lock) => self.lock = Some(lock),
            Err(error) => log!("sleep lock: logind refused its sleep delay lock: {error:#}"),
        }
    }

    async fn allowance(&self) -> Duration {
        delay_limit(&self.proxy, "sleep lock").await
    }
}

impl PowerEvents for Logind {
    /// The sleep waits while logind's descriptor stays open.
    type Hold = OwnedFd;

    async fn next(&mut self) -> Option<PowerEvent<OwnedFd>> {
        loop {
            let announced = match self.announcements.next().await?.args() {
                Ok(args) => args.start,
                Err(error) => {
                    log!("sleep lock: unreadable announcement: {error:#}");
                    continue;
                }
            };
            if announced {
                let allowance = self.allowance().await;
                let hold = self.lock.take();
                return Some(PowerEvent::Sleeping { hold, allowance });
            }
            // Held again for the next sleep.
            self.take_lock().await;
            return Some(PowerEvent::Woke);
        }
    }
}
