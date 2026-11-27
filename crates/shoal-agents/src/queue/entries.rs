//! Operations on one queue entry, from its admission to the agent that takes
//! its slot, or its failure.
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::model::ByKind;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, params};

use super::{
    blockers,
    dispatch::Start,
    store::{
        Awaits, ENTRY_COLUMNS, Launch, LaunchRequest, ensure_recordable, entry, insert_launch,
    },
    worker::Replacement,
};
use crate::Agents;
use crate::daemon::Daemon;
use crate::model::QueueAdmission;
use crate::model::QueueEntry;
use crate::model::QueueRequest;
use crate::model::QueuedPlan;
use crate::model::SpaceKind;
use crate::model::Workspace;
use crate::state::QueueKind;
use crate::state::QueueState;
use shoal_daemon_base::alerts::AlertKind;
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_protocol::launches::LaunchSource;
use shoal_protocol::{
    space_names::{SpaceName, SpaceSelector},
    time::UnixTime,
};
use shoal_schedule::{rank::QueueRank, scheduler::Ranked};

impl Agents {
    /// Admit `request` to a free slot or queue it, while its space has no
    /// agent of its own. The daemon starts an admitted launch unless its
    /// caller starts it.
    pub async fn queue_add(
        &self,
        daemon: &dyn Daemon,
        request: QueueRequest,
    ) -> Result<QueueAdmission> {
        (self.queue_add_awaiting(daemon, request, Awaits::FailedSetup)).await
    }

    /// [`Self::queue_add`] for a launch that `awaits` what it says in its
    /// space, in place of the stopped or asleep agent it replaces.
    pub async fn queue_add_awaiting(
        &self,
        daemon: &dyn Daemon,
        request: QueueRequest,
        awaits: Awaits,
    ) -> Result<QueueAdmission> {
        ensure!(
            !request.now || request.blockers.is_empty(),
            "a launch that starts now cannot be blocked"
        );
        let space = daemon.space(&request.space).await?;
        let repository = space.repository_id.clone();
        let priority = match &repository {
            Some(repository) => daemon.launch_priority(&request, repository).await?,
            None => None,
        };
        let (caller_starts, replace) = (request.starts_in_caller, request.replace);
        let id = QueueEntryId::generate();
        let state = if request.now {
            QueueState::Starting
        } else {
            QueueState::Queued
        };
        let start = Start {
            id: id.clone(),
            name: request.name.clone(),
            env: request.env.clone(),
        };
        // The queue's row names a workspace as the one the launch opens.
        let (opens, chat) = match space.kind() {
            SpaceKind::Workspace => (Some(space.id.clone()), None),
            SpaceKind::Chat => (None, Some(space.id.clone())),
        };
        let launch = Launch {
            id: id.clone(),
            repository,
            request: LaunchRequest {
                priority,
                ..request.into()
            },
            state,
            chat,
            opens,
            scope: None,
            awaits,
        };
        let now = launch.request.now;
        let launch = std::sync::Arc::new(launch);
        if replace {
            self.prepare_replacement(daemon, &space, &launch).await?;
        }
        let _gate = self.agent_queue.lock_admission().await;
        self.take_worker(daemon, &space, &launch, replace).await?;
        let (records, removals) = (self.records.clone(), self.removals.clone());
        self.store
            .write(move |tx| insert_launch(tx, &*records, &*removals, &launch))
            .await?;
        // Its queued agent works the space's issues.
        daemon.settle_agent(&space.id);
        let admitted = now || self.admit_waiting(daemon, Some(&id)).await?;
        // Kept before the gate opens, so no pass starts it in the background
        // before its caller holds it.
        if caller_starts && !admitted {
            self.agent_queue.keep_for_attach(&id);
        }
        // Read the response before a detached worker can take the entry.
        let admission = self
            .admission_of(daemon, &id, admitted && caller_starts)
            .await?;
        if admitted && !caller_starts {
            self.start_launches(daemon, vec![start], None).await?;
        }
        Ok(admission)
    }

    /// Stop the wrappers that wait to restore the asleep agent of `space`
    /// that `launch` replaces, once recording the launch would not refuse
    /// it, before the queue gate is taken. A waiting wrapper would refuse
    /// the replacement's own check, so that check follows the stop: when it
    /// refuses, such as for a launch queued meanwhile, the agent stays
    /// stopped for `shoal resume`.
    async fn prepare_replacement(
        &self,
        daemon: &dyn Daemon,
        space: &crate::model::Space,
        launch: &Arc<Launch>,
    ) -> Result<()> {
        self.ensure_launch_recordable(launch).await?;
        self.stop_asleep_wrappers(daemon, space).await
    }

    /// Give `launch` the one agent of `space`: refuse it while the space has
    /// an agent of its own, unless it `replaces` a stopped or asleep one,
    /// which goes once nothing refuses the launch. Requires the queue gate,
    /// which keeps another launch out until this one is recorded.
    async fn take_worker(
        &self,
        daemon: &dyn Daemon,
        space: &crate::model::Space,
        launch: &Arc<Launch>,
        replaces: bool,
    ) -> Result<()> {
        if !replaces {
            return self.ensure_takes_worker(space).await;
        }
        let replacement = self.check_replacement(space, launch).await?;
        self.replace_stopped_worker(daemon, replacement).await
    }

    /// Plan the replacement of the stopped or asleep agent of `space` by
    /// `launch`, refusing it wherever recording the launch or replacing the
    /// agent would.
    async fn check_replacement(
        &self,
        space: &crate::model::Space,
        launch: &Arc<Launch>,
    ) -> Result<Replacement> {
        self.ensure_launch_recordable(launch).await?;
        self.plan_replacement(space).await
    }

    /// Refuse `launch` where recording it would refuse it.
    async fn ensure_launch_recordable(&self, launch: &Arc<Launch>) -> Result<()> {
        let (launch, removals) = (launch.clone(), self.removals.clone());
        (self.store)
            .read(move |tx| ensure_recordable(tx, &*removals, &launch))
            .await
            .map(drop)
    }

    /// The answer to the launch `id`, which the caller starts when `start`:
    /// its entry and the entries waiting.
    pub async fn admission_of(
        &self,
        daemon: &dyn Daemon,
        id: &QueueEntryId,
        start: bool,
    ) -> Result<QueueAdmission> {
        let entries = self.queue_entries(daemon).await?;
        Ok(QueueAdmission {
            start,
            waiting: entries
                .iter()
                .filter(|entry| entry.state == QueueState::Queued)
                .count(),
            entry: entries.into_iter().find(|entry| entry.id == *id),
        })
    }

    /// What blocks the space of the restore `entry` of an agent Shoal
    /// stopped because its space was blocked, by name or URL; none for any
    /// other entry.
    async fn blocked_restore(
        &self,
        daemon: &dyn Daemon,
        entry: &QueueEntry,
    ) -> Result<Option<Vec<String>>> {
        let (Some(workspace), Some(execution)) = (&entry.space_id, &entry.execution) else {
            return Ok(None);
        };
        if !self.restore_waits_for_unblock(workspace, execution) {
            return Ok(None);
        }
        let progress = daemon.space_progress(&SpaceSelector::id(workspace)).await?;
        Ok(Some(
            (progress.iter())
                .flat_map(|progress| progress.all_blockers())
                .map(|(_, blocker)| blocker.name.clone())
                .collect(),
        ))
    }

    /// The spaces the launches of new chats go below, by entry, until they
    /// record their chat: the space that queued the launch, in whose scope
    /// it records the chat; none for a launch the user queued, which
    /// records it where the user's start names.
    pub fn launch_parents(&self, entries: &[QueueEntry]) -> HashMap<QueueEntryId, SpaceId> {
        (entries.iter())
            .filter(|entry| entry.kind == QueueKind::Launch && entry.space().is_none())
            .filter_map(|entry| Some((entry.id.clone(), entry.queued_by.clone()?)))
            .collect()
    }

    /// Entries, the starting ones first and then the waiting ones in rank
    /// order, numbered, with their rank, what blocks them and what held them
    /// back at the last admission pass.
    pub async fn queue_entries(&self, daemon: &dyn Daemon) -> Result<Vec<QueueEntry>> {
        let mut entries = self.ranked_entries(daemon).await?;
        let mut readiness = self.blocker_readiness(daemon).await?;
        let waking = self.restores_waiting_for_updates().await?;
        for entry in &mut entries {
            if let Some(blockers::Readiness::Waiting(blocked_by)) = readiness.remove(&entry.id) {
                entry.blockers = blocked_by;
            }
            entry.waits_for_update = waking.contains(&entry.id);
            if entry.state == QueueState::Queued {
                entry.held_by = self.agent_queue.held_back(&entry.id);
            }
            if let Some(blocked_by) = self.blocked_restore(daemon, entry).await? {
                entry.blockers = blocked_by;
            }
        }
        Ok(entries)
    }

    /// Entries, the starting ones first and then the waiting ones in rank
    /// order, each waiting one numbered and with its rank, then the failed
    /// launches. One that arrived since the rank was read goes last among
    /// those waiting.
    async fn ranked_entries(&self, daemon: &dyn Daemon) -> Result<Vec<QueueEntry>> {
        let ranked = self.ranked_queue(daemon).await?;
        let (entries, failed) = (self.store)
            .run(move |db| Ok((all_entries(db)?, super::failures::entries(db)?)))
            .await?;
        let mut entries = in_rank_order(entries, ranked);
        entries.extend(failed);
        Ok(entries)
    }

    pub async fn queued_plan(&self, id: &QueueEntryId) -> Result<QueuedPlan> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let (entry, plan): (QueueEntry, Option<String>) = db
                    .query_row(
                        &format!("SELECT {ENTRY_COLUMNS},plan FROM agent_queue WHERE id=?1"),
                        [&id],
                        |row| Ok((entry(row)?, row.get("plan")?)),
                    )
                    .optional()?
                    .context("queue entry no longer exists")?;
                let plan = plan.context("queue entry restores an agent; it has no launch plan")?;
                ensure!(
                    entry.state == QueueState::Starting,
                    "queue entry {} is still waiting for a heavy slot",
                    entry.name
                );
                Ok(QueuedPlan { entry, plan })
            })
            .await
    }

    /// Hold the queue gate for an agent taking over `entry`'s slot. Refuses an
    /// entry no longer starting, whose start a stop or completion cancelled.
    /// Returns one to the queue while the system shuts down or the machine
    /// sleeps, or once a stop, reload or unsafe reading withdrew the healthy
    /// readings that admitted it. The guard keeps cancellation out until the
    /// agent has taken the slot.
    pub async fn reserve_queue_entry(
        &self,
        entry: &QueueEntryId,
    ) -> Result<tokio::sync::MutexGuard<'_, ()>> {
        let gate = self.agent_queue.lock_admission().await;
        if self.agent_queue.holds_admission() {
            self.return_start(entry).await?;
            bail!(
                "the system is shutting down or sleeping, or its user is away or paused it; the agent queue starts this agent later"
            );
        }
        if !self.recovery_admission_holds(entry) {
            self.return_start(entry).await?;
            bail!(
                "the healthy readings that admitted this restore no longer hold; it waits in the agent queue again"
            );
        }
        ensure!(
            self.queue_entry_starting(entry).await?,
            "this agent's start from the agent queue was cancelled"
        );
        Ok(gate)
    }

    /// Whether the healthy readings that admitted the restore `entry`, if
    /// they did, still hold: no stop, reload or unsafe reading advanced the
    /// recovery epoch since.
    fn recovery_admission_holds(&self, entry: &QueueEntryId) -> bool {
        (self.agent_queue.take_recovery_start(entry))
            .is_none_or(|epoch| epoch == self.overload_recovery.epoch())
    }

    /// Return the starting entry `id` to the queue, to be admitted again.
    async fn return_start(&self, id: &QueueEntryId) -> Result<()> {
        let id = id.to_owned();
        (self.store)
            .run(move |db| super::store::return_start(db, &id))
            .await
    }

    /// Count an open hold on an entry: a starting one keeps its slot, and a
    /// waiting one stays out of admission, since its holder starts it.
    pub async fn hold_queue_entry(&self, id: &QueueEntryId) -> Result<()> {
        ensure!(
            self.queue_entry_present(id).await?,
            "queue entry is no longer in the agent queue"
        );
        self.agent_queue.hold(id);
        Ok(())
    }

    /// Drop a hold; the entry's grace period starts once none remain.
    pub async fn release_queue_hold(&self, id: &QueueEntryId) -> Result<()> {
        self.agent_queue.release(id);
        let (id, now) = (id.to_owned(), UnixTime::now());
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE agent_queue SET seen_at=?2 WHERE id=?1",
                    params![id, now],
                )?;
                Ok(())
            })
            .await
    }

    /// Whether the entry `id` still waits or starts.
    pub async fn queue_entry_present(&self, id: &QueueEntryId) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| shoal_store::exists(db, "SELECT 1 FROM agent_queue WHERE id=?1", [id]))
            .await
    }

    pub async fn queue_entry_starting(&self, id: &QueueEntryId) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                shoal_store::exists(
                    db,
                    "SELECT 1 FROM agent_queue WHERE id=?1 AND state=?2",
                    params![id, QueueState::Starting],
                )
            })
            .await
    }

    /// Record the space `space` names as the one the launch `id` starts its
    /// agent in: a workspace as [`Self::attach_workspace`] does, a chat as
    /// [`Self::attach_chat`] does.
    pub async fn queue_attach(
        &self,
        daemon: &dyn Daemon,
        id: &QueueEntryId,
        space: &SpaceSelector,
    ) -> Result<()> {
        let space = daemon.space(space).await?;
        let space_id = space.id.clone();
        match space.by_kind() {
            ByKind::Workspace(workspace) => self.attach_workspace(id, workspace).await,
            ByKind::Chat(chat) => self.attach_chat(id, &chat.id).await,
        }?;
        // Its queued agent works its issues.
        daemon.settle_agent(&space_id);
        Ok(())
    }

    /// Record `workspace` as the one the starting entry `id` started its
    /// agent in, which must be the one the launch names.
    async fn attach_workspace(&self, id: &QueueEntryId, workspace: Workspace) -> Result<()> {
        let id = id.to_owned();
        let attached = self
            .store
            .write(move |tx| {
                let started = tx.execute(
                    "UPDATE agent_queue SET space_id=?2
                     WHERE id=?1 AND state=?3 AND opens_workspace_id=?2
                     AND (space_id IS NULL OR space_id=?2)",
                    params![id, workspace.id, QueueState::Starting],
                )?;
                if started == 1 {
                    blockers::replace_launch(tx, &id, &workspace.id)?;
                }
                Ok(started)
            })
            .await?;
        ensure!(
            attached == 1,
            "queue entry no longer starts, or starts its agent in another workspace"
        );
        Ok(())
    }

    /// Whether an agent launch in the space, or one of a chat its scoped
    /// process queued that has not recorded the chat, waits in the queue or
    /// starts.
    pub async fn has_queued_launch(&self, space_id: &SpaceId) -> Result<bool> {
        let space_id = space_id.to_owned();
        self.store
            .run(move |db| {
                shoal_store::exists(
                    db,
                    "SELECT 1 FROM agent_queue WHERE kind=?2 AND (space_id=?1 OR opens_workspace_id=?1
                        OR (scope_space_id=?1 AND space_id IS NULL))",
                    params![space_id, QueueKind::Launch],
                )
            })
            .await
    }

    /// The workspaces in which an agent launch or restore waits in the queue
    /// or starts.
    pub async fn queued_agent_workspaces(&self) -> Result<HashSet<SpaceId>> {
        self.store
            .run(|db| {
                Ok(db
                    .prepare(
                        "SELECT DISTINCT COALESCE(space_id,opens_workspace_id) FROM agent_queue
                         WHERE kind IN (?1,?2)
                            AND COALESCE(space_id,opens_workspace_id) IS NOT NULL",
                    )?
                    .query_map(params![QueueKind::Launch, QueueKind::Restore], |row| {
                        row.get(0)
                    })?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// End a starting entry that cannot start its agent and say why. A launch
    /// in a recorded space stays there as a failed entry, which `shoal resume`
    /// queues again. An entry that already ended, such as with its removed
    /// space, has no slot left to free.
    pub async fn fail_queue_entry(
        &self,
        daemon: &dyn Daemon,
        id: &QueueEntryId,
        error: &str,
    ) -> Result<()> {
        let (entry, text) = (id.to_owned(), error.to_owned());
        let failed = (self.store)
            .write(move |tx| super::failures::end_start(tx, &entry, &text))
            .await?;
        let Some(failed) = failed else {
            return Ok(());
        };
        self.fail_renewal_started_by(id, error).await;
        if let Some(space) = &failed.space {
            daemon.settle_agent(space);
        }
        match (&failed.space, failed.kept) {
            (Some(space), true) => self.tell_launch_failed(daemon, space, error).await,
            (space, _) => {
                (self.tell_queue_failed(daemon, &failed.name, space.as_ref(), error)).await;
            }
        }
        self.agent_queue.wake();
        Ok(())
    }

    /// Tell that the launch named `name`, of `space` when it opened one,
    /// did not start from the queue with `error`.
    async fn tell_queue_failed(
        &self,
        daemon: &dyn Daemon,
        name: &SpaceName,
        space: Option<&SpaceId>,
        error: &str,
    ) {
        // A launch that opened no workspace has its name alone, which no
        // workspace's identifies since names repeat.
        let (kind, message) = (
            AlertKind::QueueFailed,
            format!("{name} did not start from the agent queue: {error}"),
        );
        match space {
            Some(space) => daemon.notify_space(name, space, kind, message).await,
            None => daemon.notify(Some(name), kind, message).await,
        }
    }

    /// The agent a queued launch started takes over the slot its entry kept,
    /// and what made the launch. Only that launch's agent names the entry, so
    /// another agent registering in the same workspace leaves it starting. The
    /// workspace is recorded as the one the launch started in as the entry
    /// goes, so a client that no longer finds the entry finds the workspace.
    pub async fn consume_queue_entry(
        &self,
        id: &QueueEntryId,
        workspace_id: &SpaceId,
    ) -> Option<LaunchSource> {
        let (entry, workspace) = (id.to_owned(), workspace_id.to_owned());
        let consumed = self
            .store
            .write(move |tx| {
                let source: Option<Option<String>> = tx
                    .query_row(
                        "DELETE FROM agent_queue WHERE id=?1 AND space_id=?2 AND state=?3
                         RETURNING source",
                        params![entry, workspace, QueueState::Starting],
                        |row| row.get(0),
                    )
                    .optional()?;
                let Some(source) = source else {
                    return Ok(None);
                };
                record_started(tx, &entry, &workspace)?;
                Ok(source)
            })
            .await;
        let source = consumed.unwrap_or_else(|error| {
            crate::log!("agent queue: {error:#}");
            None
        })?;
        // The entry is consumed whatever its source holds: one this build
        // cannot read is left out of the agent's start.
        serde_json::from_str(&source)
            .inspect_err(|error| crate::log!("launch source of {id} not read: {error:#}"))
            .ok()
    }

    /// The workspace the launch `entry`, which left the queue, started its
    /// agent in, while it is recorded.
    pub async fn started_launch(&self, entry: &QueueEntryId) -> Result<Option<SpaceId>> {
        let entry = entry.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT workspace_id FROM started_launches
                            WHERE entry_id=?1 AND started_at>?2",
                        params![entry, crate::time::UnixTime::now() - STARTED_KEPT],
                        |row| row.get(0),
                    )
                    .optional()?)
            })
            .await
    }
}

/// Every entry of the queue, in arrival order.
fn all_entries(db: &rusqlite::Connection) -> Result<Vec<QueueEntry>> {
    Ok(db
        .prepare(&format!(
            "SELECT {ENTRY_COLUMNS} FROM agent_queue ORDER BY seq"
        ))?
        .query_map([], entry)?
        .collect::<rusqlite::Result<_>>()?)
}

/// `entries` with the starting ones first and then the waiting ones in the
/// order of `ranked`, each waiting one numbered and with its rank.
fn in_rank_order(mut entries: Vec<QueueEntry>, ranked: Vec<Ranked>) -> Vec<QueueEntry> {
    let mut ranked: HashMap<QueueEntryId, (usize, QueueRank)> = (ranked.into_iter())
        .enumerate()
        .map(|(place, ranked)| (ranked.id, (place, ranked.rank)))
        .collect();
    entries.sort_by_key(|entry| {
        let place = (ranked.get(&entry.id)).map_or(usize::MAX, |(place, _)| *place);
        (entry.state == QueueState::Queued, place)
    });
    let waiting = (entries.iter_mut()).filter(|entry| entry.state == QueueState::Queued);
    for (position, entry) in waiting.enumerate() {
        entry.position = Some(position + 1);
        entry.rank = ranked.remove(&entry.id).map(|(_, rank)| rank);
    }
    entries
}

/// How long the workspace a launch started its agent in stays recorded
/// after the launch left the queue, for a client waiting to attach to it.
const STARTED_KEPT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Record that the launch `entry` started its agent in `workspace`, in the
/// transaction that takes it out of the queue, forgetting launches that
/// started long ago.
fn record_started(
    tx: &rusqlite::Transaction<'_>,
    entry: &QueueEntryId,
    workspace: &SpaceId,
) -> Result<()> {
    let now = crate::time::UnixTime::now();
    tx.execute(
        "DELETE FROM started_launches WHERE started_at<=?1",
        [now - STARTED_KEPT],
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO started_launches(entry_id,workspace_id,started_at)
            VALUES (?1,?2,?3)",
        params![entry, workspace, now],
    )?;
    Ok(())
}
