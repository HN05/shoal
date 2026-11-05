//! Restores of stopped agents and last sessions that the daemon starts, as
//! it starts launches in existing workspaces: `shoal resume` queues them, and
//! so does a signal that ended an agent's session, through a request beside
//! its record. Each is admitted like a launch and leaves the queue once its
//! agent registers, or while waiting once what it restores is gone, as after
//! `shoal resume --now` or a discard. A restart's restore starts at once, in
//! the slot its agent kept.
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
};

use anyhow::{Context as _, Result, ensure};
use rusqlite::{Connection, params};

use super::Start;
use crate::{
    daemon::workspace::Manager,
    execution::recovery,
    model::{QueueEntry, Workspace},
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
    /// Shoal queued it on its own rather than `shoal resume`, so an agent
    /// stopped between turns waits for an update.
    automatic: bool,
    /// Queued, or starting at once.
    state: QueueState,
}

impl Restore {
    fn new(
        target: Target,
        execution: &str,
        agent: &str,
        env: &BTreeMap<String, String>,
        automatic: bool,
    ) -> Result<Self> {
        Ok(Self {
            id: uuid::Uuid::new_v4().to_string(),
            target,
            agent: agent.to_owned(),
            execution: execution.to_owned(),
            env: serde_json::to_string(env)?,
            automatic,
            state: QueueState::Queued,
        })
    }

    /// The restore, admitted as the entry `id` as it is recorded.
    fn starting(self, id: String) -> Self {
        Self {
            id,
            state: QueueState::Starting,
            ..self
        }
    }
}

/// Record `restore` behind the work already waiting, unless its execution
/// already has one, which a resume keeps in place and makes no longer
/// automatic; one that starts at once refuses it. Its workspace must be
/// ready and its execution ended, since a recorded one runs, waits for its
/// wrapper's restore or is unresolved.
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
    ensure!(
        restore.state == QueueState::Queued
            || !crate::daemon::store::exists(
                &tx,
                "SELECT 1 FROM agent_queue WHERE execution_id=?1",
                [&restore.execution],
            )?,
        "the restore of {} is already in the agent queue",
        restore.target.name
    );
    let now = crate::time::unix_seconds() as i64;
    tx.execute(
        "INSERT INTO agent_queue(id,kind,repository_id,name,agent,env,execution_id,state,created_at,seen_at,workspace_id,automatic)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?10,?11)
         ON CONFLICT(execution_id) DO UPDATE SET automatic=automatic AND excluded.automatic",
        params![
            restore.id,
            QueueKind::Restore,
            restore.target.repository,
            restore.target.name,
            restore.agent,
            restore.env,
            restore.execution,
            restore.state,
            now,
            restore.target.id,
            restore.automatic,
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

/// Every workspace's ID.
fn all_workspaces(db: &Connection) -> Result<Vec<String>> {
    Ok(db
        .prepare("SELECT id FROM workspaces")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The workspaces with a restore in the queue.
fn restoring_workspaces(db: &Connection) -> Result<HashSet<String>> {
    Ok(db
        .prepare("SELECT DISTINCT workspace_id FROM agent_queue WHERE kind=?1")?
        .query_map([QueueKind::Restore], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn stopped_restores(db: &Connection) -> Result<HashSet<String>> {
    Ok(db
        .prepare("SELECT execution_id FROM agent_queue WHERE kind=?1 AND env IS NOT NULL")?
        .query_map([QueueKind::Restore], |row| row.get(0))?
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

/// The IDs of the `waiting` restores whose recovery record or last session
/// is gone. A file whose existence cannot be read counts as kept.
fn resolved_restores(paths: &Paths, waiting: Vec<(String, String, String)>) -> Vec<String> {
    waiting
        .into_iter()
        .filter(|(_, workspace, execution)| {
            let gone = |path: PathBuf| !path.try_exists().unwrap_or(true);
            gone(recovery::record_path(paths, workspace, execution))
                && gone(recovery::LastSession::path(paths, workspace, execution))
        })
        .map(|(id, _, _)| id)
        .collect()
}

/// The agent stopped as `execution` in `workspace_id`, from its recovery
/// record or else its last session.
fn stopped_agent(paths: &Paths, workspace_id: &str, execution: &str) -> Result<Option<String>> {
    let record = recovery::Record::load(&recovery::record_path(paths, workspace_id, execution))?;
    if let Some(record) = record {
        return Ok(Some(record.agent));
    }
    let path = recovery::LastSession::path(paths, workspace_id, execution);
    Ok(recovery::LastSession::load(&path)?.map(|last| last.agent))
}

/// Delete the restores the daemon starts of `workspace_id`'s stopped agents.
fn delete_stopped_restores(db: &Connection, workspace_id: &str) -> Result<()> {
    db.execute(
        "DELETE FROM agent_queue WHERE workspace_id=?1 AND kind=?2 AND env IS NOT NULL",
        params![workspace_id, QueueKind::Restore],
    )?;
    Ok(())
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
    /// or of its last session, which the daemon starts with `env` once
    /// admitted, and return its entry. Queueing it again keeps its place,
    /// and the agent no longer waits for an update there.
    pub async fn queue_stopped_restore(
        &self,
        workspace: &str,
        execution: &str,
        env: BTreeMap<String, String>,
    ) -> Result<QueueEntry> {
        let workspace = self.workspace(workspace).await?;
        let agent = stopped_agent(&self.paths, &workspace.id, execution)?
            .with_context(|| format!("{} has no stopped agent {execution}", workspace.name))?;
        let target = Target {
            id: workspace.id,
            repository: workspace.repository_id,
            name: workspace.name,
        };
        let restore = Restore::new(target, execution, &agent, &env, false)?;
        // Read the entry before dispatch can start it and its agent take it.
        let _gate = self.agent_queue.lock_admission().await;
        self.store.run(move |db| insert(db, &restore)).await?;
        self.queue_entries()
            .await?
            .into_iter()
            .find(|entry| entry.execution.as_deref() == Some(execution))
            .context("the restore already left the agent queue")
    }

    /// Record the restore of the agent stopped as `execution` in `workspace`
    /// as the admitted entry `id`, so it starts at once in the slot the
    /// agent kept while its record waits, and return its start, where the
    /// agent ran. Requires the queue gate.
    pub(crate) async fn admit_stopped_restore(
        &self,
        workspace: &Workspace,
        execution: &str,
        id: String,
    ) -> Result<Start> {
        let path = recovery::record_path(&self.paths, &workspace.id, execution);
        let record = recovery::Record::load(&path)?
            .with_context(|| format!("{} has no stopped agent {execution}", workspace.name))?;
        let env = crate::cli::terminal::detached_start_environment(record.origin.as_ref());
        let target = Target {
            id: workspace.id.clone(),
            repository: workspace.repository_id.clone(),
            name: workspace.name.clone(),
        };
        let restore = Restore::new(target, execution, &record.agent, &env, false)?.starting(id);
        let start = Start {
            id: restore.id.clone(),
            name: restore.target.name.clone(),
            env,
        };
        self.store.run(move |db| insert(db, &restore)).await?;
        Ok(start)
    }

    /// Queue the restore of `agent`, stopped as `execution` in `workspace`,
    /// whose wrapper exited with `code` after its restore was admitted but
    /// before the agent started, as one whose binary an upgrade removed may.
    /// Shoal queues it on its own, so an agent stopped between turns waits
    /// for an update, and starts it where the agent ran. Returns whether it
    /// is queued; otherwise its record stays for `shoal resume`.
    pub(crate) async fn queue_failed_restore(
        &self,
        workspace: &crate::model::Workspace,
        agent: &str,
        execution: &str,
        code: i32,
    ) -> bool {
        let path = recovery::record_path(&self.paths, &workspace.id, execution);
        let Some(record) = recovery::Record::load(&path).ok().flatten() else {
            return false;
        };
        let env = crate::cli::terminal::detached_start_environment(record.origin.as_ref());
        let target = Target {
            id: workspace.id.clone(),
            repository: workspace.repository_id.clone(),
            name: workspace.name.clone(),
        };
        let queued = async {
            let restore = Restore::new(target, execution, agent, &env, true)?;
            let _gate = self.agent_queue.lock_admission().await;
            self.store.run(move |db| insert(db, &restore)).await
        };
        if let Err(error) = queued.await {
            crate::daemon::log!(
                "agent queue: cannot queue failed restore of {execution}: {error:#}"
            );
            return false;
        }
        self.notify(
            Some(&workspace.name),
            crate::daemon::notifications::NotificationKind::AgentExited,
            format!(
                "Restore of {agent} failed with exit code {code}; its restore waits in the agent queue"
            ),
        )
        .await;
        true
    }

    /// Queue the restores that stopped agents request once their execution
    /// has ended. A done workspace's agent waits until `shoal resume` names
    /// its workspace.
    pub(super) async fn queue_requested_restores(&self) -> Result<()> {
        let (workspaces, recorded) = self
            .store
            .run(|db| Ok((open_workspaces(db)?, recorded_executions(db)?)))
            .await?;
        for workspace in workspaces {
            for (execution, env) in recovery::restore_requests(&self.paths, &workspace.id)? {
                if !recorded.contains(&execution) {
                    self.queue_requested_restore(&workspace, &execution, &env)
                        .await?;
                }
            }
        }
        Ok(())
    }

    /// Queue the restore requested for the agent stopped as `execution` in
    /// `workspace`, which starts with `env`, then forget the request. One
    /// whose record is gone, resumed or discarded, is only forgotten; one
    /// that cannot be queued stays without holding back the others.
    async fn queue_requested_restore(
        &self,
        workspace: &Target,
        execution: &str,
        env: &BTreeMap<String, String>,
    ) -> Result<()> {
        let path = recovery::record_path(&self.paths, &workspace.id, execution);
        if let Some(record) = recovery::Record::load(&path).ok().flatten() {
            let restore = Restore::new(workspace.clone(), execution, &record.agent, env, true)?;
            if let Err(error) = self.store.run(move |db| insert(db, &restore)).await {
                crate::daemon::log!("agent queue: cannot queue restore of {execution}: {error:#}");
                return Ok(());
            }
        }
        recovery::forget_restore_request(&self.paths, &workspace.id, execution)
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

    /// Cancel the restores of `workspace_id`'s stopped agents, requested,
    /// waiting or admitted, and the restarts that would start one in a new
    /// wrapper, which stay stopped until `shoal resume`. The queue gate keeps
    /// dispatch from queueing a request meanwhile, and a restart from
    /// recording its restore, and a restore whose agent already registered
    /// has left the queue.
    pub(crate) async fn cancel_stopped_restores(&self, workspace_id: &str) -> Result<()> {
        let _gate = self.agent_queue.lock_admission().await;
        self.cancel_renewals(workspace_id).await;
        self.clear_restore_requests(workspace_id)?;
        let workspace_id = workspace_id.to_owned();
        self.store
            .run(move |db| delete_stopped_restores(db, &workspace_id))
            .await
    }

    /// Forget the restore requests of `workspace_id`'s stopped agents.
    fn clear_restore_requests(&self, workspace_id: &str) -> Result<()> {
        for (execution, _) in recovery::restore_requests(&self.paths, workspace_id)? {
            recovery::forget_restore_request(&self.paths, workspace_id, &execution)?;
        }
        Ok(())
    }

    /// The workspaces with a stopped agent whose restore is requested or in
    /// the queue. Requires the queue gate, which keeps restores in the queue
    /// until it is released.
    pub(in crate::daemon::workspace) async fn pending_restore_workspaces(
        &self,
    ) -> Result<HashSet<String>> {
        let (workspaces, queued) = self
            .store
            .run(|db| Ok((all_workspaces(db)?, restoring_workspaces(db)?)))
            .await?;
        let mut pending = queued;
        for workspace in workspaces {
            if !recovery::restore_requests(&self.paths, &workspace)?.is_empty() {
                pending.insert(workspace);
            }
        }
        Ok(pending)
    }

    /// The executions of stopped agents whose restore the daemon starts from
    /// the queue, unlike one a waiting wrapper restores.
    pub(in crate::daemon::workspace) async fn queued_stopped_restores(
        &self,
    ) -> Result<HashSet<String>> {
        self.store.run(|db| stopped_restores(db)).await
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
