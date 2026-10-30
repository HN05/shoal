//! Holding a run of agent messages until it settles, so messages queued in
//! quick succession, such as the user's approval decisions, reach the agent in
//! one delivery instead of one hook output or turn each. Hooks, waits and wake
//! prompts deliver only settled messages; `shoal messages` reads them at once.
use std::time::Duration;

use anyhow::Result;

use super::AgentMessage;
use crate::{daemon::workspace::Manager, model::Workspace};

/// A run is held at most this many settle windows after its first message,
/// so a steady stream still arrives.
const MAX_WINDOWS: i64 = 6;

/// The Unix second at which unread `messages` settle: `settle` seconds after
/// the newest, at most [`MAX_WINDOWS`] windows after the oldest; `None` once
/// they have settled at `now`.
pub(crate) fn held_until(messages: &[AgentMessage], settle: u64, now: i64) -> Option<i64> {
    let settle = i64::try_from(settle).ok()?;
    let newest = messages.iter().map(|message| message.created_at).max()?;
    let oldest = messages.iter().map(|message| message.created_at).min()?;
    let settles = (newest + settle).min(oldest + settle * MAX_WINDOWS);
    (now < settles).then_some(settles)
}

impl Manager {
    /// How long the workspace's unread `messages` wait for more to arrive;
    /// `None` once they have settled.
    pub(crate) async fn message_hold(
        &self,
        workspace: &Workspace,
        messages: &[AgentMessage],
    ) -> Result<Option<Duration>> {
        if messages.is_empty() {
            return Ok(None);
        }
        let now = i64::try_from(crate::time::unix_seconds())?;
        let settle = self.settle_seconds(workspace).await;
        Ok(held_until(messages, settle, now).map(|until| Duration::from_secs(until.abs_diff(now))))
    }

    /// The workspace's unread messages once they have settled, waiting for
    /// them to.
    pub(crate) async fn settled_messages(
        &self,
        workspace: &Workspace,
        mut messages: Vec<AgentMessage>,
    ) -> Result<Vec<AgentMessage>> {
        while let Some(hold) = self.message_hold(workspace, &messages).await? {
            tokio::time::sleep(hold).await;
            messages = self.agent_messages(&workspace.id).await?;
        }
        Ok(messages)
    }

    /// The workspace's unread messages that have settled: all of them, or
    /// none while they wait for more.
    pub(crate) async fn deliverable_messages(
        &self,
        workspace: &Workspace,
    ) -> Result<Vec<AgentMessage>> {
        let messages = self.agent_messages(&workspace.id).await?;
        if self.message_hold(workspace, &messages).await?.is_some() {
            return Ok(Vec::new());
        }
        Ok(messages)
    }

    /// The workspace's settle window. A configuration that does not resolve
    /// holds nothing, since messages are how the user reaches the agent to
    /// fix it.
    async fn settle_seconds(&self, workspace: &Workspace) -> u64 {
        match self.workspace_settings(workspace).await {
            Ok(settings) => settings.messages.settle_seconds,
            Err(error) => {
                crate::daemon::log!("agent messages delivered unsettled: {error:#}");
                0
            }
        }
    }
}

#[cfg(test)]
mod tests;
