//! Restores of stopped agents that the daemon starts, as it starts launches
//! in existing workspaces. Each is admitted like a launch and leaves the
//! queue once its agent registers, or while waiting once its record is gone,
//! as after `shoal resume --now` or a discard.
use std::collections::BTreeMap;

use anyhow::{Context as _, Result, ensure};
use rusqlite::{Connection, params};

use crate::{
    daemon::workspace::Manager,
    execution::recovery,
    model::QueueEntry,
    state::{QueueKind, QueueState},
};

/// The workspace a stopped agent restores in.
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

    /// Forget the waiting restores whose stopped agent's record is gone, as
    /// after `shoal resume --now` or a discard.
    pub(super) async fn forget_resolved_restores(&self) -> Result<()> {
        let waiting = self.store.run(|db| waiting_restores(db)).await?;
        let resolved: Vec<String> = waiting
            .into_iter()
            .filter(|(_, workspace, execution)| {
                let path = recovery::record_path(&self.paths, workspace, execution);
                !path.try_exists().unwrap_or(true)
            })
            .map(|(id, _, _)| id)
            .collect();
        if resolved.is_empty() {
            return Ok(());
        }
        self.store
            .run(move |db| {
                for id in resolved {
                    db.execute(
                        "DELETE FROM agent_queue WHERE id=?1 AND state=?2",
                        params![id, QueueState::Queued],
                    )?;
                }
                Ok(())
            })
            .await
    }

    /// Cancel the waiting restores of `workspace_id`'s stopped agents, which
    /// stay stopped until `shoal resume`.
    pub(crate) async fn cancel_stopped_restores(&self, workspace_id: &str) -> Result<()> {
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
