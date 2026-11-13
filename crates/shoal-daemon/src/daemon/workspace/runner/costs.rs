//! What each repository's CI jobs use: the peak resident memory and the
//! duration of every run, read from its workspace's usage samples and kept
//! per repository, workflow and job name, the cost a waiting or starting
//! run of a job counts at in admission: the peak memory of its recent runs,
//! none before it has run, and how long the jobs a space waits for have
//! left.
use crate::client::{repository_ids::RepositoryId, space_ids::SpaceId};
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use std::time::Duration;

use super::ends::ClaimedJob;
use crate::{
    daemon::workspace::{Manager, capacity::JobCost},
    model::QueuedJob,
};

/// How many of a job's latest runs its cost reads.
const RECENT_RUNS: i64 = 5;

impl Manager {
    /// Raise each claimed job's peak memory to what its workspace's latest
    /// usage sample shows.
    pub(crate) async fn sample_job_usage(&self) -> Result<()> {
        let now = tokio::time::Instant::now();
        for job in self.claimed_jobs().await? {
            let Some(usage) = self
                .usage
                .with_workspaces(|usages| usages.get(&job.workspace_id, now))
            else {
                continue;
            };
            let memory = i64::try_from(usage.memory_bytes).unwrap_or(i64::MAX);
            self.store
                .run(move |db| {
                    db.execute(
                        "UPDATE ci_jobs SET peak_memory=max(coalesce(peak_memory,0),?2)
                         WHERE id=?1",
                        params![job.id, memory],
                    )?;
                    Ok(())
                })
                .await?;
        }
        Ok(())
    }

    /// Keep the peak memory of the ended `job` as its latest run, with how
    /// long it ran when its start is known, unless no sample saw it run or
    /// it is kept already, as before a removal that is tried again,
    /// forgetting runs older than [`RECENT_RUNS`].
    pub(super) async fn record_job_usage(&self, job: &ClaimedJob) -> Result<()> {
        let (id, repository) = (job.id.clone(), job.repository_id.clone());
        let (workflow, name) = key(&job.job);
        let now = crate::time::UnixTime::now();
        let seconds = (job.launched_at).map(|launched| {
            i64::try_from(now.duration_since(launched).as_secs()).unwrap_or(i64::MAX)
        });
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
                    "INSERT INTO ci_job_usage(repository_id,workflow,name,ended_at,peak_memory,seconds)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                    params![repository, workflow, name, now, memory, seconds],
                )?;
                tx.execute(
                    "DELETE FROM ci_job_usage WHERE repository_id=?1 AND workflow=?2 AND name=?3
                     AND rowid NOT IN (SELECT rowid FROM ci_job_usage
                         WHERE repository_id=?1 AND workflow=?2 AND name=?3
                         ORDER BY ended_at DESC, rowid DESC LIMIT ?4)",
                    params![repository, workflow, name, RECENT_RUNS],
                )?;
                Ok(())
            })
            .await
    }

    /// What a run of `job` in `repository_id` is expected to use: the most
    /// memory its recent runs reached, none before it has run.
    pub(crate) async fn job_cost(
        &self,
        repository_id: &RepositoryId,
        job: &QueuedJob,
    ) -> Result<JobCost> {
        let repository = repository_id.to_owned();
        let (workflow, name) = key(job);
        let peak: Option<i64> = self
            .store
            .run(move |db| {
                Ok(db.query_row(
                    "SELECT max(peak_memory) FROM (SELECT peak_memory
                     FROM ci_job_usage WHERE repository_id=?1 AND workflow=?2 AND name=?3
                     ORDER BY ended_at DESC, rowid DESC LIMIT ?4)",
                    params![repository, workflow, name, RECENT_RUNS],
                    |row| row.get(0),
                )?)
            })
            .await?;
        Ok(JobCost {
            memory: peak.and_then(|peak| u64::try_from(peak).ok()),
        })
    }

    /// How long a run of `job` in `repository_id` is expected to take: the
    /// longest of its recent runs whose duration is known, none before one.
    async fn job_duration(
        &self,
        repository_id: &RepositoryId,
        job: &QueuedJob,
    ) -> Result<Option<Duration>> {
        let repository = repository_id.to_owned();
        let (workflow, name) = key(job);
        let seconds: Option<i64> = self
            .store
            .run(move |db| {
                Ok(db.query_row(
                    "SELECT max(seconds) FROM (SELECT seconds
                     FROM ci_job_usage WHERE repository_id=?1 AND workflow=?2 AND name=?3
                        AND seconds IS NOT NULL
                     ORDER BY ended_at DESC, rowid DESC LIMIT ?4)",
                    params![repository, workflow, name, RECENT_RUNS],
                    |row| row.get(0),
                )?)
            })
            .await?;
        Ok(seconds.and_then(|seconds| Some(Duration::from_secs(u64::try_from(seconds).ok()?))))
    }

    /// How long the CI jobs this machine's runner runs or has queued for
    /// the space `id` are expected to go on at `now`, from the recent runs
    /// of each: the longest a running job has left, or a waiting job takes.
    /// None when no such job serves the space, or one has no known
    /// duration, since then Shoal cannot tell.
    pub(crate) async fn ci_remaining(
        &self,
        id: &SpaceId,
        now: crate::time::UnixTime,
    ) -> Result<Option<Duration>> {
        let claimed = (self.claimed_jobs().await?.into_iter())
            .map(|job| (job.repository_id, job.job, job.launched_at));
        let waiting = (self
            .store
            .read(|tx| super::linked::waiting_jobs(tx))
            .await?)
            .into_iter()
            .map(|(_, repository, job)| (repository, job, None));
        let mut remaining = None;
        for (repository, job, launched) in claimed.chain(waiting) {
            if !job.space_ids.contains(id) {
                continue;
            }
            let Some(duration) = self.job_duration(&repository, &job).await? else {
                return Ok(None);
            };
            let ran = launched.map_or(Duration::ZERO, |launched| now.duration_since(launched));
            let left = duration.saturating_sub(ran);
            remaining = Some(remaining.map_or(left, |longest: Duration| longest.max(left)));
        }
        Ok(remaining)
    }
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
