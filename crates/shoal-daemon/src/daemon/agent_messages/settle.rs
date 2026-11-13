//! Holding a run of agent messages until it settles, so messages queued in
//! quick succession, such as the user's approval decisions, reach the agent in
//! one delivery instead of one hook output or turn each. Hooks, waits and wake
//! prompts deliver only settled messages; `shoal messages` reads them at once.
use std::time::Duration;

use anyhow::Result;

use super::AgentMessage;
use crate::{daemon::workspace::Manager, model::PresentSpace, time::UnixTime};

mod clock;
pub(crate) use clock::SettleClock;

/// A run is held at most this many settle windows after its first message,
/// so a steady stream still arrives.
const MAX_WINDOWS: u32 = 6;

/// When unread `messages` settle: `settle` after the newest, at most
/// [`MAX_WINDOWS`] windows after the oldest; `None` once they have settled at
/// `now`.
pub(crate) fn held_until(
    messages: &[AgentMessage],
    settle: Duration,
    now: UnixTime,
) -> Option<UnixTime> {
    let newest = messages.iter().map(|message| message.created_at).max()?;
    let oldest = messages.iter().map(|message| message.created_at).min()?;
    let settles = (newest + settle).min(oldest + settle.saturating_mul(MAX_WINDOWS));
    (now < settles).then_some(settles)
}

impl Manager {
    /// How long the unread `messages` of `space` wait for more to arrive;
    /// `None` once they have settled.
    pub(crate) async fn message_hold(
        &self,
        space: &PresentSpace,
        messages: &[AgentMessage],
    ) -> Result<Option<Duration>> {
        if messages.is_empty() {
            return Ok(None);
        }
        let now = self.settle_clock.now();
        let settle = self.settle_window(space).await;
        Ok(held_until(messages, settle, now).map(|until| until.duration_since(now)))
    }

    /// The unread messages of `space` once they have settled, waiting for
    /// them to.
    pub(crate) async fn settled_messages(
        &self,
        space: &PresentSpace,
        mut messages: Vec<AgentMessage>,
    ) -> Result<Vec<AgentMessage>> {
        while let Some(hold) = self.message_hold(space, &messages).await? {
            self.settle_clock.sleep(hold).await;
            messages = self.unread_messages(space.id()).await?.messages;
        }
        Ok(messages)
    }

    /// The unread messages of `space` that have settled: all of them, or
    /// none while they wait for more.
    pub(crate) async fn deliverable_messages(
        &self,
        space: &PresentSpace,
    ) -> Result<Vec<AgentMessage>> {
        let messages = self.unread_messages(space.id()).await?.messages;
        if self.message_hold(space, &messages).await?.is_some() {
            return Ok(Vec::new());
        }
        Ok(messages)
    }

    /// The settle window of `space`. A configuration that does not resolve
    /// holds nothing, since messages are how the user reaches the agent to
    /// fix it.
    async fn settle_window(&self, space: &PresentSpace) -> Duration {
        match self.present_space_settings(space).await {
            Ok(settings) => Duration::from_secs(settings.messages.settle_seconds),
            Err(error) => {
                crate::daemon::log!("agent messages delivered unsettled: {error:#}");
                Duration::ZERO
            }
        }
    }
}

#[cfg(test)]
mod tests;
