//! Holding a run of alerts to an agent until it settles, so alerts queued in
//! quick succession reach it in one delivery. Hooks, waits and wake prompts
//! deliver only settled alerts; `shoal alert list` reads them at once.
use std::time::Duration;

use anyhow::Result;

use shoal_daemon_base::alerts::Alert;

use crate::{Agents, daemon::Daemon, model::Space, time::UnixTime};

mod clock;
pub use clock::SettleClock;

/// A run is held at most this many settle windows after its first message,
/// so a steady stream still arrives.
const MAX_WINDOWS: u32 = 6;

/// When unread `messages` settle: `settle` after the newest, at most
/// [`MAX_WINDOWS`] windows after the oldest; `None` once they have settled at
/// `now`.
pub fn held_until(messages: &[Alert], settle: Duration, now: UnixTime) -> Option<UnixTime> {
    let newest = messages.iter().map(|message| message.created_at).max()?;
    let oldest = messages.iter().map(|message| message.created_at).min()?;
    let settles = (newest + settle).min(oldest + settle.saturating_mul(MAX_WINDOWS));
    (now < settles).then_some(settles)
}

impl Agents {
    /// How long the unread `messages` of `space` wait for more to arrive;
    /// `None` once they have settled.
    pub async fn message_hold(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        messages: &[Alert],
    ) -> Result<Option<Duration>> {
        if messages.is_empty() {
            return Ok(None);
        }
        let now = self.settle_clock.now();
        let settle = self.settle_window(daemon, space).await;
        Ok(held_until(messages, settle, now).map(|until| until.duration_since(now)))
    }

    /// The unread messages of `space` once they have settled, waiting for
    /// them to.
    pub async fn settled_messages(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        mut messages: Vec<Alert>,
    ) -> Result<Vec<Alert>> {
        while let Some(hold) = self.message_hold(daemon, space, &messages).await? {
            self.settle_clock.sleep(hold).await;
            messages = self.unread_messages(&space.id).await?.alerts;
        }
        Ok(messages)
    }

    /// The unread messages of `space` that have settled: all of them, or
    /// none while they wait for more.
    pub async fn deliverable_messages(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
    ) -> Result<Vec<Alert>> {
        let messages = self.unread_messages(&space.id).await?.alerts;
        if self.message_hold(daemon, space, &messages).await?.is_some() {
            return Ok(Vec::new());
        }
        Ok(messages)
    }

    /// The settle window of `space`. A configuration that does not resolve
    /// holds nothing, since messages are how the user reaches the agent to
    /// fix it.
    async fn settle_window(&self, daemon: &dyn Daemon, space: &Space) -> Duration {
        match daemon.space_settings(space).await {
            Ok(settings) => Duration::from_secs(settings.alerts.settle_seconds),
            Err(error) => {
                crate::log!("agent messages delivered unsettled: {error:#}");
                Duration::ZERO
            }
        }
    }
}

#[cfg(test)]
mod tests;
