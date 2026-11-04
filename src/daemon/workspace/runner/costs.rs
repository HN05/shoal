//! What each repository's CI jobs use: the peak CPU and resident memory of
//! every run, read from its workspace's usage samples and kept per
//! repository, workflow and job name, and the cost a waiting or starting
//! run of a job counts at in admission: the peaks of its recent runs, or
//! without any the load of a job without history.
use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use super::ends::ClaimedJob;
use crate::{
    daemon::workspace::{Manager, capacity::JobCost},
    model::QueuedJob,
};

/// How many of a job's latest runs its cost reads.
const RECENT_RUNS: i64 = 5;

impl Manager {
    /// Raise each claimed job's peaks to what its workspace's latest usage
    /// sample shows.
    pub(crate) async fn sample_job_usage(&self) -> Result<()> {
        let now = tokio::time::Instant::now();
        for job in self.claimed_jobs().await? {
            let Some(usage) = self
                .usage
                .with_workspaces(|usages| usages.get(&job.workspace_id, now))
            else {
                continue;
            };
            let cpu = usage.cpu_percent.unwrap_or_default();
            let memory = i64::try_from(usage.memory_bytes).unwrap_or(i64::MAX);
            self.store
                .run(move |db| {
                    db.execute(
                        "UPDATE ci_jobs SET peak_cpu=max(coalesce(peak_cpu,0),?2),
                            peak_memory=max(coalesce(peak_memory,0),?3) WHERE id=?1",
                        params![job.id, cpu, memory],
                    )?;
                    Ok(())
                })
                .await?;
        }
        Ok(())
    }

    /// Keep the peaks of the ended `job` as its latest run, unless no
    /// sample saw it run or they are kept already, as before a removal that
    /// is tried again, forgetting runs older than [`RECENT_RUNS`].
    pub(super) async fn record_job_usage(&self, job: &ClaimedJob) -> Result<()> {
        let (id, repository) = (job.id.clone(), job.repository_id.clone());
        let (workflow, name) = key(&job.job);
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                let peaks: Option<(Option<f64>, Option<i64>)> = tx
                    .query_row(
                        "SELECT peak_cpu,peak_memory FROM ci_jobs WHERE id=?1 AND usage_recorded=0",
                        [&id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                let Some((Some(cpu), Some(memory))) = peaks else {
                    return Ok(());
                };
                tx.execute("UPDATE ci_jobs SET usage_recorded=1 WHERE id=?1", [&id])?;
                tx.execute(
                    "INSERT INTO ci_job_usage(repository_id,workflow,name,ended_at,peak_cpu,peak_memory)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        repository,
                        workflow,
                        name,
                        crate::time::unix_seconds() as i64,
                        cpu,
                        memory
                    ],
                )?;
                tx.execute(
                    "DELETE FROM ci_job_usage WHERE repository_id=?1 AND workflow=?2 AND name=?3
                     AND rowid NOT IN (SELECT rowid FROM ci_job_usage
                         WHERE repository_id=?1 AND workflow=?2 AND name=?3
                         ORDER BY ended_at DESC, rowid DESC LIMIT ?4)",
                    params![repository, workflow, name, RECENT_RUNS],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// What a run of `job` in `repository_id` is expected to use: the most
    /// CPU and memory its recent runs reached, or without any runs the
    /// cost of a job without history.
    pub(crate) async fn job_cost(&self, repository_id: &str, job: &QueuedJob) -> Result<JobCost> {
        let repository = repository_id.to_owned();
        let (workflow, name) = key(job);
        let peaks: (Option<f64>, Option<i64>) = self
            .store
            .run(move |db| {
                Ok(db.query_row(
                    "SELECT max(peak_cpu),max(peak_memory) FROM (SELECT peak_cpu,peak_memory
                     FROM ci_job_usage WHERE repository_id=?1 AND workflow=?2 AND name=?3
                     ORDER BY ended_at DESC, rowid DESC LIMIT ?4)",
                    params![repository, workflow, name, RECENT_RUNS],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?)
            })
            .await?;
        Ok(match peaks {
            (Some(cpu), Some(memory)) => JobCost {
                cores: cpu / 100.0,
                memory: u64::try_from(memory).ok(),
            },
            _ => JobCost::UNKNOWN,
        })
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
