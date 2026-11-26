//! Launches their caller runs itself, which the queue keeps for the caller
//! while they wait.
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};

use crate::Agents;
use crate::daemon::Daemon;
use crate::model::QueueEntry;
use crate::state::QueueKind;
use crate::state::QueueState;
use shoal_ids::queue_entry_ids::QueueEntryId;

impl Agents {
    /// Start the launch `id`, which its caller held while its workspace was
    /// prepared, in the caller. It starts at once when its start asked for that
    /// and nothing blocks it any longer, else when the limits admit it now. One
    /// the limits do not admit waits for the queue, which starts it once the
    /// caller lets go.
    pub async fn queue_claim(&self, daemon: &dyn Daemon, id: &QueueEntryId) -> Result<QueueEntry> {
        let _gate = self.agent_queue.lock_admission().await;
        // The caller holds it now, and once it lets go the queue takes it.
        self.agent_queue.end_attach_keep(id);
        let entry = self.selected_entry(daemon, id.as_str(), None).await?;
        ensure!(
            entry.kind == QueueKind::Launch,
            "{} is not a launch, so only the agent queue starts it",
            entry.name
        );
        if entry.state != QueueState::Queued {
            return Ok(entry);
        }
        if self.starts_released_at_once(id).await? {
            return self.start_waiting(daemon, entry, true).await;
        }
        if !self.admit_waiting(daemon, Some(id)).await? {
            return self.selected_entry(daemon, id.as_str(), None).await;
        }
        Ok(QueueEntry {
            state: QueueState::Starting,
            position: None,
            ..entry
        })
    }

    /// Whether the waiting launch `id` asked to start at once and nothing
    /// blocks it any longer.
    async fn starts_released_at_once(&self, id: &QueueEntryId) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .read(move |tx| {
                let released = tx
                    .query_row(
                        "SELECT starts_at_once AND NOT EXISTS (SELECT 1 FROM agent_queue_blockers b
                            WHERE b.entry_id=agent_queue.id)
                         FROM agent_queue WHERE id=?1 AND state=?2",
                        params![id, QueueState::Queued],
                        |row| row.get(0),
                    )
                    .optional()?;
                Ok(released.unwrap_or(false))
            })
            .await
    }
}
