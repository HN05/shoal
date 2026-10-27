//! Restores of stopped agents that the daemon starts, as it starts launches
//! in existing workspaces: `shoal resume` queues them, and so does a signal
//! that ended an agent's session, through the agent's recovery record. Each
//! is admitted like a launch and leaves the queue once its agent registers,
//! or while waiting once its record is gone, as after `shoal resume --now` or
//! a discard.
use std::collections::{BTreeMap, HashSet};

use anyhow::{Context as _, Result, ensure};
use rusqlite::{Connection, params};

use crate::{
    daemon::workspace::Manager,
    execution::recovery,
    model::QueueEntry,
    paths::Paths,
    state::{QueueKind, QueueState},
};

/// The workspace a stopped agent restores in.
#[derive(Clone)]
struct Target {
    id: String,
    repository: String,
    name: String,
}

/// A stopped agent's restore to record.
struct Restore {
    id: String,
    target: Target,
    agent: String,
    execution: String,
    env: String,
}

impl Restore {
    fn new(
        target: Target,
        execution: &str,
        agent: &str,
        env: &BTreeMap<String, String>,
    ) -> Result<Self> {
        Ok(Self {
            id: uuid::Uuid::new_v4().to_string(),
            target,
            agent: agent.to_owned(),
            execution: execution.to_owned(),
            env: serde_json::to_string(env)?,
        })
    }
}

/// Record `restore` behind the work already waiting, unless its execution
/// already has one. Its workspace must be ready and its execution ended,
/// since a recorded one runs, waits for its wrapper's restore or is unresolved.
fn insert(db: &mut Connection, restore: &Restore) -> Result<()> {
    let tx = db.transaction()?;
    crate::daemon::store::require_ready(&tx, &restore.target.id)
        .context("restore agents in a ready workspace")?;
    ensure!(
        !crate::daemon::store::exists(
            &tx,
            "SELECT 1 FROM executions WHERE id=?1",
            [&restore.execution],
        )?,
        "execution {} has not ended; stop it with shoal stop {} first",
        restore.execution,
        restore.target.name
    );
    let now = crate::time::unix_seconds() as i64;
    tx.execute(
        "INSERT INTO agent_queue(id,kind,repository_id,name,agent,env,execution_id,state,created_at,seen_at,workspace_id)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?10)
         ON CONFLICT(execution_id) DO NOTHING",
        params![
            restore.id,
            QueueKind::Restore,
            restore.target.repository,
            restore.target.name,
            restore.agent,
            restore.env,
            restore.execution,
            QueueState::Queued,
            now,
            restore.target.id,
        ],
    )?;
    tx.commit()?;
    Ok(())
}

/// The ready workspaces whose assignment is not done, where stopped agents
/// restore on their own.
fn open_workspaces(db: &Connection) -> Result<Vec<Target>> {
    Ok(db
        .prepare(
            "SELECT id,repository_id,name FROM workspaces w WHERE state='ready' AND NOT EXISTS
                (SELECT 1 FROM workspace_completion c WHERE c.workspace_id=w.id)",
        )?
        .query_map([], |row| {
            Ok(Target {
                id: row.get(0)?,
                repository: row.get(1)?,
                name: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// The recorded executions, which have not ended.
fn recorded_executions(db: &Connection) -> Result<HashSet<String>> {
    Ok(db
        .prepare("SELECT id FROM executions")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Each waiting restore the daemon starts: its ID, workspace and execution.
fn waiting_restores(db: &Connection) -> Result<Vec<(String, String, String)>> {
    Ok(db
        .prepare(
            "SELECT id,workspace_id,execution_id FROM agent_queue
             WHERE kind=?1 AND env IS NOT NULL AND state=?2",
        )?
        .query_map(params![QueueKind::Restore, QueueState::Queued], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// The IDs of the `waiting` restores whose recovery record is gone. A record
/// whose existence cannot be read counts as kept.
fn resolved_restores(paths: &Paths, waiting: Vec<(String, String, String)>) -> Vec<String> {
    waiting
        .into_iter()
        .filter(|(_, workspace, execution)| {
            let path = recovery::record_path(paths, workspace, execution);
            !path.try_exists().unwrap_or(true)
        })
        .map(|(id, _, _)| id)
        .collect()
}

/// Delete the entries `ids` that still wait; a starting one belongs to its agent.
fn delete_waiting(db: &Connection, ids: &[String]) -> Result<()> {
    for id in ids {
        db.execute(
            "DELETE FROM agent_queue WHERE id=?1 AND state=?2",
            params![id, QueueState::Queued],
        )?;
    }
    Ok(())
}

impl Manager {
    /// Queue the restore of the agent stopped as `execution` in `workspace`,
    /// which the daemon starts with `env` once admitted, and return its
    /// entry. Queueing it again keeps its place.
    pub async fn queue_stopped_restore(
        &self,
        workspace: &str,
        execution: &str,
        env: BTreeMap<String, String>,
    ) -> Result<QueueEntry> {
        let workspace = self.workspace(workspace).await?;
        let path = recovery::record_path(&self.paths, &workspace.id, execution);
        let record = recovery::Record::load(&path)?
            .with_context(|| format!("{} has no stopped agent {execution}", workspace.name))?;
        let target = Target {
            id: workspace.id,
            repository: workspace.repository_id,
            name: workspace.name,
        };
        let restore = Restore::new(target, execution, &record.agent, &env)?;
        self.store.run(move |db| insert(db, &restore)).await?;
        self.queue_entries()
            .await?
            .into_iter()
            .find(|entry| entry.execution.as_deref() == Some(execution))
            .context("the restore already left the agent queue")
    }

    /// Queue the restores that stopped agents' records request once their
    /// execution has ended. A done workspace's agent waits until
    /// `shoal resume` names its workspace.
    pub(super) async fn queue_requested_restores(&self) -> Result<()> {
        let (workspaces, recorded) = self
            .store
            .run(|db| Ok((open_workspaces(db)?, recorded_executions(db)?)))
            .await?;
        for workspace in workspaces {
            for (execution, record) in recovery::restore_requests(&self.paths, &workspace.id)? {
                if !recorded.contains(&execution) {
                    self.queue_requested_restore(&workspace, &execution, record)
                        .await?;
                }
            }
        }
        Ok(())
    }

    /// Queue the restore `record` requests for `execution` in `workspace`,
    /// then clear the request. One that cannot be queued keeps its request
    /// without holding back the others.
    async fn queue_requested_restore(
        &self,
        workspace: &Target,
        execution: &str,
        record: recovery::Record,
    ) -> Result<()> {
        let env = record.restore_request.unwrap_or_default();
        let restore = Restore::new(workspace.clone(), execution, &record.agent, &env)?;
        if let Err(error) = self.store.run(move |db| insert(db, &restore)).await {
            crate::daemon::log!("agent queue: cannot queue restore of {execution}: {error:#}");
            return Ok(());
        }
        recovery::clear_restore_request(&self.paths, &workspace.id, execution)
    }

    /// Forget the waiting restores whose stopped agent's record is gone, as
    /// after `shoal resume --now` or a discard.
    pub(super) async fn forget_resolved_restores(&self) -> Result<()> {
        let waiting = self.store.run(|db| waiting_restores(db)).await?;
        let resolved = resolved_restores(&self.paths, waiting);
        if resolved.is_empty() {
            return Ok(());
        }
        self.store
            .run(move |db| delete_waiting(db, &resolved))
            .await
    }

    /// Cancel the restores of `workspace_id`'s stopped agents, waiting or
    /// requested, which stay stopped until `shoal resume`. The queue gate
    /// keeps dispatch from queueing a request meanwhile.
    pub(crate) async fn cancel_stopped_restores(&self, workspace_id: &str) -> Result<()> {
        let _gate = self.agent_queue_gate.lock().await;
        for (execution, _) in recovery::restore_requests(&self.paths, workspace_id)? {
            recovery::clear_restore_request(&self.paths, workspace_id, &execution)?;
        }
        let workspace_id = workspace_id.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "DELETE FROM agent_queue
                     WHERE workspace_id=?1 AND kind=?2 AND env IS NOT NULL AND state=?3",
                    params![workspace_id, QueueKind::Restore, QueueState::Queued],
                )?;
                Ok(())
            })
            .await
    }

    /// Whether the entry `id` restores a stopped agent the daemon starts.
    pub(super) async fn restores_stopped_agent(&self, id: &str) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                crate::daemon::store::exists(
                    db,
                    "SELECT 1 FROM agent_queue WHERE id=?1 AND kind=?2 AND env IS NOT NULL",
                    params![id, QueueKind::Restore],
                )
            })
            .await
    }
}

#[cfg(test)]
mod tests;
