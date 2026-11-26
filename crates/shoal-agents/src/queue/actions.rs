//! What `shoal queue` does to a waiting entry: start it now or cancel it. Each
//! operation holds the queue gate, so dispatch cannot start the entry
//! meanwhile.
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use std::collections::{HashMap, HashSet};

use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::{OptionalExtension, params};

use super::{
    Grant,
    blockers::Readiness,
    store::{ENTRY_COLUMNS, entry as queue_entry},
};
use crate::Agents;
use crate::daemon::Daemon;
use crate::daemon::Records;
use crate::executions::StopRequest;
use crate::model::QueueEntry;
use crate::state::QueueKind;
use crate::state::QueueState;
use shoal_daemon_base::callers::Caller;

impl Agents {
    /// The entry with the ID `selector` among those `caller` may act on, if
    /// one has it.
    async fn find_entry(
        &self,
        daemon: &dyn Daemon,
        selector: &str,
        caller: Option<&Caller>,
    ) -> Result<Option<QueueEntry>> {
        let entries = daemon.reachable_queue(caller).await?;
        Ok(entries.into_iter().find(|entry| entry.id == selector))
    }

    pub async fn selected_entry(
        &self,
        daemon: &dyn Daemon,
        selector: &str,
        caller: Option<&Caller>,
    ) -> Result<QueueEntry> {
        self.find_entry(daemon, selector, caller)
            .await?
            .with_context(|| format!("{selector} is not in the agent queue"))
    }

    /// The waiting entry `selector` names; `starting` says what to do with
    /// one that already starts.
    pub async fn waiting_entry(
        &self,
        daemon: &dyn Daemon,
        selector: &str,
        caller: Option<&Caller>,
        starting: &str,
    ) -> Result<QueueEntry> {
        let entry = self.selected_entry(daemon, selector, caller).await?;
        ensure!(
            entry.state != QueueState::Failed,
            "the start of {} failed; `shoal resume` queues it again",
            entry.name
        );
        ensure!(
            entry.state == QueueState::Queued,
            "{} is already starting{starting}",
            entry.name
        );
        Ok(entry)
    }

    /// The execution of the stopped agent whose wrapper waits for `entry`,
    /// a restore the daemon does not start itself.
    pub async fn waiting_wrapper(&self, entry: &QueueEntry) -> Result<Option<ExecutionId>> {
        if entry.kind != QueueKind::Restore || self.restores_stopped_agent(&entry.id).await? {
            return Ok(None);
        }
        entry
            .execution
            .clone()
            .context("the queued restore names no stopped agent")
            .map(Some)
    }

    /// Start the waiting restore of `execution` now, regardless of the limits,
    /// as its waiting wrapper asks when the user wakes the agent in its place.
    /// Does nothing once it starts or leaves the queue. Returns `false` while
    /// the agent still stops, before its wrapper waits over its link.
    pub async fn wake_waiting_restore(
        &self,
        daemon: &dyn Daemon,
        execution: &ExecutionId,
    ) -> Result<bool> {
        let _gate = self.agent_queue.lock_admission().await;
        if !self.agent_queue.restore_links().contains(execution) {
            return Ok(false);
        }
        let entries = daemon.reachable_queue(None).await?;
        let waiting = entries.into_iter().find(|entry| {
            entry.kind == QueueKind::Restore
                && entry.state == QueueState::Queued
                && entry.execution.as_ref() == Some(execution)
        });
        if let Some(entry) = waiting {
            self.start_waiting(daemon, entry, false).await?;
        }
        Ok(true)
    }

    /// Start the waiting entry `selector` names now, regardless of the
    /// limits, or for `here` mark the launch it names starting for the
    /// caller, which starts it in its own process. A launch still waiting
    /// for other work needs that work's branch, so it is refused.
    pub async fn queue_start(
        &self,
        daemon: &dyn Daemon,
        selector: &str,
        here: bool,
    ) -> Result<QueueEntry> {
        let _gate = self.agent_queue.lock_admission().await;
        let entry = self.waiting_entry(daemon, selector, None, "").await?;
        ensure!(
            !here || entry.kind == QueueKind::Launch,
            "{} is not a launch, so only the agent queue starts it",
            entry.name
        );
        self.start_waiting(daemon, entry, here).await
    }

    /// Start the waiting `entry` now, regardless of the limits, as
    /// [`Manager::queue_start`] does, leaving a launch to the caller when
    /// `here`. Requires the queue gate.
    pub async fn start_waiting(
        &self,
        daemon: &dyn Daemon,
        mut entry: QueueEntry,
        here: bool,
    ) -> Result<QueueEntry> {
        let base = self.start_base(daemon, &entry).await?;
        match self.waiting_wrapper(&entry).await? {
            Some(execution) => self.grant_now(&entry, &execution).await?,
            None => {
                let bases = base.map(|base| (entry.id.clone(), base));
                let starts = self
                    .mark_starting(vec![entry.id.clone()], bases.into_iter().collect())
                    .await?;
                let caller = here.then_some(&entry.id);
                self.start_launches(daemon, starts, caller).await?;
                if entry.kind == QueueKind::Job {
                    daemon.wake_jobs();
                }
            }
        }
        entry.state = QueueState::Starting;
        entry.position = None;
        Ok(entry)
    }

    /// The branch a done blocker gives `entry` to build on, refusing one
    /// that something still blocks.
    async fn start_base(&self, daemon: &dyn Daemon, entry: &QueueEntry) -> Result<Option<String>> {
        match self.blocker_readiness(daemon).await?.remove(&entry.id) {
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
            self.agent_queue.restore_links().contains(execution),
            "{} is still stopping; start it once its processes have stopped",
            entry.name
        );
        // The wrapper reads its grant once it sees the entry starting.
        self.agent_queue.grant_restore(execution, Grant::Queue);
        self.mark_starting(vec![entry.id.clone()], HashMap::new())
            .await?;
        Ok(())
    }

    /// Cancel the waiting entry `selector` names, or drop a failed launch.
    /// A cancelled restore leaves its agent stopped for `shoal resume`; a
    /// waiting wrapper is stopped as `shoal stop` stops it.
    pub async fn queue_remove(
        &self,
        daemon: &dyn Daemon,
        selector: &str,
        caller: Option<&Caller>,
    ) -> Result<QueueEntry> {
        let _gate = self.agent_queue.lock_admission().await;
        let selected = self.selected_entry(daemon, selector, caller).await?;
        if selected.state == QueueState::Failed {
            let dropped = self.drop_failed_launch(&selected.id).await?;
            ensure!(dropped, "{} is no longer in the agent queue", selected.name);
            return Ok(selected);
        }
        let entry = self
            .waiting_entry(daemon, selector, caller, "; remove its workspace instead")
            .await?;
        let wrapper = self.waiting_wrapper(&entry).await?;
        if let Some(execution) = &wrapper {
            self.stop_waiting_wrapper(&entry, execution).await?;
        }
        // A stopped wrapper removes its entry as it finishes, which may come first.
        let deleted = match &entry.job {
            Some(job) => {
                let repository = entry.job_repository()?;
                (daemon.pass_over_job(&entry.id, repository, job)).await?
            }
            None => self.delete_waiting_entry(entry.id.clone()).await?,
        };
        ensure!(
            deleted || wrapper.is_some(),
            "{} is no longer waiting",
            entry.name
        );
        if let Some(space) = entry.space() {
            daemon.settle_agent(space);
        }
        Ok(entry)
    }

    /// Cancel the launches waiting to open `workspace_id`, returning them.
    /// One already starting belongs to its agent.
    pub async fn cancel_waiting_launches(
        &self,
        daemon: &dyn Daemon,
        workspace_id: &SpaceId,
    ) -> Result<Vec<QueueEntry>> {
        let _gate = self.agent_queue.lock_admission().await;
        let id = workspace_id.to_owned();
        let cancelled = (self.store)
            .write(move |tx| delete_waiting_launches(tx, &id))
            .await?;
        if !cancelled.is_empty() {
            daemon.settle_agent(workspace_id);
        }
        Ok(cancelled)
    }

    /// [`Self::cancel_waiting_launches`] while the preparation of
    /// `workspace_id` waits, recording that it now waits for its stopped agent.
    /// The check and the cancellation share one transaction, so they exclude
    /// the preparation's release, which needs the launch it waits for. `None`
    /// once the preparation no longer waits.
    pub async fn cancel_launches_while_preparation_waits(
        &self,
        daemon: &dyn Daemon,
        workspace_id: &SpaceId,
    ) -> Result<Option<Vec<QueueEntry>>> {
        let _gate = self.agent_queue.lock_admission().await;
        let (id, records) = (workspace_id.to_owned(), self.records.clone());
        let cancelled = (self.store)
            .write(move |tx| {
                if records.preparation_waits(tx, &id)?.is_none() {
                    return Ok(None);
                }
                let cancelled = delete_waiting_launches(tx, &id)?;
                records.note_preparation_stopped(tx, &id)?;
                Ok(Some(cancelled))
            })
            .await?;
        if cancelled
            .as_ref()
            .is_some_and(|cancelled| !cancelled.is_empty())
        {
            daemon.settle_agent(workspace_id);
        }
        Ok(cancelled)
    }

    /// The workspaces a waiting launch opens. Requires the queue gate, which
    /// keeps them waiting until it is released.
    pub async fn waiting_launch_workspaces(&self) -> Result<HashSet<SpaceId>> {
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
    pub async fn stop_waiting_wrapper(
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
    pub async fn delete_waiting_entry(&self, id: QueueEntryId) -> Result<bool> {
        let records = self.records.clone();
        (self.store)
            .write(move |tx| delete_waiting(tx, &*records, &id))
            .await
    }
}

/// Delete the entry `id` if it still waits, returning whether it did. The
/// preparation of the workspace a launch waited to open then waits for its
/// stopped agent.
pub fn delete_waiting(
    db: &rusqlite::Connection,
    records: &dyn Records,
    id: &QueueEntryId,
) -> Result<bool> {
    let opens: Option<SpaceId> = db
        .query_row(
            "SELECT opens_workspace_id FROM agent_queue WHERE id=?1 AND state=?2",
            params![id, QueueState::Queued],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let deleted = db.execute(
        "DELETE FROM agent_queue WHERE id=?1 AND state=?2",
        params![id, QueueState::Queued],
    )?;
    if let (1, Some(workspace)) = (deleted, &opens) {
        records.note_preparation_stopped(db, workspace)?;
    }
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
