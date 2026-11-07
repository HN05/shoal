//! Restores a stopped agent's waiting wrapper carries out: queuing one,
//! waiting for its admission, and keeping, returning or removing its entry.
use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};

use super::{
    Grant,
    store::{WaitingRestore, record_waiting_restore},
};
use crate::client::time::UnixTime;
use crate::{daemon::workspace::Manager, state::QueueState};

impl Manager {
    /// Queue the automatic restore of `execution`, an agent that protection
    /// stopped in `workspace_id`, behind the work already waiting. A wrapper
    /// that reattached while waiting keeps its place and waits again.
    pub(crate) async fn queue_restore(
        &self,
        execution: &str,
        agent: &str,
        workspace_id: &str,
    ) -> Result<()> {
        let restore = WaitingRestore {
            workspace: self.workspace_by_id(workspace_id).await?,
            execution: execution.to_owned(),
            agent: agent.to_owned(),
        };
        self.store
            .run(move |db| record_waiting_restore(db, &restore).map(|_| ()))
            .await
    }

    /// Wait until admission starts the queued restore of `execution`, whose
    /// agent waits with every process stopped, and return what admitted it.
    pub(crate) async fn await_restore_admission(&self, execution: &str) -> Result<Grant> {
        let mut changed = self.store.watch_changes();
        let _waiter = self.agent_queue.wait_for_restore(execution);
        self.agent_queue.wake();
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
                None => bail!("the restore is no longer in the agent queue"),
            }
        }
    }

    /// Return `execution`'s revoked restore to its place in the queue.
    pub(crate) async fn requeue_restore(&self, execution: &str) -> Result<()> {
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

    /// Start the grace period of `execution`'s restore, whose wrapper lost
    /// its connection and may reattach.
    pub(crate) async fn keep_restore_place(&self, execution: &str) {
        let (execution, now) = (execution.to_owned(), UnixTime::now());
        let kept = self
            .store
            .run(move |db| {
                db.execute(
                    "UPDATE agent_queue SET seen_at=?2 WHERE execution_id=?1",
                    params![execution, now],
                )?;
                Ok(())
            })
            .await;
        if let Err(error) = kept {
            crate::daemon::log!("agent queue: {error:#}");
        }
    }

    /// Take `execution`'s restore out of the queue, once its agent registered
    /// again or the restore ended without it.
    pub(crate) async fn remove_restore(&self, execution: &str) {
        let execution = execution.to_owned();
        let removed = self
            .store
            .run(move |db| {
                db.execute("DELETE FROM agent_queue WHERE execution_id=?1", [execution])?;
                Ok(())
            })
            .await;
        if let Err(error) = removed {
            crate::daemon::log!("agent queue: {error:#}");
        }
    }
}
