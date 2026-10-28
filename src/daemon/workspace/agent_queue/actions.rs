//! What `shoal queue` does to a waiting entry: start it now, cancel it, or
//! move it to another place. Each holds the queue gate, so dispatch cannot
//! start the entry meanwhile.
use std::collections::HashMap;

use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::params;

use super::{Grant, WaitingRestore, dependencies::Readiness, order, record_waiting_restore};
use crate::{
    daemon::{
        scope::Caller,
        workspace::{Manager, StopRequest},
    },
    model::{MovedEntry, QueueEntry, QueuePlace},
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
        if entry.kind == QueueKind::Launch || self.restores_stopped_agent(&entry.id).await? {
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
        let mut entry = self.waiting_entry(selector, None, "").await?;
        let base = self.start_base(&entry).await?;
        match self.waiting_wrapper(&entry).await? {
            Some(execution) => self.grant_now(&entry, &execution).await?,
            None => {
                let bases = base.map(|base| (entry.id.clone(), base));
                let starts = self
                    .mark_starting(vec![entry.id.clone()], bases.into_iter().collect())
                    .await?;
                self.start_launches(starts, None).await?;
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
        self.mark_starting(vec![entry.id.clone()], HashMap::new())
            .await?;
        self.agent_queue.grant_restore(execution, Grant::Queue);
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
        if let Some(execution) = self.waiting_wrapper(&entry).await? {
            self.executions
                .connections()
                .await
                .stop(&execution, &StopRequest::queue_cancel(), false)
                .await?;
        }
        let id = entry.id.clone();
        let removed = self
            .store
            .run(move |db| {
                Ok(db.execute(
                    "DELETE FROM agent_queue WHERE id=?1 AND state=?2",
                    params![id, QueueState::Queued],
                )?)
            })
            .await?;
        ensure!(removed == 1, "{} is no longer waiting", entry.name);
        Ok(entry)
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
            None => return self.move_running_agent(selector, place).await,
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
    /// its restore, which its wrapper waits for, at `place`. The wrapper may
    /// already have queued it, which keeps its entry.
    async fn move_running_agent(&self, selector: &str, place: QueuePlace) -> Result<MovedEntry> {
        let workspace = self
            .workspace(selector)
            .await
            .with_context(|| format!("{selector} is not in the agent queue"))?;
        let execution = self.movable_agent(&workspace).await?;
        let agent = self
            .agent_name(&execution)
            .await
            .context("the agent disconnected")?;
        self.stop_for_queue_move(&execution).await?;
        let restore = WaitingRestore {
            workspace,
            execution,
            agent,
        };
        let id = self
            .store
            .run(move |db| {
                let tx = db.transaction()?;
                let id = record_waiting_restore(&tx, &restore)?;
                order::place(&tx, &id, &place)?;
                tx.commit()?;
                Ok(id)
            })
            .await?;
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

#[cfg(test)]
mod tests;
