//! Delivering a chat's messages and the activity of its items to its
//! agent.
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::daemon::links::wait::UpdatesExt;
use anyhow::Result;

use crate::daemon::{
    links::wait::{RESPONSE_LIMIT, Updates},
    workspace::Manager,
};

/// How soon a chat's wait looks again at item activity an item change held
/// back under its PR gate.
const GATE_RETRY: std::time::Duration = std::time::Duration::from_millis(500);

impl Manager {
    /// What the chat's agent has not received, without waiting: its
    /// messages, its leftovers, and the activity queued on its items.
    /// Delivery replaces a running wait, which would otherwise deliver them
    /// again.
    pub async fn chat_updates(&self, chat: &SpaceId) -> Result<Updates> {
        let delivery = self.waits.start(chat);
        let messages = self.agent_messages(&SpaceSelector::id(chat)).await?;
        // The messages, then the leftovers, share the response with the
        // updates.
        let messages_size = serde_json::to_vec(&messages)?.len();
        let room = RESPONSE_LIMIT.saturating_sub(messages_size);
        let leftovers = self.leftovers.undelivered(chat, room)?;
        let reserved = messages_size + serde_json::to_vec(&leftovers)?.len();
        let updates = (self.queued_chat_updates(chat, reserved).await?).unwrap_or_default();
        if !delivery.is_current() {
            return Ok(Updates::superseded());
        }
        Ok(Updates {
            updates,
            leftovers,
            ..Updates::messages(messages)
        })
    }

    /// The chat's next messages or item activity, once it has any, unless a
    /// newer wait or delivery takes them first.
    pub async fn wait_chat_messages(&self, chat: &SpaceId) -> Result<Updates> {
        let wait = self.waits.start(chat);
        // Subscribe before reading, so what is queued in between still wakes.
        let mut queued = self.signals.watch_messages();
        let mut background = self.signals.watch_item_updates();
        loop {
            let messages = self.agent_messages(&SpaceSelector::id(chat)).await?;
            let reserved = serde_json::to_vec(&messages)?.len();
            let queued_updates = self.queued_chat_updates(chat, reserved).await?;
            // Releasing the PR gate signals nothing, so a held one is
            // checked again shortly.
            let gated = queued_updates.is_none();
            let updates = queued_updates.unwrap_or_default();
            if !messages.is_empty() || !updates.is_empty() {
                if !wait.is_current() {
                    return Ok(Updates::superseded());
                }
                return Ok(Updates {
                    updates,
                    ..Updates::messages(messages)
                });
            }
            tokio::select! {
                changed = queued.changed() => changed?,
                changed = background.changed() => changed?,
                () = tokio::time::sleep(GATE_RETRY), if gated => {}
                () = wait.superseded.notified() => return Ok(Updates::superseded()),
            }
        }
    }
}

#[cfg(test)]
mod tests;
