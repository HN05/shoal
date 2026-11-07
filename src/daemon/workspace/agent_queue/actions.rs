//! What `shoal queue` does to a waiting entry: start it now, cancel it, or
//! move it to another place, and the cancelling of the launches a stopped
//! workspace waits for. Each holds the queue gate, so dispatch cannot start
//! the entry meanwhile.
use std::collections::{HashMap, HashSet};

use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::params;

use super::{
    ENTRY_COLUMNS, Grant, WaitingRestore, dependencies::Readiness, entry as queue_entry, order,
    record_waiting_restore,
};
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        scope::Caller,
        workspace::{Manager, StopRequest},
    },
    model::{MovedEntry, QueueEntry, QueuePlace, QueuedJob},
    state::{QueueKind, QueueState},
};

impl Manager {
    /// The entry `selector` names by ID or name among those `caller` may
    /// see, if one does.
    async fn find_entry(
        &self,
        selector: &str,
        caller: Option<&Caller>,
    ) -> Result<Option<QueueEntry>> {
        let entries = self.visible_queue(caller).await?;
        let mut matches = entries
            .into_iter()
            .filter(|entry| entry.id == selector || entry.name == selector);
        let entry = matches.next();
        ensure!(
            matches.next().is_none(),
            "several queue entries are named {selector}; pass the entry ID"
        );
        Ok(entry)
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
    async fn waiting_wrapper(&self, entry: &QueueEntry) -> Result<Option<String>> {
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
    /// limits. A launch still waiting for other work needs that work's
    /// branch, so it is refused.
    pub async fn queue_start(&self, selector: &str) -> Result<QueueEntry> {
        let _gate = self.agent_queue.lock_admission().await;
        let entry = self.waiting_entry(selector, None, "").await?;
        self.start_waiting(entry).await
    }

    /// Start the waiting `entry` now, regardless of the limits, as
    /// [`Manager::queue_start`] does. Requires the queue gate.
    pub(super) async fn start_waiting(&self, mut entry: QueueEntry) -> Result<QueueEntry> {
        let base = self.start_base(&entry).await?;
        match self.waiting_wrapper(&entry).await? {
            Some(execution) => self.grant_now(&entry, &execution).await?,
            None => {
                let bases = base.map(|base| (entry.id.clone(), base));
                let starts = self
                    .mark_starting(vec![entry.id.clone()], bases.into_iter().collect())
                    .await?;
                self.start_launches(starts, None).await?;
                if entry.kind == QueueKind::Job {
                    self.runners.wake();
                }
            }
        }
        entry.state = QueueState::Starting;
        entry.position = None;
        Ok(entry)
    }

    /// The branch a done dependency gives `entry` to build on, refusing one
    /// that still waits for work.
    async fn start_base(&self, entry: &QueueEntry) -> Result<Option<String>> {
        match self.dependency_readiness().await?.remove(&entry.id) {
            None => Ok(None),
            Some(Readiness::Ready { base }) => Ok(base),
            Some(Readiness::Waiting(work)) => bail!(
                "{} waits for {} and builds on its branch, so it cannot start before that work is complete",
                entry.name,
                work.join(", ")
            ),
        }
    }

    /// Admit the restore `entry` of `execution`, whose wrapper restores its
    /// agent once every process of it is proven stopped.
    async fn grant_now(&self, entry: &QueueEntry, execution: &str) -> Result<()> {
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
    /// its agent stopped for `shoal resume`; a waiting wrapper is stopped as
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
            Some(job) => self.pass_over_job(&entry, job).await?,
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
        workspace_id: &str,
    ) -> Result<Vec<QueueEntry>> {
        let _gate = self.agent_queue.lock_admission().await;
        let workspace_id = workspace_id.to_owned();
        self.store
            .run(move |db| delete_waiting_launches(db, &workspace_id))
            .await
    }

    /// The workspaces a waiting launch opens. Requires the queue gate, which
    /// keeps them waiting until it is released.
    pub(in crate::daemon::workspace) async fn waiting_launch_workspaces(
        &self,
    ) -> Result<HashSet<String>> {
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
    async fn stop_waiting_wrapper(&self, entry: &QueueEntry, execution: &str) -> Result<()> {
        let stopped = self
            .executions
            .connections()
            .await
            .stop(execution, &StopRequest::queue_cancel(), false)
            .await?;
        ensure!(
            stopped,
            "the agent of {0} waits to reattach to the daemon; cancel its restore once it has, or stop it with shoal stop {0}",
            entry.name
        );
        Ok(())
    }

    /// Delete the waiting job `entry`, leaving its `job` to other runners,
    /// returning whether it still waited.
    async fn pass_over_job(&self, entry: &QueueEntry, job: &QueuedJob) -> Result<bool> {
        let (id, repository, handle) = (
            entry.id.clone(),
            entry.repository_id.clone(),
            job.job.handle.clone(),
        );
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                let deleted = delete_waiting(&tx, &id)?;
                if deleted {
                    tx.execute(
                        "INSERT OR IGNORE INTO ci_passed_jobs(repository_id,handle) VALUES (?1,?2)",
                        params![repository, handle],
                    )?;
                }
                tx.commit()?;
                Ok(deleted)
            })
            .await
    }

    /// Delete the entry `id` if it still waits, returning whether it did.
    async fn delete_waiting_entry(&self, id: String) -> Result<bool> {
        self.store.run(move |db| delete_waiting(db, &id)).await
    }

    /// Move the waiting entry `selector` names to `place`, or stop the
    /// agent running in the workspace it names and place its restore there.
    /// Scoped callers may only move an entry back, never ahead of others or
    /// a running agent.
    pub async fn queue_move(
        &self,
        selector: &str,
        place: QueuePlace,
        caller: Option<&Caller>,
    ) -> Result<MovedEntry> {
        let _gate = self.agent_queue.lock_admission().await;
        let entry = match self.find_entry(selector, caller).await? {
            Some(entry) => entry,
            None if caller.is_some() => bail!("{selector} is not in the agent queue"),
            None => return self.move_running_agent(&selector.parse()?, place).await,
        };
        ensure!(
            entry.state == QueueState::Queued,
            "{} is already starting",
            entry.name
        );
        let position = self
            .place_entry(entry.id.clone(), place, caller.is_some())
            .await?;
        self.agent_queue.wake();
        Ok(MovedEntry {
            entry: QueueEntry {
                position: Some(position),
                ..entry
            },
            stopped: false,
        })
    }

    /// Record `restore`, which no entry has yet, at `place`, returning its
    /// entry's ID.
    async fn place_restore(&self, restore: WaitingRestore, place: QueuePlace) -> Result<String> {
        self.store
            .run(move |db| record_placed_restore(db, &restore, &place))
            .await
    }

    /// Place the waiting entry `id` at `place`, only further back when
    /// `back_only`, returning its new position.
    async fn place_entry(&self, id: String, place: QueuePlace, back_only: bool) -> Result<usize> {
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                let moved = order::place(&tx, &id, &place)?;
                ensure!(
                    !back_only || moved.to >= moved.from,
                    "workspace processes can only move queue entries back"
                );
                tx.commit()?;
                Ok(moved.to)
            })
            .await
    }

    /// Stop the agent running in the workspace `selector` names and place
    /// its restore, which its wrapper waits for, at `place`. The place is
    /// taken before the stop, so a place that cannot be taken stops nothing;
    /// the wrapper's own queueing then keeps it.
    async fn move_running_agent(
        &self,
        selector: &SpaceSelector,
        place: QueuePlace,
    ) -> Result<MovedEntry> {
        let workspace = self
            .workspace(selector)
            .await
            .with_context(|| format!("{selector} is not in the agent queue"))?;
        let execution = self.movable_agent(&workspace).await?;
        let agent = self
            .agent_name(&execution)
            .await
            .context("the agent disconnected")?;
        let restore = WaitingRestore {
            workspace,
            execution: execution.clone(),
            agent,
        };
        let id = self.place_restore(restore, place).await?;
        if let Err(error) = self.stop_for_queue_move(&execution).await {
            self.delete_waiting_entry(id).await?;
            return Err(error);
        }
        let entry = self
            .queue_entries()
            .await?
            .into_iter()
            .find(|entry| entry.id == id)
            .context("the restore already left the agent queue")?;
        Ok(MovedEntry {
            entry,
            stopped: true,
        })
    }
}

/// Delete the entry `id` if it still waits, returning whether it did.
fn delete_waiting(db: &rusqlite::Connection, id: &str) -> Result<bool> {
    let deleted = db.execute(
        "DELETE FROM agent_queue WHERE id=?1 AND state=?2",
        params![id, QueueState::Queued],
    )?;
    Ok(deleted == 1)
}

/// Delete the launches waiting to open `workspace_id`, returning them in
/// queue order.
fn delete_waiting_launches(
    db: &mut rusqlite::Connection,
    workspace_id: &str,
) -> Result<Vec<QueueEntry>> {
    const WAITING: &str = "FROM agent_queue WHERE opens_workspace_id=?1 AND kind=?2 AND state=?3";
    let selection = params![workspace_id, QueueKind::Launch, QueueState::Queued];
    let tx = db.transaction()?;
    let launches = tx
        .prepare(&format!("SELECT {ENTRY_COLUMNS} {WAITING} ORDER BY seq"))?
        .query_map(selection, queue_entry)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    tx.execute(&format!("DELETE {WAITING}"), selection)?;
    tx.commit()?;
    Ok(launches)
}

/// Record `restore` at `place` in one transaction, refusing an agent whose
/// restore already has an entry.
fn record_placed_restore(
    db: &mut rusqlite::Connection,
    restore: &WaitingRestore,
    place: &QueuePlace,
) -> Result<String> {
    let tx = db.transaction()?;
    ensure!(
        !crate::daemon::store::exists(
            &tx,
            "SELECT 1 FROM agent_queue WHERE execution_id=?1",
            [&restore.execution],
        )?,
        "the restore of {} is already in the agent queue",
        restore.workspace.name
    );
    let id = record_waiting_restore(&tx, restore)?;
    order::place(&tx, &id, place)?;
    tx.commit()?;
    Ok(id)
}

#[cfg(test)]
mod tests;
