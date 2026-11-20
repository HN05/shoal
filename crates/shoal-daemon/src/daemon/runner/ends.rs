//! The load of the runner's jobs as admission counts it, and the process
//! proof of whether a job's execution still runs.
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::params;

use crate::{
    daemon::{capacity::Activity, manager::Manager},
    model::QueuedJob,
    process::{execution::Processes, identity},
    state::{ExecutionState, QueueKind, QueueState},
};

/// A job starting or running, as admission counts it.
struct ActiveJob {
    /// The ID of its queue entry, which its claim keeps.
    id: QueueEntryId,
    repository_id: RepositoryId,
    /// The workspace it runs in, once claimed.
    workspace_id: Option<SpaceId>,
    /// When its runner's execution started.
    launched_at: Option<crate::time::UnixTime>,
    job: QueuedJob,
}

impl Manager {
    /// Whether the execution `id` of a job in `workspace_id` may still run.
    /// One whose wrapper has not reattached since a daemon restart, which
    /// restart recovery leaves to its owner since a job's runner has no agent
    /// record, counts as ended, and is forgotten, once the shared process
    /// proof finds nothing of it: no wrapper, and no process carrying its
    /// marker or in its process group. Verified survivors are stopped and
    /// checked again on the next pass; a launch whose identity was never
    /// recorded proves nothing and keeps the job.
    pub(super) async fn job_execution_runs(
        &self,
        workspace_id: &SpaceId,
        id: &ExecutionId,
    ) -> Result<bool> {
        let workspace = self.workspace_by_id(workspace_id).await?;
        let execution = self
            .workspace_executions(workspace_id)
            .await?
            .into_iter()
            .find(|execution| execution.id == *id);
        let Some(execution) = execution else {
            return Ok(false);
        };
        if execution.state != ExecutionState::Unknown {
            return Ok(true);
        }
        let scan = identity::scan(HashSet::from([id.to_owned()])).await?;
        let agents = self.agent_wrappers().await?;
        let processes = Processes::inspect(&execution, &workspace.path, &scan, &agents).await?;
        if processes.wrapper.is_some() || !processes.visibility_complete() {
            return Ok(true);
        }
        if !processes.command_stopped() {
            processes.stop_owned().await?;
            return Ok(true);
        }
        self.forget_execution(id).await?;
        Ok(false)
    }

    /// The load of each job starting or running: what it is expected to
    /// use, what its workspace used at the latest usage sample, and how long
    /// ago its runner started; one whose runner has not started yet has just
    /// started, so it holds back the next start until it runs. A job whose
    /// runner ended adds nothing, however long its workspace remains.
    pub(crate) async fn job_activities(&self) -> Result<Vec<Activity>> {
        let now = crate::time::UnixTime::now();
        let mut activities = Vec::new();
        for job in self.starting_or_running_jobs().await? {
            activities.push(Activity::Job {
                age: now.duration_since(job.launched_at.unwrap_or(now)),
                cost: (self.runner.job_cost(&job.repository_id, &job.job)).await?,
                memory: (job.workspace_id.and_then(|id| self.workspace_usage(&id)))
                    .map(|usage| usage.memory_bytes),
            });
        }
        Ok(activities)
    }

    /// The jobs admitted and not yet claimed, and the claimed jobs whose
    /// runner's execution is recorded, running or awaiting reattachment, or
    /// is still to start: under a claim that runs, or, after a restart cut
    /// one short, once its workspace runs an execution the next pass adopts.
    async fn starting_or_running_jobs(&self) -> Result<Vec<ActiveJob>> {
        let rows: Vec<(ActiveJob, bool)> = self
            .store
            .run(|db| {
                Ok(db
                    .prepare(
                        "SELECT id,repository_id,NULL,NULL,job,0 FROM agent_queue
                             WHERE kind=?1 AND state=?2 AND id NOT IN (SELECT id FROM ci_jobs)
                         UNION ALL SELECT id,repository_id,workspace_id,launched_at,job,
                                 execution_id IS NULL
                                 AND workspace_id NOT IN (SELECT space_id FROM executions)
                             FROM ci_jobs
                             WHERE execution_id IS NULL
                                 OR execution_id IN (SELECT id FROM executions)",
                    )?
                    .query_map(params![QueueKind::Job, QueueState::Starting], |row| {
                        let job = ActiveJob {
                            id: row.get(0)?,
                            repository_id: row.get(1)?,
                            workspace_id: row.get(2)?,
                            launched_at: row.get(3)?,
                            job: queued_job(row, 4)?,
                        };
                        Ok((job, row.get(5)?))
                    })?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await?;
        Ok(rows
            .into_iter()
            .filter(|(job, unlaunched)| !unlaunched || self.runner.jobs().claiming(&job.id))
            .map(|(job, _)| job)
            .collect())
    }
}

/// The job a row holds as JSON in its column `index`.
fn queued_job(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<QueuedJob> {
    serde_json::from_str(&row.get::<_, String>(index)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}
