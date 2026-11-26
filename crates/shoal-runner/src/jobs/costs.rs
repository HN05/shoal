//! What each repository's CI jobs use, and what admission expects a run of a
//! job to use.
use anyhow::Result;
use rusqlite::{OptionalExtension, Transaction, params};
use shoal_daemon_base::{
    jobs::JobCost,
    run_usage::{RunKey, RunUsage, expected_memory, record_run},
};
use shoal_ids::{queue_entry_ids::QueueEntryId, repository_ids::RepositoryId};
use shoal_protocol::model::QueuedJob;
use shoal_store::exists;

use super::ClaimedJob;
use crate::{Runner, daemon::Daemon, time::UnixTime};

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

    /// Record that something cut the run of the claimed job `id` short, so
    /// its estimate leaves it out.
    pub(super) async fn mark_job_cut_short(&self, id: &QueueEntryId) -> Result<()> {
        let id = id.clone();
        self.store
            .run(move |db| {
                db.execute("UPDATE ci_jobs SET cut_short=1 WHERE id=?1", [id])?;
                Ok(())
            })
            .await
    }

    /// Record that the kernel's OOM killer killed a process of the claimed
    /// job `id`, so its estimate counts it above its peak.
    pub(super) async fn mark_job_oom_killed(&self, id: &QueueEntryId) -> Result<()> {
        let id = id.clone();
        self.store
            .run(move |db| {
                db.execute("UPDATE ci_jobs SET oom_killed=1 WHERE id=?1", [id])?;
                Ok(())
            })
            .await
    }

    /// Keep what the ended `job` used as the latest run of its job. Nothing is
    /// kept when no sample saw it run, or when a removal tried again kept it
    /// already.
    pub async fn record_job_usage(&self, job: &ClaimedJob) -> Result<()> {
        let (id, repository, key) = (job.id.clone(), job.repository_id.clone(), key(&job.job));
        let now = UnixTime::now();
        self.store
            .write(move |tx| {
                let Some(run) = ended_run(tx, &id, now)? else {
                    return Ok(());
                };
                tx.execute("UPDATE ci_jobs SET usage_recorded=1 WHERE id=?1", [&id])?;
                record_run(tx, &repository, &key, &run)
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

    /// What a run of `job` in `repository_id` is expected to use, none
    /// before it has run.
    pub async fn job_cost(&self, repository_id: &RepositoryId, job: &QueuedJob) -> Result<JobCost> {
        let (repository, key) = (repository_id.to_owned(), key(job));
        let memory = (self.store)
            .read(move |tx| expected_memory(tx, &repository, &key))
            .await?;
        Ok(JobCost { memory })
    }
}

/// What the claimed job `id`, which ended at `now`, used, unless no sample
/// saw it run or it is kept already.
fn ended_run(tx: &Transaction<'_>, id: &QueueEntryId, now: UnixTime) -> Result<Option<RunUsage>> {
    let run = tx
        .query_row(
            "SELECT peak_memory, coalesce(launched_at,started_at), cut_short, oom_killed
             FROM ci_jobs WHERE id=?1 AND usage_recorded=0 AND peak_memory IS NOT NULL",
            [id],
            |row| {
                Ok(RunUsage {
                    ended_at: now,
                    peak_memory: u64::try_from(row.get::<_, i64>(0)?).unwrap_or_default(),
                    duration: now.duration_since(row.get(1)?),
                    whole: !row.get::<_, bool>(2)?,
                    oom_killed: row.get(3)?,
                })
            },
        )
        .optional()?;
    Ok(run)
}

/// The key that tells runs of `job` apart from other jobs' runs: its
/// workflow and name.
fn key(job: &QueuedJob) -> RunKey {
    RunKey::CiJob {
        workflow: job.run.workflow.clone().unwrap_or_default(),
        name: job.job.name.clone(),
    }
}

#[cfg(test)]
mod tests;
