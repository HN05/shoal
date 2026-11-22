//! Attaching a launch to the chat its start records.
use anyhow::{Result, ensure};
use rusqlite::params;

use super::{blockers, store};
use crate::Agents;
use crate::state::QueueKind;
use crate::state::QueueState;
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};

impl Agents {
    /// Record `chat` as the one the launch `id` starts its agent in, whether
    /// it starts or waits, unless a launch already holds the chat's agent,
    /// and let admission take a launch kept for this.
    pub async fn attach_chat(&self, id: &QueueEntryId, chat: &SpaceId) -> Result<()> {
        let attached = self.attach_chat_row(id, chat).await;
        self.agent_queue.end_attach_keep(id);
        self.agent_queue.wake();
        attached
    }

    /// Record `chat` on the launch `id`'s row as [`Self::attach_chat`] does.
    async fn attach_chat_row(&self, id: &QueueEntryId, chat: &SpaceId) -> Result<()> {
        let _gate = self.agent_queue.lock_admission().await;
        let (id, chat) = (id.to_owned(), chat.to_owned());
        let attached = self
            .store
            .write(move |tx| {
                ensure!(
                    !store::queues_launch_in(tx, &chat)?,
                    "chat {chat} has an agent waiting in the agent queue"
                );
                let attached = tx.execute(
                    "UPDATE agent_queue SET space_id=?2 WHERE id=?1 AND kind=?3
                     AND state IN (?4,?5) AND space_id IS NULL AND opens_workspace_id IS NULL",
                    params![
                        id,
                        chat,
                        QueueKind::Launch,
                        QueueState::Queued,
                        QueueState::Starting
                    ],
                )?;
                if attached == 1 {
                    blockers::replace_launch(tx, &id, &chat)?;
                    super::failures::replace(tx, &chat)?;
                }
                Ok(attached)
            })
            .await?;
        ensure!(
            attached == 1,
            "queue entry no longer starts or waits, or starts its agent in another space"
        );
        Ok(())
    }
}
