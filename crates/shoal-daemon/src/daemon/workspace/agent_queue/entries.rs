//! Operations on one entry: adding a launch, listing entries, and a starting
//! entry's way from its reservation to the agent that takes its slot or its
//! failure.
use crate::client::repository_ids::RepositoryId;
use crate::client::space_ids::SpaceId;
use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, params};

use super::{
    blockers,
    dispatch::Start,
    rank::Ranked,
    store::{ENTRY_COLUMNS, Launch, entry, insert_launch},
};
use crate::client::{
    queue::QueueEntryId,
    space_names::{SpaceName, SpaceSelector},
    time::UnixTime,
};
use crate::{
    daemon::{notifications::NotificationKind, scope::Caller, workspace::Manager},
    model::{QueueAdmission, QueueEntry, QueueRank, QueueRequest, QueuedPlan},
    state::{QueueKind, QueueState},
};

impl Manager {
    /// Admit `request` to a free slot or queue it. A launch that no limit
    /// applies to and that nothing blocks starts untracked; one
    /// admitted here starts in the caller, except one the daemon starts,
    /// which a launch in an existing workspace is unless its caller starts
    /// it. A launch in an existing workspace is admitted only while the
    /// workspace has no agent of its own.
    pub async fn queue_add(&self, request: QueueRequest) -> Result<QueueAdmission> {
        self.queue_add_as(request, None).await
    }

    /// [`Self::queue_add`] for `caller`. A scoped caller's launch of a new
    /// workspace opens it in the caller's scope, and the daemon starts it,
    /// as it starts a launch in an existing workspace.
    pub(in crate::daemon) async fn queue_add_as(
        &self,
        request: QueueRequest,
        caller: Option<&Caller>,
    ) -> Result<QueueAdmission> {
        ensure!(
            !request.now || request.blockers.is_empty(),
            "a launch that starts now cannot be blocked"
        );
        let repository = self.repository(&request.repository).await?;
        let target = match &request.workspace {
            Some(workspace) => Some(self.launch_target(workspace, &repository.id).await?),
            None => None,
        };
        let scope = match target {
            Some(_) => None,
            None => caller.map(|caller| caller.space_id.clone()),
        };
        let _gate = self.agent_queue.lock_admission().await;
        if let Some(workspace) = &target {
            self.ensure_takes_worker(workspace).await?;
        }
        let target = target.map(|workspace| workspace.id);
        // Whether any limit applies does not depend on the slots in use.
        let limits = self
            .agent_limits(vec![repository.id.clone()], &[], &[])
            .await?;
        let detached = (target.is_some() && !request.starts_in_caller) || scope.is_some();
        // A launch in an existing workspace keeps its entry, which holds the
        // workspace's one agent until it registers.
        let opens_new = target.is_none() && scope.is_none();
        if opens_new && request.blockers.is_empty() && !limits.apply_to(&repository.id) {
            return Ok(QueueAdmission {
                start: true,
                entry: None,
                waiting: 0,
            });
        }
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
        let launch = Launch {
            id: id.clone(),
            repository: repository.id,
            request,
            state,
            opens: target,
            scope,
        };
        let now = launch.request.now;
        let removing = self.chat_removals.clone();
        self.store
            .write(move |tx| insert_launch(tx, &launch, &removing))
            .await?;
        let admitted = now || self.admit_waiting(Some(&id)).await?;
        // Read the response before a detached worker can take the entry.
        let entries = self.queue_entries().await?;
        if detached && admitted {
            self.start_launches(vec![start], None).await?;
        }
        Ok(QueueAdmission {
            start: admitted && !detached,
            waiting: entries
                .iter()
                .filter(|entry| entry.state == QueueState::Queued)
                .count(),
            entry: entries.into_iter().find(|entry| entry.id == id),
        })
    }

    /// The workspace of `repository_id` a launch opens; the insertion
    /// requires it ready.
    async fn launch_target(
        &self,
        workspace: &SpaceSelector,
        repository_id: &RepositoryId,
    ) -> Result<crate::model::Workspace> {
        let workspace = self.workspace(workspace).await?;
        Self::ensure_holds_agents(&workspace)?;
        ensure!(
            workspace.repository_id == *repository_id,
            "workspace {} belongs to another repository",
            workspace.name
        );
        Ok(workspace)
    }

    /// What blocks the space of the restore `entry` of an agent Shoal
    /// stopped because its space was blocked, by name or URL; none for any
    /// other entry.
    async fn blocked_restore(&self, entry: &QueueEntry) -> Result<Option<Vec<String>>> {
        let (Some(workspace), Some(execution)) = (&entry.workspace_id, &entry.execution) else {
            return Ok(None);
        };
        if !self.restore_waits_for_unblock(workspace, execution) {
            return Ok(None);
        }
        let progress = self.space_progress(&SpaceSelector::id(workspace)).await?;
        Ok(Some(
            (progress.iter())
                .flat_map(|progress| progress.all_blockers())
                .map(|(_, blocker)| blocker.name.clone())
                .collect(),
        ))
    }

    /// Entries, the starting ones first and then the waiting ones in rank
    /// order, numbered, with their rank, what blocks them and what held them
    /// back at the last admission pass.
    pub async fn queue_entries(&self) -> Result<Vec<QueueEntry>> {
        let mut entries = self.ranked_entries().await?;
        let mut readiness = self.blocker_readiness().await?;
        let waking = self.restores_waiting_for_updates().await?;
        for entry in &mut entries {
            if let Some(blockers::Readiness::Waiting(blocked_by)) = readiness.remove(&entry.id) {
                entry.blockers = blocked_by;
            }
            entry.waits_for_update = waking.contains(&entry.id);
            if entry.state == QueueState::Queued {
                entry.held_by = self.agent_queue.held_back(&entry.id);
            }
            if let Some(blocked_by) = self.blocked_restore(entry).await? {
                entry.blockers = blocked_by;
            }
        }
        Ok(entries)
    }

    /// Entries, the starting ones first and then the waiting ones in rank
    /// order, each waiting one numbered and with its rank. One that arrived
    /// since the rank was read goes last.
    async fn ranked_entries(&self) -> Result<Vec<QueueEntry>> {
        let ranked = self.ranked_queue().await?;
        let entries = self.store.run(|db| all_entries(db)).await?;
        Ok(in_rank_order(entries, ranked))
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
                    "queue entry {} is still waiting for a slot",
                    entry.name
                );
                Ok(QueuedPlan { entry, plan })
            })
            .await
    }

    /// Hold the queue gate for an agent taking over `entry`'s slot, refusing
    /// an entry no longer starting, whose start a stop or completion
    /// cancelled, and returning one to the queue while the system shuts
    /// down, which admitted it before. The guard keeps cancellation out until
    /// the agent has taken the slot.
    pub(crate) async fn reserve_queue_entry(
        &self,
        entry: &QueueEntryId,
    ) -> Result<tokio::sync::MutexGuard<'_, ()>> {
        let gate = self.agent_queue.lock_admission().await;
        if self.agent_queue.suspended() {
            self.return_start(entry).await?;
            bail!("the system is shutting down; the agent queue starts this agent later");
        }
        ensure!(
            self.queue_entry_starting(entry).await?,
            "this agent's start from the agent queue was cancelled"
        );
        Ok(gate)
    }

    /// Return the starting entry `id` to the queue, to be admitted again.
    async fn return_start(&self, id: &QueueEntryId) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE agent_queue SET state=?2 WHERE id=?1 AND state=?3",
                    params![id, QueueState::Queued, QueueState::Starting],
                )?;
                Ok(())
            })
            .await
    }

    /// Count an open hold on a starting entry.
    pub(crate) async fn hold_queue_entry(&self, id: &QueueEntryId) -> Result<()> {
        ensure!(
            self.queue_entry_starting(id).await?,
            "queue entry is no longer starting"
        );
        self.agent_queue.hold(id);
        Ok(())
    }

    /// Drop a hold; the entry's grace period starts once none remain.
    pub(crate) async fn release_queue_hold(&self, id: &QueueEntryId) -> Result<()> {
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

    pub(crate) async fn queue_entry_starting(&self, id: &QueueEntryId) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                crate::daemon::store::exists(
                    db,
                    "SELECT 1 FROM agent_queue WHERE id=?1 AND state=?2",
                    params![id, QueueState::Starting],
                )
            })
            .await
    }

    /// Record the workspace a starting entry opened, or the existing one a
    /// waiting launch opens once admitted, which the daemon then starts as
    /// it starts any launch in an existing workspace.
    pub async fn queue_attach(&self, id: &QueueEntryId, workspace: &SpaceSelector) -> Result<()> {
        let workspace = self.workspace(workspace).await?;
        let opens = Self::ensure_holds_agents(&workspace).is_ok();
        let id = id.to_owned();
        let attached = self
            .store
            .write(move |tx| {
                let started = tx.execute(
                    "UPDATE agent_queue SET workspace_id=?2
                     WHERE id=?1 AND state=?3 AND (opens_workspace_id IS NULL OR opens_workspace_id=?2)
                     AND (workspace_id IS NULL OR workspace_id=?2)",
                    params![id, workspace.id, QueueState::Starting],
                )?;
                if started == 1 {
                    blockers::replace_launch(tx, &id, &workspace.id)?;
                }
                let waiting = match opens && started == 0 {
                    true => tx.execute(
                        "UPDATE agent_queue SET opens_workspace_id=?2
                         WHERE id=?1 AND state=?3 AND kind=?4 AND repository_id=?5
                         AND workspace_id IS NULL AND opens_workspace_id IS NULL",
                        params![
                            id,
                            workspace.id,
                            QueueState::Queued,
                            QueueKind::Launch,
                            workspace.repository_id
                        ],
                    )?,
                    false => 0,
                };
                Ok(started + waiting)
            })
            .await?;
        ensure!(
            attached == 1,
            "queue entry no longer starts or waits, or starts its agent in another workspace"
        );
        Ok(())
    }

    /// Whether an agent launch in the space, or one its scoped process
    /// queued that has not opened its workspace, waits in the queue or
    /// starts.
    pub(crate) async fn has_queued_launch(&self, space_id: &SpaceId) -> Result<bool> {
        let space_id = space_id.to_owned();
        self.store
            .run(move |db| {
                crate::daemon::store::exists(
                    db,
                    "SELECT 1 FROM agent_queue WHERE kind=?2 AND (workspace_id=?1 OR opens_workspace_id=?1
                        OR (scope_space_id=?1 AND workspace_id IS NULL))",
                    params![space_id, QueueKind::Launch],
                )
            })
            .await
    }

    /// The caller as which the launch of the starting entry `id` opens its
    /// workspace: the space whose scoped process queued it, or none for a
    /// launch of the user's.
    pub(in crate::daemon) async fn queued_scope(
        &self,
        id: &QueueEntryId,
    ) -> Result<Option<Caller>> {
        let id = id.to_owned();
        let scope: Option<Option<SpaceId>> = self
            .store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT scope_space_id FROM agent_queue WHERE id=?1 AND state=?2",
                        params![id, QueueState::Starting],
                        |row| row.get(0),
                    )
                    .optional()?)
            })
            .await?;
        let scope = scope.context("the queue entry no longer starts")?;
        Ok(scope.map(|space_id| Caller {
            space_id,
            execution: None,
        }))
    }

    /// The workspaces in which an agent launch or restore waits in the queue
    /// or starts.
    pub(crate) async fn queued_agent_workspaces(&self) -> Result<HashSet<SpaceId>> {
        self.store
            .run(|db| {
                Ok(db
                    .prepare(
                        "SELECT DISTINCT COALESCE(workspace_id,opens_workspace_id) FROM agent_queue
                         WHERE kind IN (?1,?2)
                            AND COALESCE(workspace_id,opens_workspace_id) IS NOT NULL",
                    )?
                    .query_map(params![QueueKind::Launch, QueueKind::Restore], |row| {
                        row.get(0)
                    })?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// End a starting entry that cannot start its agent and say why. An entry
    /// that already ended, such as with its removed workspace, has no slot
    /// left to free.
    pub async fn fail_queue_entry(&self, id: &QueueEntryId, error: &str) -> Result<()> {
        let entry_id = id.to_owned();
        let entry = self
            .store
            .run(move |db| {
                let entry: Option<(SpaceName, bool, Option<SpaceId>)> = db
                    .query_row(
                        "SELECT name, state=?2, COALESCE(workspace_id,opens_workspace_id)
                            FROM agent_queue WHERE id=?1",
                        params![entry_id, QueueState::Starting],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?;
                db.execute(
                    "DELETE FROM agent_queue WHERE id=?1 AND state=?2",
                    params![entry_id, QueueState::Starting],
                )?;
                Ok(entry)
            })
            .await?;
        let (name, workspace) = match entry {
            None => return Ok(()),
            Some((_, false, _)) => bail!("queue entry is no longer starting"),
            Some((name, true, workspace)) => (name, workspace),
        };
        self.fail_renewal_started_by(id, error).await;
        // A launch that opened no workspace has its name alone, which no
        // workspace's identifies since names repeat.
        let (kind, message) = (
            NotificationKind::QueueFailed,
            format!("{name} did not start from the agent queue: {error}"),
        );
        match workspace {
            Some(workspace) => self.notify_space(&name, &workspace, kind, message).await,
            None => self.notify(Some(&name), kind, message).await,
        }
        self.agent_queue.wake();
        Ok(())
    }

    /// The agent a queued launch started takes over the slot its entry kept.
    /// Only that launch's agent names the entry, so another agent registering
    /// in the same workspace leaves it starting. The workspace is recorded
    /// as the one the launch started in as the entry goes, so a client that
    /// no longer finds the entry finds it.
    pub(crate) async fn consume_queue_entry(&self, id: &QueueEntryId, workspace_id: &SpaceId) {
        let (entry, workspace) = (id.to_owned(), workspace_id.to_owned());
        let consumed = self
            .store
            .write(move |tx| {
                let consumed = tx.execute(
                    "DELETE FROM agent_queue WHERE id=?1 AND workspace_id=?2 AND state=?3",
                    params![entry, workspace, QueueState::Starting],
                )?;
                if consumed == 1 {
                    record_started(tx, &entry, &workspace)?;
                }
                Ok(())
            })
            .await;
        if let Err(error) = consumed {
            crate::daemon::log!("agent queue: {error:#}");
        }
    }

    /// The workspace the launch `entry`, which left the queue, started its
    /// agent in, while it is recorded.
    pub(crate) async fn started_launch(&self, entry: &QueueEntryId) -> Result<Option<SpaceId>> {
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
