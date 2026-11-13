//! Restores of stopped agents and last sessions that the daemon starts, as
//! it starts launches in existing workspaces: `shoal restore` queues them, and
//! so does a signal that ended an agent's session, through a request beside
//! its record. Each is admitted like a launch and leaves the queue once its
//! agent registers, or while waiting once what it restores is gone, as after
//! `shoal restore --now` or `--forget`. A restart's restore starts at once, in
//! the slot its agent kept.
use crate::client::execution_ids::ExecutionId;
use crate::client::repository_ids::RepositoryId;
use crate::client::{queue::QueueEntryId, space_ids::SpaceId};
use crate::execution::recovery::LastSessionExt;
use crate::execution::recovery::RecordExt;
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
};

use anyhow::{Context as _, Result, ensure};
use rusqlite::{Connection, params};

use super::Start;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::workspace::{Manager, agents},
    execution::recovery,
    model::{Asleep, PresentSpace, QueueEntry},
    paths::Paths,
    state::{QueueKind, QueueState},
};

/// The space a stopped agent restores in.
#[derive(Clone)]
struct Target {
    id: SpaceId,
    /// Its repository; none for a chat without one.
    repository: Option<RepositoryId>,
    name: String,
}

impl Target {
    fn of(space: &PresentSpace) -> Self {
        Self {
            id: space.id().clone(),
            repository: space.repository_id().cloned(),
            name: space.name().to_string(),
        }
    }
}

/// A stopped agent's restore to record.
struct Restore {
    id: QueueEntryId,
    target: Target,
    agent: crate::agent::Agent,
    execution: ExecutionId,
    env: String,
    /// Why Shoal put the agent to sleep and what wakes it, when Shoal queued
    /// it on its own rather than `shoal restore`, so an agent stopped between
    /// turns waits for an update.
    asleep: Option<Asleep>,
    /// Queued, or starting at once.
    state: QueueState,
}

impl Restore {
    fn new(
        target: Target,
        execution: &ExecutionId,
        agent: &crate::agent::Agent,
        env: &BTreeMap<String, String>,
        asleep: Option<Asleep>,
    ) -> Result<Self> {
        Ok(Self {
            id: QueueEntryId::generate(),
            target,
            agent: agent.to_owned(),
            execution: execution.to_owned(),
            env: serde_json::to_string(env)?,
            asleep,
            state: QueueState::Queued,
        })
    }

    /// Whether Shoal queued it on its own.
    fn automatic(&self) -> bool {
        self.asleep.is_some()
    }

    /// The restore, admitted as the entry `id` as it is recorded.
    fn starting(self, id: QueueEntryId) -> Self {
        Self {
            id,
            state: QueueState::Starting,
            ..self
        }
    }
}

/// Record `restore` behind the work already waiting, unless its execution
/// already has one, which a resume keeps in place and makes no longer
/// automatic; one that starts at once refuses it. Its space must be a ready
/// workspace or a chat, and its execution ended, since a recorded one runs
/// or is unresolved, unless its wrapper waits to restore it.
fn insert(tx: &rusqlite::Transaction<'_>, restore: &Restore) -> Result<()> {
    if !crate::daemon::links::space::is_chat(tx, &restore.target.id)? {
        crate::daemon::store::require_ready(tx, &restore.target.id)
            .context("restore agents in a ready workspace")?;
    }
    if restore.state == QueueState::Queued && keep_wrapper_restore(tx, restore)? {
        return Ok(());
    }
    ensure!(
        !crate::daemon::store::exists(
            tx,
            "SELECT 1 FROM executions WHERE id=?1",
            [&restore.execution],
        )?,
        "execution {} has not ended; stop it with shoal stop {} first",
        restore.execution,
        restore.target.id
    );
    ensure!(
        restore.state == QueueState::Queued
            || !crate::daemon::store::exists(
                tx,
                "SELECT 1 FROM agent_queue WHERE execution_id=?1",
                [&restore.execution],
            )?,
        "the restore of {} is already in the agent queue",
        restore.target.name
    );
    let now = crate::time::UnixTime::now();
    tx.execute(
        "INSERT INTO agent_queue(id,kind,repository_id,name,agent,env,execution_id,state,created_at,seen_at,space_id,automatic,stop_reason,wakes_when)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?10,?11,?12,?13)
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
            restore.automatic(),
            restore.asleep.as_ref().map(|asleep| &asleep.reason),
            restore.asleep.as_ref().map(|asleep| &asleep.wakes_when),
        ],
    )?;
    Ok(())
}

/// Keep the restore a waiting wrapper carries out for `restore`'s execution
/// in place, no longer automatic unless `restore` is, returning whether the
/// wrapper has one.
fn keep_wrapper_restore(db: &Connection, restore: &Restore) -> Result<bool> {
    let kept = db.execute(
        "UPDATE agent_queue SET automatic=automatic AND ?2
         WHERE execution_id=?1 AND kind=?3 AND env IS NULL",
        params![restore.execution, restore.automatic(), QueueKind::Restore],
    )?;
    Ok(kept == 1)
}

/// The ready workspaces and the chats whose assignment is not done, where
/// stopped agents restore on their own.
fn open_spaces(db: &Connection) -> Result<Vec<Target>> {
    Ok(db
        .prepare(
            "SELECT id,repository_id,name FROM workspaces w WHERE state='ready' AND NOT EXISTS
                (SELECT 1 FROM space_completion c WHERE c.space_id=w.id)
             UNION ALL SELECT id,repository_id,name FROM chats h WHERE NOT EXISTS
                (SELECT 1 FROM space_completion c WHERE c.space_id=h.id)",
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
fn recorded_executions(db: &Connection) -> Result<HashSet<ExecutionId>> {
    Ok(db
        .prepare("SELECT id FROM executions")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Every space's ID.
fn all_spaces(db: &Connection) -> Result<Vec<SpaceId>> {
    Ok(db
        .prepare("SELECT id FROM workspaces UNION ALL SELECT id FROM chats")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The workspaces with a restore in the queue.
fn restoring_workspaces(db: &Connection) -> Result<HashSet<SpaceId>> {
    Ok(db
        .prepare("SELECT DISTINCT space_id FROM agent_queue WHERE kind=?1")?
        .query_map([QueueKind::Restore], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn stopped_restores(db: &Connection) -> Result<HashSet<ExecutionId>> {
    Ok(db
        .prepare("SELECT execution_id FROM agent_queue WHERE kind=?1 AND env IS NOT NULL")?
        .query_map([QueueKind::Restore], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Each waiting restore the daemon starts: its ID, workspace and execution.
fn waiting_restores(db: &Connection) -> Result<Vec<(QueueEntryId, SpaceId, ExecutionId)>> {
    Ok(db
        .prepare(
            "SELECT id,space_id,execution_id FROM agent_queue
             WHERE kind=?1 AND env IS NOT NULL AND state=?2",
        )?
        .query_map(params![QueueKind::Restore, QueueState::Queued], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// The IDs of the `waiting` restores whose recovery record or last session
/// is gone. A file whose existence cannot be read counts as kept.
fn resolved_restores(
    paths: &Paths,
    waiting: Vec<(QueueEntryId, SpaceId, ExecutionId)>,
) -> Vec<QueueEntryId> {
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

/// Why the agent whose stop saved `record` is asleep and what wakes it, for
/// a restore Shoal queues on its own: once nothing blocks its space for an
/// agent stopped because it was blocked, else once an update arrives for
/// one stopped between turns, else once the queue starts it.
fn asleep(record: &recovery::Record) -> Asleep {
    let reason = record.stop_reason.as_deref();
    let wakes_when = match reason == Some(agents::BLOCKED_REASON) {
        true => agents::WAKES_UNBLOCKED,
        false if record.between_turns() => agents::WAKES_ON_UPDATE,
        false => WAKES_IN_QUEUE,
    };
    Asleep {
        reason: reason.unwrap_or(UNKNOWN_STOP).to_owned(),
        wakes_when: wakes_when.to_owned(),
    }
}

/// What wakes an agent interrupted mid-turn whose restore the daemon starts.
pub(super) const WAKES_IN_QUEUE: &str = "the agent queue starts it";

/// Why a stop that saved no reason put an agent to sleep.
pub(super) const UNKNOWN_STOP: &str = "its session ended";

/// The agent stopped as `execution` in `workspace_id`, from its recovery
/// record or else its last session.
fn stopped_agent(
    paths: &Paths,
    workspace_id: &SpaceId,
    execution: &ExecutionId,
) -> Result<Option<crate::agent::Agent>> {
    let record = recovery::Record::load(&recovery::record_path(paths, workspace_id, execution))?;
    if let Some(record) = record {
        return Ok(Some(record.agent));
    }
    let path = recovery::LastSession::path(paths, workspace_id, execution);
    Ok(recovery::LastSession::load(&path)?.map(|last| last.agent))
}

/// Delete the restores the daemon starts of `workspace_id`'s stopped agents.
fn delete_stopped_restores(db: &Connection, workspace_id: &SpaceId) -> Result<()> {
    db.execute(
        "DELETE FROM agent_queue WHERE space_id=?1 AND kind=?2 AND env IS NOT NULL",
        params![workspace_id, QueueKind::Restore],
    )?;
    Ok(())
}

/// Delete the entries `ids` that still wait; a starting one belongs to its agent.
fn delete_waiting(db: &Connection, ids: &[QueueEntryId]) -> Result<()> {
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
    /// admitted, and return its entry. Queueing it again, or one its waiting
    /// wrapper carries out, keeps its place, and the agent no longer waits
    /// for an update or healthy readings there.
    pub async fn queue_stopped_restore(
        &self,
        workspace: &SpaceSelector,
        execution: &ExecutionId,
        env: BTreeMap<String, String>,
    ) -> Result<QueueEntry> {
        self.add_stopped_restore(workspace, execution, env, false)
            .await
    }

    /// Queue the restore as [`Manager::queue_stopped_restore`] does and
    /// start it at once, regardless of the limits.
    pub async fn start_stopped_restore(
        &self,
        workspace: &SpaceSelector,
        execution: &ExecutionId,
        env: BTreeMap<String, String>,
    ) -> Result<QueueEntry> {
        self.add_stopped_restore(workspace, execution, env, true)
            .await
    }

    /// Queue the restore of the agent stopped as `execution` in
    /// `workspace`, starting it at once when `now`, and return its entry; an
    /// agent of a blocked space first reads what still blocks it.
    /// Asking for it says the work is not finished, so it withdraws the
    /// workspace's completion, whichever wrapper restores the agent; only
    /// once the queue's gate is released, since recording a completion takes
    /// that gate under the workspace's PR gate.
    async fn add_stopped_restore(
        &self,
        space: &SpaceSelector,
        execution: &ExecutionId,
        env: BTreeMap<String, String>,
        now: bool,
    ) -> Result<QueueEntry> {
        let space = self.space(space).await?;
        let id = space.id().clone();
        // Read before the restored agent reads its messages.
        self.tell_still_blocked(&id).await;
        let entry = (self.record_stopped_restore(&space, execution, env, now)).await?;
        self.reopen_for_resume(&id).await;
        Ok(entry)
    }

    /// Record the restore of the agent stopped as `execution` in `space`,
    /// starting it at once when `now`, and return its entry.
    async fn record_stopped_restore(
        &self,
        space: &PresentSpace,
        execution: &ExecutionId,
        env: BTreeMap<String, String>,
        now: bool,
    ) -> Result<QueueEntry> {
        let agent = stopped_agent(&self.paths, space.id(), execution)?
            .with_context(|| format!("{} has no stopped agent {execution}", space.name()))?;
        let restore = Restore::new(Target::of(space), execution, &agent, &env, None)?;
        // Read the entry before dispatch can start it and its agent take it.
        let _gate = self.agent_queue.lock_admission().await;
        self.store.write(move |tx| insert(tx, &restore)).await?;
        let entry = self
            .queue_entries()
            .await?
            .into_iter()
            .find(|entry| entry.execution.as_ref() == Some(execution))
            .context("the restore already left the agent queue")?;
        match now && entry.state == QueueState::Queued {
            true => self.start_waiting(entry, false).await,
            false => Ok(entry),
        }
    }

    /// Record the restore of the agent stopped as `execution` in `workspace`
    /// as the admitted entry `id`, so it starts at once in the slot the
    /// agent kept while its record waits, and return its start, where the
    /// agent ran. Requires the queue gate.
    pub(crate) async fn admit_stopped_restore(
        &self,
        space: &PresentSpace,
        execution: &ExecutionId,
        id: QueueEntryId,
    ) -> Result<Start> {
        let path = recovery::record_path(&self.paths, space.id(), execution);
        let record = recovery::Record::load(&path)?
            .with_context(|| format!("{} has no stopped agent {execution}", space.name()))?;
        let env = crate::terminal::detached_start_environment(record.origin.as_ref());
        let restore =
            Restore::new(Target::of(space), execution, &record.agent, &env, None)?.starting(id);
        let start = Start {
            id: restore.id.clone(),
            name: space.name().clone(),
            env,
        };
        self.store.write(move |tx| insert(tx, &restore)).await?;
        Ok(start)
    }

    /// Queue the restore of `agent`, stopped as `execution` in `workspace`,
    /// whose wrapper exited with `code` after its restore was admitted but
    /// before the agent started, as one whose binary an upgrade removed may.
    /// Shoal queues it on its own, so an agent stopped between turns waits
    /// for an update, and starts it where the agent ran. Returns whether it
    /// is queued; otherwise its record stays for `shoal restore`.
    pub(crate) async fn queue_failed_restore(
        &self,
        space: &PresentSpace,
        agent: &crate::agent::Agent,
        execution: &ExecutionId,
        code: i32,
    ) -> bool {
        let path = recovery::record_path(&self.paths, space.id(), execution);
        let Some(record) = recovery::Record::load(&path).ok().flatten() else {
            return false;
        };
        let env = crate::terminal::detached_start_environment(record.origin.as_ref());
        let queued = async {
            let restore = Restore::new(
                Target::of(space),
                execution,
                agent,
                &env,
                Some(asleep(&record)),
            )?;
            let _gate = self.agent_queue.lock_admission().await;
            self.store.write(move |tx| insert(tx, &restore)).await
        };
        if let Err(error) = queued.await {
            crate::daemon::log!(
                "agent queue: cannot queue failed restore of {execution}: {error:#}"
            );
            return false;
        }
        self.notify_space(space.name(), space.id(),
            crate::daemon::notifications::NotificationKind::AgentExited,
            format!(
                "Restore of {agent} failed with exit code {code}; its restore waits in the agent queue"
            ),
        )
        .await;
        true
    }

    /// Queue the restores that stopped agents request once their execution
    /// has ended. A done space's agent waits until `shoal restore` names its
    /// space.
    pub(super) async fn queue_requested_restores(&self) -> Result<()> {
        let (spaces, recorded) = self
            .store
            .run(|db| Ok((open_spaces(db)?, recorded_executions(db)?)))
            .await?;
        for space in spaces {
            for (execution, env) in recovery::restore_requests(&self.paths, &space.id)? {
                if !recorded.contains(&execution) {
                    self.queue_requested_restore(&space, &execution, &env)
                        .await?;
                }
            }
        }
        Ok(())
    }

    /// Queue the restore requested for the agent stopped as `execution` in
    /// `space`, which starts with `env`, then forget the request. One whose
    /// record is gone, resumed or forgotten, is only forgotten; one that
    /// cannot be queued stays without holding back the others.
    async fn queue_requested_restore(
        &self,
        space: &Target,
        execution: &ExecutionId,
        env: &BTreeMap<String, String>,
    ) -> Result<()> {
        let path = recovery::record_path(&self.paths, &space.id, execution);
        if let Some(record) = recovery::Record::load(&path).ok().flatten() {
            let asleep = Some(asleep(&record));
            let restore = Restore::new(space.clone(), execution, &record.agent, env, asleep)?;
            if let Err(error) = self.store.write(move |tx| insert(tx, &restore)).await {
                crate::daemon::log!("agent queue: cannot queue restore of {execution}: {error:#}");
                return Ok(());
            }
        }
        recovery::forget_restore_request(&self.paths, &space.id, execution)
    }

    /// Forget the waiting restores whose stopped agent's record is gone, as
    /// after `shoal restore --now` or `--forget`.
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
    /// wrapper, which stay stopped until `shoal restore`. The queue gate keeps
    /// dispatch from queueing a request meanwhile, and a restart from
    /// recording its restore, and a restore whose agent already registered
    /// has left the queue.
    pub(crate) async fn cancel_stopped_restores(&self, workspace_id: &SpaceId) -> Result<()> {
        let _gate = self.agent_queue.lock_admission().await;
        self.cancel_renewals(workspace_id).await;
        self.clear_restore_requests(workspace_id)?;
        let workspace_id = workspace_id.to_owned();
        self.store
            .run(move |db| delete_stopped_restores(db, &workspace_id))
            .await
    }

    /// Forget the restore requests of `workspace_id`'s stopped agents.
    fn clear_restore_requests(&self, workspace_id: &SpaceId) -> Result<()> {
        for (execution, _) in recovery::restore_requests(&self.paths, workspace_id)? {
            recovery::forget_restore_request(&self.paths, workspace_id, &execution)?;
        }
        Ok(())
    }

    /// The workspaces with a stopped agent whose restore is requested or in
    /// the queue, or that a restart starts in a new wrapper. Requires the
    /// queue gate, which keeps restores in the queue, and a restart from
    /// recording its own, until it is released.
    pub(in crate::daemon::workspace) async fn pending_restore_workspaces(
        &self,
    ) -> Result<HashSet<SpaceId>> {
        let (spaces, queued) = self
            .store
            .run(|db| Ok((all_spaces(db)?, restoring_workspaces(db)?)))
            .await?;
        let mut pending = queued;
        pending.extend(self.renewing_workspaces().await);
        for space in spaces {
            if !recovery::restore_requests(&self.paths, &space)?.is_empty() {
                pending.insert(space);
            }
        }
        Ok(pending)
    }

    /// The executions of stopped agents whose restore the daemon starts from
    /// the queue, unlike one a waiting wrapper restores.
    pub(in crate::daemon::workspace) async fn queued_stopped_restores(
        &self,
    ) -> Result<HashSet<ExecutionId>> {
        self.store.run(|db| stopped_restores(db)).await
    }

    /// Whether the entry `id` restores a stopped agent the daemon starts.
    pub(super) async fn restores_stopped_agent(&self, id: &QueueEntryId) -> Result<bool> {
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
