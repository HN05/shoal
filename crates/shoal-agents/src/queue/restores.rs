//! Restores a stopped agent's waiting wrapper carries out: queuing one,
//! waiting over the wrapper's link for its admission, and keeping, returning
//! or removing its entry.
use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

use super::{
    Grant, RestoreLink,
    store::{WaitingRestore, record_waiting_restore},
};
use crate::Agents;
use crate::daemon::Daemon;
use crate::state::QueueState;

impl Agents {
    /// Queue the automatic restore of `execution`, an agent that protection
    /// stopped in the space `id`, behind the work already waiting. A wrapper
    /// that reattached while waiting keeps its place and waits again.
    pub async fn queue_restore(
        &self,
        daemon: &dyn Daemon,
        execution: &ExecutionId,
        agent: &crate::agent::Agent,
        id: &SpaceId,
    ) -> Result<()> {
        let restore = WaitingRestore {
            space: daemon.space(&SpaceSelector::id(id)).await?,
            execution: execution.to_owned(),
            agent: agent.to_owned(),
        };
        self.store
            .run(move |db| record_waiting_restore(db, &restore).map(|_| ()))
            .await
    }

    /// Wait until admission hands the queued restore to the wrapper of
    /// `link`, whose agent waits with every process stopped, and return what
    /// admitted it.
    pub async fn await_restore_admission(&self, link: &RestoreLink<'_>) -> Result<Grant> {
        let mut changed = self.store.watch_changes();
        let execution = link.execution();
        loop {
            let id = execution.to_owned();
            let state = self
                .store
                .run(move |db| {
                    Ok(db
                        .query_row(
                            "SELECT state FROM agent_queue WHERE execution_id=?1",
                            [id],
                            |row| row.get(0),
                        )
                        .optional()?)
                })
                .await?;
            match state {
                Some(QueueState::Starting) => {
                    return self
                        .agent_queue
                        .restore_grant(execution)
                        .context("the restore started without admission");
                }
                Some(QueueState::Queued) => changed.changed().await?,
                Some(QueueState::Failed) | None => {
                    bail!("the restore is no longer in the agent queue")
                }
            }
        }
    }

    /// Return `execution`'s revoked restore to its place in the queue.
    pub async fn requeue_restore(&self, execution: &ExecutionId) -> Result<()> {
        self.agent_queue.revoke_restore(execution);
        let execution = execution.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE agent_queue SET state=?2 WHERE execution_id=?1",
                    params![execution, QueueState::Queued],
                )?;
                Ok(())
            })
            .await
    }

    /// Take `execution`'s restore out of the queue, once its agent registered
    /// again or the restore ended without it.
    pub async fn remove_restore(&self, execution: &ExecutionId) {
        let execution = execution.to_owned();
        let removed = self
            .store
            .run(move |db| {
                db.execute("DELETE FROM agent_queue WHERE execution_id=?1", [execution])?;
                Ok(())
            })
            .await;
        if let Err(error) = removed {
            crate::log!("agent queue: {error:#}");
        }
    }
}
