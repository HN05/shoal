//! What the runner's jobs load the machine with, as admission counts it:
//! each job starting or running, with what it is expected to use, what its
//! workspace used at the latest usage sample, and how long ago its runner
//! started. One whose runner has not started yet has just started, so it
//! holds back the next start until it runs; a job whose runner ended adds
//! nothing, however long its workspace remains.
use std::time::Duration;

use anyhow::Result;
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::model::QueuedJob;

use super::JobCost;
use crate::{Runner, daemon::Daemon, time::UnixTime};

/// A job starting or running, as admission counts it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JobLoad {
    /// How long ago its runner started; none yet for one starting.
    pub age: Duration,
    /// What its runs are expected to use.
    pub cost: JobCost,
    /// What its workspace used at the latest usage sample.
    pub memory: Option<u64>,
}

/// A job starting or running, as the records hold it.
struct ActiveJob {
    repository_id: RepositoryId,
    /// The workspace it runs in, once claimed.
    workspace_id: Option<SpaceId>,
    /// When its runner's execution started.
    launched_at: Option<UnixTime>,
    job: QueuedJob,
}

impl Runner {
    /// The load of each job starting or running.
    pub async fn job_loads(&self, daemon: &dyn Daemon) -> Result<Vec<JobLoad>> {
        let now = UnixTime::now();
        let mut loads = Vec::new();
        for job in self.starting_or_running_jobs().await? {
            loads.push(JobLoad {
                age: now.duration_since(job.launched_at.unwrap_or(now)),
                cost: self.job_cost(&job.repository_id, &job.job).await?,
                memory: (job.workspace_id.and_then(|id| daemon.workspace_usage(&id)))
                    .map(|usage| usage.memory_bytes),
            });
        }
        Ok(loads)
    }

    /// The jobs admitted and not yet claimed, and the claimed jobs whose
    /// runner's execution is recorded, running or awaiting reattachment, or
    /// is still to start: under a claim that runs, or, after a restart cut
    /// one short, once its workspace runs an execution the next pass adopts.
    async fn starting_or_running_jobs(&self) -> Result<Vec<ActiveJob>> {
        let records = self.records.clone();
        let (admitted, claimed, executions) = self
            .store
            .read(move |tx| {
                let admitted = records.admitted_jobs(tx)?;
                let executions = records.recorded_executions(tx)?;
                Ok((admitted, super::claimed::claims(tx)?, executions))
            })
            .await?;
        let claimed_ids: std::collections::HashSet<_> =
            claimed.iter().map(|job| job.id.clone()).collect();
        let starting = (admitted.into_iter())
            .filter(|(id, _, _)| !claimed_ids.contains(id))
            .map(|(_, repository_id, job)| ActiveJob {
                repository_id,
                workspace_id: None,
                launched_at: None,
                job,
            });
        let running = (claimed.into_iter())
            .filter(|job| (job.execution_id.as_ref()).is_none_or(|id| executions.ids.contains(id)))
            .filter(|job| {
                let unlaunched =
                    job.execution_id.is_none() && !executions.spaces.contains(&job.workspace_id);
                !unlaunched || self.jobs.claiming(&job.id)
            })
            .map(|job| ActiveJob {
                repository_id: job.repository_id,
                workspace_id: Some(job.workspace_id),
                launched_at: job.launched_at,
                job: job.job,
            });
        Ok(starting.chain(running).collect())
    }
}

#[cfg(test)]
mod tests;
