//! The end of a claimed job: once the execution running the forge's runner
//! ends, however it ended, the job's workspace is removed whatever it holds,
//! with its permit, and the runner's directory of a job no longer claimed,
//! however its workspace went, is deleted. A job ends
//! only on the forge's word: stopping it cancels its run there, which the
//! forge's runner then stops.
use std::time::Duration;

use anyhow::Result;
use rusqlite::params;

use crate::{
    daemon::{
        notifications::NotificationKind,
        workspace::{Manager, capacity::Activity},
    },
    forge::{ForgeRepo, ci::CiAction},
    model::QueuedJob,
    state::{QueueKind, QueueState},
};

/// A job the runner claimed.
#[derive(Debug, Clone)]
pub(crate) struct ClaimedJob {
    /// The ID its queue entry had.
    pub id: String,
    pub repository_id: String,
    pub workspace_id: String,
    /// The execution running the forge's runner, once it started.
    pub execution_id: Option<String>,
    pub job: QueuedJob,
}

impl Manager {
    /// The jobs the runner claimed.
    pub(crate) async fn claimed_jobs(&self) -> Result<Vec<ClaimedJob>> {
        self.store
            .run(|db| {
                Ok(db
                    .prepare(
                        "SELECT id,repository_id,workspace_id,execution_id,job FROM ci_jobs
                         ORDER BY started_at",
                    )?
                    .query_map([], |row| {
                        Ok(ClaimedJob {
                            id: row.get(0)?,
                            repository_id: row.get(1)?,
                            workspace_id: row.get(2)?,
                            execution_id: row.get(3)?,
                            job: serde_json::from_str(&row.get::<_, String>(4)?).map_err(
                                |error| {
                                    rusqlite::Error::FromSqlConversionFailure(
                                        4,
                                        rusqlite::types::Type::Text,
                                        Box::new(error),
                                    )
                                },
                            )?,
                        })
                    })?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// Remove the workspace of each claimed job whose execution ended, or
    /// that never started one and whose claim no longer runs, as after a
    /// daemon restart. A removal that fails is tried again on the next pass.
    pub(crate) async fn finish_ended_jobs(&self) -> Result<()> {
        for job in self.claimed_jobs().await? {
            let running = match &job.execution_id {
                Some(execution) => self.execution_recorded(execution).await?,
                None if self.runners.claiming(&job.id) => true,
                None => self.adopt_job_execution(&job).await?,
            };
            if running {
                continue;
            }
            self.record_job_usage(&job).await?;
            if let Err(error) = self.remove_job_workspace(&job.workspace_id).await {
                self.notify(
                    None,
                    NotificationKind::RunnerFailed,
                    format!(
                        "The workspace of the ended CI job {} could not be removed: {error:#}",
                        job.job.job.name
                    ),
                )
                .await;
                continue;
            }
        }
        self.remove_unclaimed_job_directories().await
    }

    /// Record the execution running in the workspace of `job`, whose claim
    /// a restart cut short after its command started but before its
    /// execution was recorded, returning whether there is one.
    async fn adopt_job_execution(&self, job: &ClaimedJob) -> Result<bool> {
        let executions = self.workspace_executions(&job.workspace_id).await?;
        let Some(execution) = executions.into_iter().next() else {
            return Ok(false);
        };
        self.record_job_execution(&job.id, execution.id).await?;
        Ok(true)
    }

    /// Delete the runner's directory of each job no longer claimed, however
    /// its workspace went; one that cannot be deleted is tried again on the
    /// next pass.
    async fn remove_unclaimed_job_directories(&self) -> Result<()> {
        let parent = self.paths.state.join("runner");
        let entries = match std::fs::read_dir(&parent) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let claimed: Vec<String> = self
            .claimed_jobs()
            .await?
            .into_iter()
            .map(|job| job.id)
            .collect();
        for entry in entries {
            let entry = entry?;
            let id = entry.file_name().to_string_lossy().into_owned();
            if claimed.contains(&id) || self.runners.claiming(&id) {
                continue;
            }
            if let Err(error) = std::fs::remove_dir_all(entry.path()) {
                crate::daemon::log!("runner: {}: {error:#}", entry.path().display());
            }
        }
        Ok(())
    }

    /// Cancel on the forge the run of the job running in the workspace
    /// `selector` names, returning whether the workspace runs one.
    pub(crate) async fn cancel_job_in(&self, selector: &str) -> Result<bool> {
        let workspace = self.workspace(selector).await?;
        let Some(job) = self
            .claimed_jobs()
            .await?
            .into_iter()
            .find(|job| job.workspace_id == workspace.id)
        else {
            return Ok(false);
        };
        // The run names its repository, also once the runner is gone.
        let forge = ForgeRepo::resolve_run_url(&job.job.run.url, &self.forge_hosts()).await?;
        let account = self.user_account();
        let run = forge
            .find_run(&account, &job.job.run.url, CiAction::Cancel)
            .await?;
        forge
            .act_on_run(&account, &run, CiAction::Cancel, false)
            .await?;
        Ok(true)
    }

    /// Whether the daemon still records the execution `id`, running or
    /// awaiting reattachment.
    async fn execution_recorded(&self, id: &str) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                crate::daemon::store::exists(db, "SELECT 1 FROM executions WHERE id=?1", [id])
            })
            .await
    }

    /// The load of each admitted or claimed job: what it is expected to
    /// use, and how long ago it was admitted.
    pub(crate) async fn job_activities(&self) -> Result<Vec<Activity>> {
        let jobs: Vec<(String, Option<i64>, String)> = self
            .store
            .run(|db| {
                Ok(db
                    .prepare(
                        "SELECT repository_id,seen_at,job FROM agent_queue WHERE kind=?1 AND state=?2
                             AND id NOT IN (SELECT id FROM ci_jobs)
                         UNION ALL SELECT repository_id,started_at,job FROM ci_jobs",
                    )?
                    .query_map(params![QueueKind::Job, QueueState::Starting], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await?;
        let now = crate::time::unix_seconds() as i64;
        let mut activities = Vec::new();
        for (repository, since, job) in jobs {
            let job: QueuedJob = serde_json::from_str(&job)?;
            activities.push(Activity::Job {
                age: Duration::from_secs(now.saturating_sub(since.unwrap_or(now)).max(0) as u64),
                cost: self.job_cost(&repository, &job).await?,
            });
        }
        Ok(activities)
    }
}
