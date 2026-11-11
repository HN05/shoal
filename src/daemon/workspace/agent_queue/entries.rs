//! Operations on one entry: adding a launch, listing entries, and a starting
//! entry's way from its reservation to the agent that takes its slot or its
//! failure.
use crate::client::repository_ids::RepositoryId;
use crate::client::space_ids::SpaceId;
use std::collections::HashSet;

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, params};

use super::{
    dependencies,
    dispatch::Start,
    store::{ENTRY_COLUMNS, Launch, entry, insert_launch},
};
use crate::client::{
    queue::QueueEntryId,
    space_names::{SpaceName, SpaceSelector},
    time::UnixTime,
};
use crate::{
    daemon::{notifications::NotificationKind, workspace::Manager},
    model::{QueueAdmission, QueueEntry, QueueRequest, QueuedPlan},
    state::{QueueKind, QueueState},
};

impl Manager {
    /// Admit `request` to a free slot or queue it. A launch that no limit
    /// applies to and that waits for no other work starts untracked; one
    /// admitted here starts in the caller, except a launch in an existing
    /// workspace, which the daemon always starts.
    pub async fn queue_add(&self, request: QueueRequest) -> Result<QueueAdmission> {
        ensure!(
            !request.now || request.after.is_empty(),
            "a launch that starts now cannot wait for other work"
        );
        let repository = self.repository(&request.repository).await?;
        let target = match &request.workspace {
            Some(workspace) => Some(self.launch_target(workspace, &repository.id).await?),
            None => None,
        };
        let _gate = self.agent_queue.lock_admission().await;
        // Whether any limit applies does not depend on the slots in use.
        let limits = self
            .agent_limits(vec![repository.id.clone()], &[], &[])
            .await?;
        if target.is_none() && request.after.is_empty() && !limits.apply_to(&repository.id) {
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
        };
        let detached = launch.opens.is_some();
        let now = launch.request.now;
        self.store
            .write(move |tx| insert_launch(tx, &launch))
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

    /// The ID of the workspace of `repository_id` a launch opens; the
    /// insertion requires it ready.
    async fn launch_target(
        &self,
        workspace: &SpaceSelector,
        repository_id: &RepositoryId,
    ) -> Result<SpaceId> {
        let workspace = self.workspace(workspace).await?;
        Self::ensure_holds_agents(&workspace)?;
        ensure!(
            workspace.repository_id == *repository_id,
            "workspace {} belongs to another repository",
            workspace.name
        );
        Ok(workspace.id)
    }

    /// Entries in queue order, waiting ones numbered and with the work they
    /// wait for and what held them back at the last admission pass.
    pub async fn queue_entries(&self) -> Result<Vec<QueueEntry>> {
        let mut entries = self.numbered_entries().await?;
        let mut readiness = self.dependency_readiness().await?;
        let waking = self.restores_waiting_for_updates().await?;
        for entry in &mut entries {
            if let Some(dependencies::Readiness::Waiting(work)) = readiness.remove(&entry.id) {
                entry.waiting_for = work;
            }
            entry.waits_for_update = waking.contains(&entry.id);
            if entry.state == QueueState::Queued {
                entry.held_by = self.agent_queue.held_back(&entry.id);
            }
        }
        Ok(entries)
    }

    async fn numbered_entries(&self) -> Result<Vec<QueueEntry>> {
        self.store
            .run(|db| {
                let mut entries = db
                    .prepare(&format!(
                        "SELECT {ENTRY_COLUMNS} FROM agent_queue ORDER BY seq"
                    ))?
                    .query_map([], entry)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let mut position = 0;
                for entry in &mut entries {
                    if entry.state == QueueState::Queued {
                        position += 1;
                        entry.position = Some(position);
                    }
                }
                Ok(entries)
            })
            .await
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
                     WHERE id=?1 AND state=?3 AND (opens_workspace_id IS NULL OR opens_workspace_id=?2)",
                    params![id, workspace.id, QueueState::Starting],
                )?;
                if started == 1 {
                    dependencies::replace_launch(tx, &id, &workspace.id)?;
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

    /// Whether an agent launch in the workspace waits in the queue or starts.
    pub(crate) async fn has_queued_launch(&self, workspace_id: &SpaceId) -> Result<bool> {
        let workspace_id = workspace_id.to_owned();
        self.store
            .run(move |db| {
                crate::daemon::store::exists(
                    db,
                    "SELECT 1 FROM agent_queue WHERE (workspace_id=?1 OR opens_workspace_id=?1) AND kind=?2",
                    params![workspace_id, QueueKind::Launch],
                )
            })
            .await
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
                let entry: Option<(SpaceName, bool)> = db
                    .query_row(
                        "SELECT name, state=?2 FROM agent_queue WHERE id=?1",
                        params![entry_id, QueueState::Starting],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                db.execute(
                    "DELETE FROM agent_queue WHERE id=?1 AND state=?2",
                    params![entry_id, QueueState::Starting],
                )?;
                Ok(entry)
            })
            .await?;
        let name = match entry {
            None => return Ok(()),
            Some((_, false)) => bail!("queue entry is no longer starting"),
            Some((name, true)) => name,
        };
        self.fail_renewal_started_by(id, error).await;
        self.notify(
            Some(&name),
            NotificationKind::QueueFailed,
            format!("{name} did not start from the agent queue: {error}"),
        )
        .await;
        self.agent_queue.wake();
        Ok(())
    }

    /// The agent a queued launch started takes over the slot its entry kept.
    /// Only that launch's agent names the entry, so another agent registering
    /// in the same workspace leaves it starting.
    pub(crate) async fn consume_queue_entry(&self, id: &QueueEntryId, workspace_id: &SpaceId) {
        let (id, workspace_id) = (id.to_owned(), workspace_id.to_owned());
        let consumed = self
            .store
            .run(move |db| {
                Ok(db.execute(
                    "DELETE FROM agent_queue WHERE id=?1 AND workspace_id=?2 AND state=?3",
                    params![id, workspace_id, QueueState::Starting],
                )?)
            })
            .await;
        if let Err(error) = consumed {
            crate::daemon::log!("agent queue: {error:#}");
        }
    }
}
