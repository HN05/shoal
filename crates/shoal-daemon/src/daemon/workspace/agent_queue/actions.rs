//! What `shoal queue` does to a waiting entry: start it now or cancel it,
//! and the cancelling of the launches a stopped workspace waits for. Each holds the queue gate, so dispatch cannot start
//! the entry meanwhile.
use crate::client::execution_ids::ExecutionId;
use crate::client::repository_ids::RepositoryId;
use crate::client::{queue::QueueEntryId, space_ids::SpaceId};
use std::collections::{HashMap, HashSet};

use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::params;

use super::{
    Grant,
    blockers::Readiness,
    store::{ENTRY_COLUMNS, entry as queue_entry},
};
use crate::{
    daemon::{
        scope::Caller,
        workspace::{Manager, StopRequest},
    },
    model::{QueueEntry, QueuedJob},
    state::{QueueKind, QueueState},
};

impl Manager {
    /// The entry with the ID `selector` among those `caller` may act on, if
    /// one has it.
    async fn find_entry(
        &self,
        selector: &str,
        caller: Option<&Caller>,
    ) -> Result<Option<QueueEntry>> {
        let entries = self.reachable_queue(caller).await?;
        Ok(entries.into_iter().find(|entry| entry.id == selector))
    }

    async fn selected_entry(&self, selector: &str, caller: Option<&Caller>) -> Result<QueueEntry> {
        self.find_entry(selector, caller)
            .await?
            .with_context(|| format!("{selector} is not in the agent queue"))
    }

    /// The waiting entry `selector` names; `starting` says what to do with
    /// one that already starts.
    async fn waiting_entry(
        &self,
        selector: &str,
        caller: Option<&Caller>,
        starting: &str,
    ) -> Result<QueueEntry> {
        let entry = self.selected_entry(selector, caller).await?;
        ensure!(
            entry.state == QueueState::Queued,
            "{} is already starting{starting}",
            entry.name
        );
        Ok(entry)
    }

    /// The execution of the stopped agent whose wrapper waits for `entry`,
    /// a restore the daemon does not start itself.
    async fn waiting_wrapper(&self, entry: &QueueEntry) -> Result<Option<ExecutionId>> {
        if entry.kind != QueueKind::Restore || self.restores_stopped_agent(&entry.id).await? {
            return Ok(None);
        }
        entry
            .execution
            .clone()
            .context("the queued restore names no stopped agent")
            .map(Some)
    }

    /// Start the waiting entry `selector` names now, regardless of the
    /// limits, or for `here` mark the launch it names starting for the
    /// caller, which starts it in its own process. A launch still waiting
    /// for other work needs that work's branch, so it is refused.
    pub async fn queue_start(&self, selector: &str, here: bool) -> Result<QueueEntry> {
        let _gate = self.agent_queue.lock_admission().await;
        let entry = self.waiting_entry(selector, None, "").await?;
        ensure!(
            !here || entry.kind == QueueKind::Launch,
            "{} is not a launch, so only the agent queue starts it",
            entry.name
        );
        self.start_waiting(entry, here).await
    }

    /// Start the waiting `entry` now, regardless of the limits, as
    /// [`Manager::queue_start`] does, leaving a launch to the caller when
    /// `here`. Requires the queue gate.
    pub(super) async fn start_waiting(
        &self,
        mut entry: QueueEntry,
        here: bool,
    ) -> Result<QueueEntry> {
        let base = self.start_base(&entry).await?;
        match self.waiting_wrapper(&entry).await? {
            Some(execution) => self.grant_now(&entry, &execution).await?,
            None => {
                let bases = base.map(|base| (entry.id.clone(), base));
                let starts = self
                    .mark_starting(vec![entry.id.clone()], bases.into_iter().collect())
                    .await?;
                let caller = here.then_some(&entry.id);
                self.start_launches(starts, caller).await?;
                if entry.kind == QueueKind::Job {
                    self.runners.wake();
                }
            }
        }
        entry.state = QueueState::Starting;
        entry.position = None;
        Ok(entry)
    }

    /// The branch a done blocker gives `entry` to build on, refusing one
    /// that something still blocks.
    async fn start_base(&self, entry: &QueueEntry) -> Result<Option<String>> {
        match self.blocker_readiness().await?.remove(&entry.id) {
            None => Ok(None),
            Some(Readiness::Ready { base }) => Ok(base),
            Some(Readiness::Waiting(blocked_by)) => bail!(
                "{} is blocked by {} and builds on its branch, so it cannot start before that work is complete",
                entry.name,
                blocked_by.join(", ")
            ),
        }
    }

    /// Admit the restore `entry` of `execution`, whose wrapper restores its
    /// agent once every process of it is proven stopped.
    async fn grant_now(&self, entry: &QueueEntry, execution: &ExecutionId) -> Result<()> {
        ensure!(
            self.agent_queue.restore_waiters().contains(execution),
            "{} is still stopping; start it once its processes have stopped",
            entry.name
        );
        // The wrapper reads its grant once it sees the entry starting.
        self.agent_queue.grant_restore(execution, Grant::Queue);
        self.mark_starting(vec![entry.id.clone()], HashMap::new())
            .await?;
        Ok(())
    }

    /// Cancel the waiting entry `selector` names. A cancelled restore leaves
    /// its agent stopped for `shoal restore`; a waiting wrapper is stopped as
    /// `shoal stop` stops it.
    pub async fn queue_remove(
        &self,
        selector: &str,
        caller: Option<&Caller>,
    ) -> Result<QueueEntry> {
        let _gate = self.agent_queue.lock_admission().await;
        let entry = self
            .waiting_entry(selector, caller, "; remove its workspace instead")
            .await?;
        let wrapper = self.waiting_wrapper(&entry).await?;
        if let Some(execution) = &wrapper {
            self.stop_waiting_wrapper(&entry, execution).await?;
        }
        // A stopped wrapper removes its entry as it finishes, which may come first.
        let deleted = match &entry.job {
            Some(job) => (self.pass_over_job(&entry.id, entry.job_repository()?, job)).await?,
            None => self.delete_waiting_entry(entry.id.clone()).await?,
        };
        ensure!(
            deleted || wrapper.is_some(),
            "{} is no longer waiting",
            entry.name
        );
        Ok(entry)
    }

    /// Cancel the launches waiting to open `workspace_id`, returning them.
    /// One already starting belongs to its agent.
    pub(crate) async fn cancel_waiting_launches(
        &self,
        workspace_id: &SpaceId,
    ) -> Result<Vec<QueueEntry>> {
        let _gate = self.agent_queue.lock_admission().await;
        let workspace_id = workspace_id.to_owned();
        self.store
            .write(move |tx| delete_waiting_launches(tx, &workspace_id))
            .await
    }

    /// The workspaces a waiting launch opens. Requires the queue gate, which
    /// keeps them waiting until it is released.
    pub(in crate::daemon::workspace) async fn waiting_launch_workspaces(
        &self,
    ) -> Result<HashSet<SpaceId>> {
        self.store
            .run(|db| {
                Ok(db
                    .prepare(
                        "SELECT DISTINCT opens_workspace_id FROM agent_queue
                         WHERE opens_workspace_id IS NOT NULL AND kind=?1 AND state=?2",
                    )?
                    .query_map(params![QueueKind::Launch, QueueState::Queued], |row| {
                        row.get(0)
                    })?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// Stop the wrapper of `execution` waiting for the restore `entry`, as
    /// `shoal stop` stops it, leaving its recovery record. A disconnected
    /// wrapper would queue the restore again once it reattaches, so it is
    /// refused.
    async fn stop_waiting_wrapper(
        &self,
        entry: &QueueEntry,
        execution: &ExecutionId,
    ) -> Result<()> {
        let stopped = self
            .executions
            .connections()
            .await
            .stop(execution, &StopRequest::queue_cancel())
            .await?;
        if stopped {
            self.forget_agent_stop(execution).await;
        }
        // Names select nothing; a restore's workspace is known.
        let workspace = (entry.space()).map_or_else(|| entry.id.to_string(), ToString::to_string);
        ensure!(
            stopped,
            "the agent of {} waits to reattach to the daemon; cancel its restore once it has, or stop it with shoal stop {workspace}",
            entry.name
        );
        Ok(())
    }

    /// Delete the entry `id` if it still waits, returning whether it did.
    async fn delete_waiting_entry(&self, id: QueueEntryId) -> Result<bool> {
        self.store.run(move |db| delete_waiting(db, &id)).await
    }

    /// Delete `repository`'s job entry `id` if it still waits, leaving its
    /// `job` to other runners, returning whether it still waited.
    pub(in crate::daemon::workspace) async fn pass_over_job(
        &self,
        id: &QueueEntryId,
        repository: &RepositoryId,
        job: &QueuedJob,
    ) -> Result<bool> {
        let (id, repository, handle) =
            (id.to_owned(), repository.to_owned(), job.job.handle.clone());
        self.store
            .write(move |tx| {
                let deleted = delete_waiting(tx, &id)?;
                if deleted {
                    tx.execute(
                        "INSERT OR IGNORE INTO ci_passed_jobs(repository_id,handle) VALUES (?1,?2)",
                        params![repository, handle],
                    )?;
                }
                Ok(deleted)
            })
            .await
    }
}

/// Delete the entry `id` if it still waits, returning whether it did.
fn delete_waiting(db: &rusqlite::Connection, id: &QueueEntryId) -> Result<bool> {
    let deleted = db.execute(
        "DELETE FROM agent_queue WHERE id=?1 AND state=?2",
        params![id, QueueState::Queued],
    )?;
    Ok(deleted == 1)
}

/// Delete the launches waiting to open `workspace_id`, returning them in
/// queue order.
fn delete_waiting_launches(
    tx: &rusqlite::Transaction<'_>,
    workspace_id: &SpaceId,
) -> Result<Vec<QueueEntry>> {
    const WAITING: &str = "FROM agent_queue WHERE opens_workspace_id=?1 AND kind=?2 AND state=?3";
    let selection = params![workspace_id, QueueKind::Launch, QueueState::Queued];
    let launches = tx
        .prepare(&format!("SELECT {ENTRY_COLUMNS} {WAITING} ORDER BY seq"))?
        .query_map(selection, queue_entry)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    tx.execute(&format!("DELETE {WAITING}"), selection)?;
    Ok(launches)
}

#[cfg(test)]
mod tests;
