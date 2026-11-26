//! Restores of stopped agents and last sessions that the daemon starts, rather
//! than a waiting wrapper. `docs/design/queue.md` says what each waits for.
use crate::execution::recovery::LastSessionExt;
use crate::execution::recovery::RecordExt;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
};

use anyhow::{Context as _, Result, ensure};
use rusqlite::{Connection, params};

use super::Start;
use crate::Agents;
use crate::daemon::Daemon;
use crate::daemon::Records;
use crate::execution::recovery;
use crate::model::Asleep;
use crate::model::QueueEntry;
use crate::model::Space;
use crate::paths::Paths;
use crate::state::QueueKind;
use crate::state::QueueState;
use shoal_protocol::alerts::AlertKind;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

/// The space a stopped agent restores in.
#[derive(Clone)]
struct Target {
    id: SpaceId,
    /// Its repository; none for a chat without one.
    repository: Option<RepositoryId>,
    name: String,
}

impl Target {
    fn of(space: &Space) -> Self {
        Self {
            id: space.id.clone(),
            repository: space.repository_id.as_ref().cloned(),
            name: space.name.to_string(),
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
    /// Why Shoal put the agent to sleep and what wakes it, when Shoal queued it
    /// on its own rather than `shoal resume`.
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

/// Record `restore` behind the work already waiting. A restore already recorded
/// for its execution keeps its place, and a resume makes it no longer
/// automatic; a restore that starts at once refuses it. Its space must be a
/// ready workspace or a chat. Its execution must have ended, since a recorded
/// one runs or is unresolved, unless its wrapper waits to restore it.
fn insert(tx: &rusqlite::Transaction<'_>, records: &dyn Records, restore: &Restore) -> Result<()> {
    if !records.space_is_chat(tx, &restore.target.id)? {
        shoal_store::require_ready(tx, &restore.target.id)
            .context("restore agents in a ready workspace")?;
    }
    if restore.state == QueueState::Queued && keep_wrapper_restore(tx, restore)? {
        return Ok(());
    }
    ensure!(
        !shoal_store::exists(
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
            || !shoal_store::exists(
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
fn open_spaces(db: &Connection, records: &dyn Records) -> Result<Vec<Target>> {
    Ok((records.spaces(db)?.into_iter())
        .filter(|space| space.open() && !space.done)
        .map(|space| Target {
            id: space.id,
            repository: space.repository,
            name: space.name,
        })
        .collect())
}

/// The recorded executions, which have not ended.
fn recorded_executions(db: &Connection) -> Result<HashSet<ExecutionId>> {
    Ok(db
        .prepare("SELECT id FROM executions")?
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

/// The stopped agents of the space `id` whose restore is in the queue, with
/// whether it is starting.
fn space_restores(
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
) -> Result<Vec<(ExecutionId, bool)>> {
    Ok(tx
        .prepare("SELECT execution_id, state=?3 FROM agent_queue WHERE space_id=?1 AND kind=?2")?
        .query_map(
            params![id, QueueKind::Restore, QueueState::Starting],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?
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

/// Each restore whose waiting wrapper is gone, since its execution has
/// ended and no wrapper can reattach to it: its space and execution.
fn orphaned_restores(db: &Connection) -> Result<Vec<(SpaceId, ExecutionId)>> {
    Ok(db
        .prepare(
            "SELECT space_id,execution_id FROM agent_queue q
             WHERE kind=?1 AND env IS NULL AND space_id IS NOT NULL
                AND NOT EXISTS (SELECT 1 FROM executions e WHERE e.id=q.execution_id)",
        )?
        .query_map([QueueKind::Restore], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Take over the restore of `execution`, whose waiting wrapper is gone, from
/// the agent's `record`, or forget it once the record is gone.
fn take_over(
    db: &Connection,
    execution: &ExecutionId,
    record: Option<&recovery::Record>,
) -> Result<()> {
    let Some(record) = record else {
        db.execute("DELETE FROM agent_queue WHERE execution_id=?1", [execution])?;
        return Ok(());
    };
    let env = crate::terminal::detached_start_environment(record.origin.as_ref());
    adopt(db, execution, &env, &asleep(record))
}

/// Hand the restore of `execution`, whose waiting wrapper is gone, back to
/// its place in the queue for the daemon to start with `env`, saying why its
/// agent is asleep while Shoal restores it on its own.
fn adopt(
    db: &Connection,
    execution: &ExecutionId,
    env: &BTreeMap<String, String>,
    asleep: &Asleep,
) -> Result<()> {
    db.execute(
        "UPDATE agent_queue SET env=?2,state=?3,
            stop_reason=CASE WHEN automatic THEN ?4 END,
            wakes_when=CASE WHEN automatic THEN ?5 END
         WHERE execution_id=?1 AND env IS NULL",
        params![
            execution,
            serde_json::to_string(env)?,
            QueueState::Queued,
            asleep.reason,
            asleep.wakes_when,
        ],
    )?;
    Ok(())
}

/// A restore the daemon starts that Shoal queued on its own.
struct AutomaticRestore {
    id: QueueEntryId,
    space: SpaceId,
    name: SpaceName,
    execution: ExecutionId,
    agent: crate::agent::Agent,
}

/// Each restore the daemon starts that Shoal queued on its own, in `state`.
fn automatic_restores(db: &Connection, state: QueueState) -> Result<Vec<AutomaticRestore>> {
    Ok(db
        .prepare(
            "SELECT id,space_id,name,execution_id,agent FROM agent_queue
             WHERE kind=?1 AND env IS NOT NULL AND state=?2 AND automatic",
        )?
        .query_map(params![QueueKind::Restore, state], |row| {
            Ok(AutomaticRestore {
                id: row.get(0)?,
                space: row.get(1)?,
                name: row.get(2)?,
                execution: row.get(3)?,
                agent: row.get(4)?,
            })
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

/// Why the agent whose stop saved `record` is asleep and what wakes it, for a
/// restore Shoal queues on its own.
fn asleep(record: &recovery::Record) -> Asleep {
    let reason = record.stop_reason.as_deref();
    let between_turns = record.between_turns() || reason == Some(crate::control::BLOCKED_REASON);
    let wakes_when = match between_turns {
        true => crate::control::wakes_between_turns(reason.unwrap_or_default()),
        false => WAKES_IN_QUEUE,
    };
    Asleep {
        reason: reason.unwrap_or(UNKNOWN_STOP).to_owned(),
        wakes_when: wakes_when.to_owned(),
        keeps: Vec::new(),
    }
}

/// What wakes an agent interrupted mid-turn whose restore the daemon starts.
pub const WAKES_IN_QUEUE: &str = "the agent queue starts it";

/// Why a stop that saved no reason put an agent to sleep.
pub const UNKNOWN_STOP: &str = "its session ended";

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

/// Return the entries `ids` that still start to their place in the queue.
fn requeue_starts(db: &Connection, ids: &[QueueEntryId]) -> Result<()> {
    for id in ids {
        db.execute(
            "UPDATE agent_queue SET state=?2 WHERE id=?1 AND state=?3",
            params![id, QueueState::Queued, QueueState::Starting],
        )?;
    }
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

impl Agents {
    /// Queue the restore of the agent stopped as `execution` in `workspace`,
    /// or of its last session, which the daemon starts with `env` once
    /// admitted, and return its entry. Queueing it again, or one its waiting
    /// wrapper carries out, keeps its place, and the agent no longer waits
    /// for an update or healthy readings there.
    pub async fn queue_stopped_restore(
        &self,
        daemon: &dyn Daemon,
        workspace: &SpaceSelector,
        execution: &ExecutionId,
        env: BTreeMap<String, String>,
    ) -> Result<QueueEntry> {
        self.add_stopped_restore(daemon, workspace, execution, env, false)
            .await
    }

    /// Queue the restore as [`Manager::queue_stopped_restore`] does and
    /// start it at once, regardless of the limits.
    pub async fn start_stopped_restore(
        &self,
        daemon: &dyn Daemon,
        workspace: &SpaceSelector,
        execution: &ExecutionId,
        env: BTreeMap<String, String>,
    ) -> Result<QueueEntry> {
        self.add_stopped_restore(daemon, workspace, execution, env, true)
            .await
    }

    /// Queue the restore of the agent stopped as `execution` in `workspace`,
    /// starting it at once when `now`, and return its entry. Asking for it
    /// withdraws the workspace's completion, since the work is not finished.
    /// That waits until the queue's gate is released, since recording a
    /// completion takes that gate under the workspace's PR gate.
    async fn add_stopped_restore(
        &self,
        daemon: &dyn Daemon,
        space: &SpaceSelector,
        execution: &ExecutionId,
        env: BTreeMap<String, String>,
        now: bool,
    ) -> Result<QueueEntry> {
        let space = daemon.space(space).await?;
        let id = space.id.clone();
        // Read before the restored agent reads its messages.
        daemon.tell_still_blocked(&id).await;
        let entry = (self.record_stopped_restore(daemon, &space, execution, env, now)).await?;
        daemon.reopen_for_resume(&id).await;
        // The queued agent works its issues again.
        daemon.settle_agent(&id);
        Ok(entry)
    }

    /// Record the restore of the agent stopped as `execution` in `space`,
    /// starting it at once when `now`, and return its entry.
    async fn record_stopped_restore(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        execution: &ExecutionId,
        env: BTreeMap<String, String>,
        now: bool,
    ) -> Result<QueueEntry> {
        let agent = stopped_agent(&self.paths, &space.id, execution)?
            .with_context(|| format!("{} has no stopped agent {execution}", space.name))?;
        let restore = Restore::new(Target::of(space), execution, &agent, &env, None)?;
        // Read the entry before dispatch can start it and its agent take it.
        let _gate = self.agent_queue.lock_admission().await;
        let records = self.records.clone();
        self.store
            .write(move |tx| insert(tx, &*records, &restore))
            .await?;
        let entry = self
            .queue_entries(daemon)
            .await?
            .into_iter()
            .find(|entry| entry.execution.as_ref() == Some(execution))
            .context("the restore already left the agent queue")?;
        match now && entry.state == QueueState::Queued {
            true => self.start_waiting(daemon, entry, false).await,
            false => Ok(entry),
        }
    }

    /// Record the restore of the agent stopped as `execution` in `workspace`
    /// as the admitted entry `id`, so it starts at once in the slot the
    /// agent kept while its record waits, and return its start, where the
    /// agent ran. Requires the queue gate.
    pub async fn admit_stopped_restore(
        &self,
        space: &Space,
        execution: &ExecutionId,
        id: QueueEntryId,
    ) -> Result<Start> {
        let path = recovery::record_path(&self.paths, &space.id, execution);
        let record = recovery::Record::load(&path)?
            .with_context(|| format!("{} has no stopped agent {execution}", space.name))?;
        let env = crate::terminal::detached_start_environment(record.origin.as_ref());
        let restore =
            Restore::new(Target::of(space), execution, &record.agent, &env, None)?.starting(id);
        let start = Start {
            id: restore.id.clone(),
            name: space.name.clone(),
            env,
        };
        let records = self.records.clone();
        self.store
            .write(move |tx| insert(tx, &*records, &restore))
            .await?;
        Ok(start)
    }

    /// Queue the restore of `agent`, stopped as `execution` in `workspace`,
    /// whose wrapper exited after its restore was admitted but before the agent
    /// started, as one whose binary an upgrade removed may. Returns whether it
    /// is queued; otherwise its record stays for `shoal resume`.
    pub async fn queue_failed_restore(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        agent: &crate::agent::Agent,
        execution: &ExecutionId,
        code: i32,
    ) -> bool {
        let path = recovery::record_path(&self.paths, &space.id, execution);
        let Some(record) = recovery::Record::load(&path).ok().flatten() else {
            return false;
        };
        let env = crate::terminal::detached_start_environment(record.origin.as_ref());
        let queued = async {
            // Healthy readings admitted the restore that failed.
            recovery::clear_load_mark(&self.paths, &space.id, execution)?;
            let restore = Restore::new(
                Target::of(space),
                execution,
                agent,
                &env,
                Some(asleep(&record)),
            )?;
            let _gate = self.agent_queue.lock_admission().await;
            let records = self.records.clone();
            self.store
                .write(move |tx| insert(tx, &*records, &restore))
                .await
        };
        if let Err(error) = queued.await {
            crate::log!("agent queue: cannot queue failed restore of {execution}: {error:#}");
            return false;
        }
        daemon.notify_space(&space.name, &space.id,
            shoal_daemon_base::alerts::AlertKind::AgentExited,
            format!(
                "Restore of {agent} failed with exit code {code}; its restore waits in the agent queue"
            ),
        )
        .await;
        true
    }

    /// Queue the restores that stopped agents request once their execution
    /// has ended. A done space's agent waits until `shoal resume` names its
    /// space.
    pub async fn queue_requested_restores(&self) -> Result<()> {
        let records = self.records.clone();
        let (spaces, recorded) = self
            .store
            .run(move |db| Ok((open_spaces(db, &*records)?, recorded_executions(db)?)))
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
            let records = self.records.clone();
            let queued = (self.store).write(move |tx| insert(tx, &*records, &restore));
            if let Err(error) = queued.await {
                crate::log!("agent queue: cannot queue restore of {execution}: {error:#}");
                return Ok(());
            }
        }
        recovery::forget_restore_request(&self.paths, &space.id, execution)
    }

    /// Hand the restores whose waiting wrapper is gone to the daemon, which
    /// starts each agent where its record says it ran. A wrapper is gone once
    /// its execution has ended. A restore whose record is gone is forgotten,
    /// and one whose record cannot be read waits.
    pub async fn adopt_orphaned_restores(&self) -> Result<()> {
        let orphaned = self.store.run(|db| orphaned_restores(db)).await?;
        for (space, execution) in orphaned {
            let path = recovery::record_path(&self.paths, &space, &execution);
            let Ok(record) = recovery::Record::load(&path) else {
                continue;
            };
            (self.store)
                .run(move |db| take_over(db, &execution, record.as_ref()))
                .await?;
        }
        Ok(())
    }

    /// Leave the waiting restores the daemon starts of agents load or disk
    /// space stopped mid-turn for `shoal resume` while
    /// `[machine.overload.recovery]` is disabled, as their waiting wrappers
    /// leave theirs, telling each space. Their records stay.
    pub async fn leave_unrecovered_restores(&self, daemon: &dyn Daemon) -> Result<()> {
        if self.config.current().machine.overload.recovery.enabled {
            return Ok(());
        }
        let left = self.load_restores(QueueState::Queued).await?;
        if left.is_empty() {
            return Ok(());
        }
        let ids: Vec<_> = left.iter().map(|restore| restore.id.clone()).collect();
        self.store.run(move |db| delete_waiting(db, &ids)).await?;
        for restore in left {
            self.tell_restore_left(daemon, &restore).await;
        }
        Ok(())
    }

    /// Return the starting restores the daemon starts of agents load or disk
    /// space stopped mid-turn to their place in the queue as the daemon
    /// starts: the healthy readings that admitted them were the previous
    /// daemon's, so they wait for this one's, as a reattaching wrapper's
    /// restore does.
    pub async fn return_recovery_starts(&self) -> Result<()> {
        let ids: Vec<_> = (self.load_restores(QueueState::Starting).await?.into_iter())
            .map(|restore| restore.id)
            .collect();
        self.store.run(move |db| requeue_starts(db, &ids)).await
    }

    /// The restores in `state` the daemon starts of agents load or disk
    /// space stopped mid-turn, which Shoal queued on its own.
    async fn load_restores(&self, state: QueueState) -> Result<Vec<AutomaticRestore>> {
        let restores = self
            .store
            .run(move |db| automatic_restores(db, state))
            .await?;
        Ok((restores.into_iter())
            .filter(|restore| {
                self.stopped_as(&restore.space, &restore.execution) == super::wake::Stopped::ForLoad
            })
            .collect())
    }

    /// Tell the space of `restore`, which recovery no longer restores, that
    /// its agent stays stopped and what restores it.
    async fn tell_restore_left(&self, daemon: &dyn Daemon, restore: &AutomaticRestore) {
        let message = format!(
            "Automatic recovery is disabled, so {} stays stopped; `shoal resume {}` restores it",
            restore.agent, restore.space
        );
        let kind = AlertKind::RecoveryBlocked;
        (daemon.notify_space(&restore.name, &restore.space, kind, message)).await;
    }

    /// Forget the waiting restores whose stopped agent's record is gone, as
    /// after `shoal restore --now` or `--discard-agent`.
    pub async fn forget_resolved_restores(&self) -> Result<()> {
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
    /// wrapper. Those agents stay stopped until `shoal resume`. The queue gate
    /// keeps dispatch from queueing a request meanwhile, and a restart from
    /// recording its restore. A restore whose agent already registered has left
    /// the queue.
    pub async fn cancel_stopped_restores(
        &self,
        daemon: &dyn Daemon,
        workspace_id: &SpaceId,
    ) -> Result<()> {
        let _gate = self.agent_queue.lock_admission().await;
        self.cancel_renewals(workspace_id).await;
        self.clear_restore_requests(workspace_id)?;
        let id = workspace_id.to_owned();
        (self.store)
            .run(move |db| delete_stopped_restores(db, &id))
            .await?;
        // An agent no longer asleep no longer works its issues.
        daemon.settle_agent(workspace_id);
        Ok(())
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
    pub async fn pending_restore_workspaces(&self) -> Result<HashSet<SpaceId>> {
        let records = self.records.clone();
        let (spaces, queued) = self
            .store
            .run(move |db| {
                let spaces = records.spaces(db)?.into_iter().map(|space| space.id);
                Ok((spaces.collect::<Vec<_>>(), restoring_workspaces(db)?))
            })
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
    pub async fn queued_stopped_restores(&self) -> Result<HashSet<ExecutionId>> {
        self.store.run(|db| stopped_restores(db)).await
    }

    /// Whether a restore in the queue is about to run the agent of the space
    /// `id` again in a new place: one that starts now, or one that continues
    /// the session a conversion kept for its new place.
    pub async fn restores_moved_agent(&self, id: &SpaceId) -> Result<bool> {
        let space = id.to_owned();
        let restores = self
            .store
            .read(move |tx| space_restores(tx, &space))
            .await?;
        Ok((restores.iter())
            .any(|(execution, starting)| *starting || self.converted(id, execution)))
    }

    /// Whether the last session `execution` of the space `id` kept is one a
    /// conversion kept, to continue in the space's new place.
    fn converted(&self, id: &SpaceId, execution: &ExecutionId) -> bool {
        let path = recovery::LastSession::path(&self.paths, id, execution);
        recovery::LastSession::load(&path)
            .ok()
            .flatten()
            .is_some_and(|last| last.converted)
    }

    /// Whether the entry `id` restores a stopped agent the daemon starts.
    pub async fn restores_stopped_agent(&self, id: &QueueEntryId) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                shoal_store::exists(
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
