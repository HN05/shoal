//! What each repository's CI jobs use: the peak resident memory of every
//! run, read from its workspace's usage samples and kept per repository,
//! workflow and job name, the cost a waiting or starting run of a job counts
//! at in admission: the highest peak among its runs that ended within
//! [`RECENT_FOR`] and its latest [`RECENT_RUNS`] runs, none before it has
//! run. A run cut short, as by load protection, uses less than a whole one,
//! so it can raise a job's cost but never lower it below a whole run's
//! still in the window; a peak lowers it only once it ages out.
use std::time::Duration;

use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use shoal_daemon_base::jobs::JobCost;
use shoal_ids::{queue_entry_ids::QueueEntryId, repository_ids::RepositoryId};
use shoal_protocol::model::QueuedJob;
use shoal_store::exists;

use super::ClaimedJob;
use crate::{Runner, daemon::Daemon, time::UnixTime};

/// How many of a job's latest runs its cost reads, whatever their age, so
/// a job that has not run for a while keeps one.
const RECENT_RUNS: i64 = 5;
/// How long after it ended a run counts towards its job's cost, however
/// many runs ended since.
const RECENT_FOR: Duration = Duration::from_secs(3 * 24 * 60 * 60);

impl Runner {
    /// Raise each claimed job's peak memory to what its workspace's latest
    /// usage sample shows.
    pub async fn sample_job_usage(&self, daemon: &dyn Daemon) -> Result<()> {
        for job in self.claimed_jobs().await? {
            let Some(usage) = daemon.workspace_usage(&job.workspace_id) else {
                continue;
            };
            self.raise_job_peak(&job.id, usage.memory_bytes).await?;
        }
        Ok(())
    }

    /// Raise the peak memory of the claimed job `id` to `memory` bytes.
    pub(super) async fn raise_job_peak(&self, id: &QueueEntryId, memory: u64) -> Result<()> {
        let (id, memory) = (id.clone(), i64::try_from(memory).unwrap_or(i64::MAX));
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE ci_jobs SET peak_memory=max(coalesce(peak_memory,0),?2)
                     WHERE id=?1",
                    params![id, memory],
                )?;
                Ok(())
            })
            .await
    }

    /// Keep the peak memory of the ended `job` as its latest run, unless no
    /// sample saw it run or it is kept already, as before a removal that is
    /// tried again, forgetting the runs its cost no longer reads.
    pub async fn record_job_usage(&self, job: &ClaimedJob) -> Result<()> {
        let (id, repository) = (job.id.clone(), job.repository_id.clone());
        let (workflow, name) = key(&job.job);
        let now = UnixTime::now();
        self.store
            .write(move |tx| {
                let peak: Option<Option<i64>> = tx
                    .query_row(
                        "SELECT peak_memory FROM ci_jobs WHERE id=?1 AND usage_recorded=0",
                        [&id],
                        |row| row.get(0),
                    )
                    .optional()?;
                let Some(Some(memory)) = peak else {
                    return Ok(());
                };
                tx.execute("UPDATE ci_jobs SET usage_recorded=1 WHERE id=?1", [&id])?;
                tx.execute(
                    "INSERT INTO ci_job_usage(repository_id,workflow,name,ended_at,peak_memory)
                     VALUES (?1,?2,?3,?4,?5)",
                    params![repository, workflow, name, now, memory],
                )?;
                tx.execute(
                    &format!(
                        "DELETE FROM ci_job_usage WHERE {JOB}
                         AND rowid NOT IN (SELECT rowid FROM ci_job_usage WHERE {})",
                        recent_runs()
                    ),
                    params![repository, workflow, name, RECENT_RUNS, now - RECENT_FOR],
                )?;
                Ok(())
            })
            .await
    }

    /// Whether a sample saw the claimed job `id` run and no run of it is
    /// kept or discarded yet.
    pub(super) async fn job_usage_unkept(&self, id: &QueueEntryId) -> Result<bool> {
        let id = id.clone();
        self.store
            .run(move |db| {
                exists(
                    db,
                    "SELECT 1 FROM ci_jobs
                     WHERE id=?1 AND usage_recorded=0 AND peak_memory IS NOT NULL",
                    [id],
                )
            })
            .await
    }

    /// Keep no run of the claimed job `id` for its job's cost, as for one
    /// whose runner never took it and so used nothing a run does.
    pub(super) async fn discard_job_usage(&self, id: &QueueEntryId) -> Result<()> {
        let id = id.clone();
        self.store
            .run(move |db| {
                db.execute("UPDATE ci_jobs SET usage_recorded=1 WHERE id=?1", [id])?;
                Ok(())
            })
            .await
    }

    /// What a run of `job` in `repository_id` is expected to use: the most
    /// memory its recent runs reached, none before it has run.
    pub async fn job_cost(&self, repository_id: &RepositoryId, job: &QueuedJob) -> Result<JobCost> {
        let repository = repository_id.to_owned();
        let (workflow, name) = key(job);
        let since = UnixTime::now() - RECENT_FOR;
        let peak: Option<i64> = self
            .store
            .run(move |db| {
                Ok(db.query_row(
                    &format!(
                        "SELECT max(peak_memory) FROM ci_job_usage WHERE {}",
                        recent_runs()
                    ),
                    params![repository, workflow, name, RECENT_RUNS, since],
                    |row| row.get(0),
                )?)
            })
            .await?;
        Ok(JobCost {
            memory: peak.and_then(|peak| u64::try_from(peak).ok()),
        })
    }
}

/// The condition that selects the runs of the job of repository `?1`,
/// workflow `?2` and name `?3`.
const JOB: &str = "repository_id=?1 AND workflow=?2 AND name=?3";

/// The condition that selects the runs of that job its cost reads: its
/// latest `?4` and those that ended at `?5` or later.
fn recent_runs() -> String {
    format!(
        "{JOB} AND (ended_at>=?5 OR rowid IN (SELECT rowid FROM ci_job_usage WHERE {JOB}
         ORDER BY ended_at DESC, rowid DESC LIMIT ?4))"
    )
}

/// The workflow and name that tell runs of one job apart from others.
fn key(job: &QueuedJob) -> (String, String) {
    (
        job.run.workflow.clone().unwrap_or_default(),
        job.job.name.clone(),
    )
}

#[cfg(test)]
mod tests;
