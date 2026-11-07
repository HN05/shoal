//! The end of a claimed job: once the execution running the forge's runner
//! ends, however it ended, the runner registered for the job alone is
//! deleted and the job's workspace is removed whatever it holds,
//! with its permit, and the runner's directory of a job no longer claimed,
//! however its workspace went, is deleted. A job ends only on the forge's
//! word: stopping it, or removing its workspace while it runs, cancels its
//! run there, and the forge's runner then stops it.
use std::{collections::HashSet, time::Duration};

use anyhow::Result;
use rusqlite::params;

use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        notifications::NotificationKind,
        workspace::{Manager, capacity::Activity},
    },
    forge::{ForgeRepo, ci::CiAction},
    model::QueuedJob,
    process::{execution::Processes, identity},
    state::ExecutionState,
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
    /// When that execution started.
    pub launched_at: Option<i64>,
    pub job: QueuedJob,
    /// The runner the forge registered for this job alone, if it did.
    pub runner_id: Option<u64>,
}

impl Manager {
    /// The jobs the runner claimed.
    pub(crate) async fn claimed_jobs(&self) -> Result<Vec<ClaimedJob>> {
        self.store
            .run(|db| {
                Ok(db
                    .prepare(
                        "SELECT id,repository_id,workspace_id,execution_id,job,launched_at,runner_id
                         FROM ci_jobs ORDER BY started_at",
                    )?
                    .query_map([], |row| {
                        Ok(ClaimedJob {
                            id: row.get(0)?,
                            repository_id: row.get(1)?,
                            workspace_id: row.get(2)?,
                            execution_id: row.get(3)?,
                            launched_at: row.get(5)?,
                            runner_id: row.get::<_, Option<i64>>(6)?.map(|id| id as u64),
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
                Some(execution) => {
                    self.job_execution_runs(&job.workspace_id, execution)
                        .await?
                }
                None if self.runners.claiming(&job.id) => true,
                None => self.adopt_job_execution(&job).await?,
            };
            if running {
                continue;
            }
            self.record_job_usage(&job).await?;
            if let Err(error) = self.delete_job_runner(&job).await {
                crate::daemon::log!("runner: {error:#}");
            }
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
        for entry in entries {
            let entry = entry?;
            let id = entry.file_name().to_string_lossy().into_owned();
            // Checked for each directory, under the lock a claim writes its
            // files with, so a claim recorded or begun meanwhile keeps them.
            let _files = self.runners.job_files().await;
            if self.runners.claiming(&id) || self.job_claimed(&id).await? {
                continue;
            }
            if let Err(error) = std::fs::remove_dir_all(entry.path()) {
                crate::daemon::log!("runner: {}: {error:#}", entry.path().display());
            }
        }
        Ok(())
    }

    /// Whether the runner records a claim of the job `id`.
    async fn job_claimed(&self, id: &str) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                crate::daemon::store::exists(db, "SELECT 1 FROM ci_jobs WHERE id=?1", [id])
            })
            .await
    }

    /// Cancel on the forge the run of the job running in the workspace
    /// `selector` names, returning the run's URL, or `None` when the
    /// workspace runs no job.
    pub(crate) async fn cancel_job_in(&self, selector: &SpaceSelector) -> Result<Option<String>> {
        let workspace = self.workspace(selector).await?;
        let Some(job) = self.workspace_claim(&workspace.id).await? else {
            return Ok(None);
        };
        Ok(Some(self.cancel_job_run(&job).await?))
    }

    /// Cancel on the forge the run of a job still running in the workspace
    /// `workspace_id` before the daemon stops its runner, returning whether
    /// the workspace holds a job; a forge that refuses is told to the user,
    /// and the run times out there.
    pub(crate) async fn cancel_job_before_stopping(&self, workspace_id: &str) -> bool {
        let Ok(Some(job)) = self.workspace_claim(workspace_id).await else {
            return false;
        };
        // The runner may run before its claim records its execution, so
        // any execution of the workspace counts.
        let running = match &job.execution_id {
            Some(execution) => self.execution_recorded(execution).await,
            None => self.workspace_has_executions(workspace_id).await,
        };
        if !running.unwrap_or(true) {
            return true;
        }
        if let Err(error) = self.cancel_job_run(&job).await {
            self.notify(
                None,
                NotificationKind::RunnerFailed,
                format!(
                    "Stopping CI job {} could not cancel {} on the forge, where it times out: {error:#}",
                    job.job.job.name, job.job.run.url
                ),
            )
            .await;
        }
        true
    }

    /// The job claimed in the workspace `workspace_id`, if any.
    async fn workspace_claim(&self, workspace_id: &str) -> Result<Option<ClaimedJob>> {
        Ok(self
            .claimed_jobs()
            .await?
            .into_iter()
            .find(|job| job.workspace_id == workspace_id))
    }

    /// Cancel `job`'s run on the forge its URL names, or the run its
    /// runner reported taking a job of, also once its runner is gone,
    /// returning the cancelled run's URL.
    pub(super) async fn cancel_job_run(&self, job: &ClaimedJob) -> Result<String> {
        let forge = ForgeRepo::resolve_run_url(&job.job.run.url, &self.forge_hosts()).await?;
        let account = self.user_account();
        let url = self.reported_run_url(&forge, job);
        let run = forge.find_run(&account, &url, CiAction::Cancel).await?;
        forge
            .act_on_run(&account, &run, CiAction::Cancel, false)
            .await?;
        Ok(url)
    }

    /// Whether the execution `id` of a job in `workspace_id` may still run.
    /// One whose wrapper has not reattached since a daemon restart, which
    /// restart recovery leaves to its owner since a job's runner has no agent
    /// record, counts as ended, and is forgotten, once the shared process
    /// proof finds nothing of it: no wrapper, and no process carrying its
    /// marker or in its process group. Verified survivors are stopped and
    /// checked again on the next pass; a launch whose identity was never
    /// recorded proves nothing and keeps the job.
    async fn job_execution_runs(&self, workspace_id: &str, id: &str) -> Result<bool> {
        let workspace = self.workspace_by_id(workspace_id).await?;
        let execution = self
            .workspace_executions(workspace_id)
            .await?
            .into_iter()
            .find(|execution| execution.id == id);
        let Some(execution) = execution else {
            return Ok(false);
        };
        if execution.state != ExecutionState::Unknown {
            return Ok(true);
        }
        let scan = identity::scan(HashSet::from([id.to_owned()])).await?;
        let processes = Processes::inspect(&execution, &workspace.path, &scan).await?;
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
    /// use, and how long ago its runner started; one whose runner has not
    /// started yet has just started, so it keeps warming and holds back the
    /// next start until it runs.
    pub(crate) async fn job_activities(&self) -> Result<Vec<Activity>> {
        let jobs: Vec<(String, Option<i64>, String)> = self
            .store
            .run(|db| {
                Ok(db
                    .prepare(
                        "SELECT repository_id,NULL,job FROM agent_queue WHERE kind=?1 AND state=?2
                             AND id NOT IN (SELECT id FROM ci_jobs)
                         UNION ALL SELECT repository_id,launched_at,job FROM ci_jobs",
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
