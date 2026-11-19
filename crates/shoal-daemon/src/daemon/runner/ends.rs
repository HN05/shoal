//! The end of a claimed job: once the execution running the forge's runner
//! ends, however it ended, the runner registered for the job alone is
//! deleted and the job's workspace is removed whatever it holds,
//! with its permit, and the runner's directory of a job no longer claimed,
//! however its workspace went, is deleted. A job ends only on the forge's
//! word: stopping it, removing its workspace while it runs, or a signal
//! ending its wrapper's session cancels its run there, and the forge's
//! runner then stops it, or Shoal or the wrapper does once the runner
//! outlives the run or the signal. Shoal's log names what stopped each
//! runner.
use crate::forge::items::reference::ItemRef;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use std::collections::HashSet;

use anyhow::{Context as _, Result};
use rusqlite::params;

use super::JobStop;
use crate::{
    daemon::{capacity::Activity, manager::Manager, notifications::AlertKind, scope::Caller},
    forge::ci::CiAction,
    model::QueuedJob,
    process::{execution::Processes, identity},
    protocol::SessionSignal,
    state::{ExecutionState, QueueKind, QueueState},
};
use shoal_forge::ci::run_action::RunActionRequests;
use shoal_protocol::space_names::SpaceSelector;
use shoal_runner::jobs::ClaimedJob;

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
    /// Remove the workspace of each claimed job whose execution ended, or
    /// that never started one and whose claim no longer runs, as after a
    /// daemon restart. A removal that fails is tried again on the next pass.
    pub(crate) async fn finish_ended_jobs(&self) -> Result<()> {
        let jobs = self.runner.claimed_jobs().await?;
        self.runner
            .jobs()
            .keep_runs(&jobs.iter().map(|job| job.id.clone()).collect());
        for job in jobs {
            let running = match &job.execution_id {
                Some(execution) => {
                    self.job_execution_runs(&job.workspace_id, execution)
                        .await?
                }
                None if self.runner.jobs().claiming(&job.id) => true,
                None => self.adopt_job_execution(&job).await?,
            };
            if running && !self.ended_overrun_job(&job).await {
                continue;
            }
            self.runner.record_job_usage(&job).await?;
            if let Err(error) = self.delete_job_runner(&job).await {
                crate::daemon::log!("runner: {error:#}");
            }
            if let Err(error) = self.remove_job_workspace(&job.workspace_id).await {
                self.notify(
                    None,
                    AlertKind::RunnerFailed,
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

    /// Have the runner end the job the ended execution `id` ran now, instead
    /// of on its next poll, so its workspace goes as soon as it can.
    pub(crate) async fn wake_runner_for_ended_job(&self, id: &ExecutionId) {
        let id = id.to_owned();
        let ran_job = self
            .store
            .run(move |db| {
                crate::daemon::store::exists(
                    db,
                    "SELECT 1 FROM ci_jobs WHERE execution_id=?1",
                    [id],
                )
            })
            .await;
        // A failed lookup costs only a pass that finds nothing to do.
        if ran_job.unwrap_or(true) {
            self.runner.jobs().wake();
        }
    }

    /// Whether Shoal ended `job`, whose runner outlived its run; a job it
    /// could not end, or whose run could not be read, is tried again on a
    /// later pass.
    async fn ended_overrun_job(&self, job: &ClaimedJob) -> bool {
        self.end_overrun_job(job).await.unwrap_or_else(|error| {
            crate::daemon::log!("runner: {}: {error:#}", job.job.job.name);
            false
        })
    }

    /// Record the execution running in the workspace of `job`, whose claim
    /// a restart cut short after its command started but before its
    /// execution was recorded, returning whether there is one.
    async fn adopt_job_execution(&self, job: &ClaimedJob) -> Result<bool> {
        let executions = self.workspace_executions(&job.workspace_id).await?;
        let Some(execution) = executions.into_iter().next() else {
            return Ok(false);
        };
        self.record_job_execution(job.id.as_str(), execution.id)
            .await?;
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
            let id = QueueEntryId::from(entry.file_name().to_string_lossy().into_owned());
            // Checked for each directory, under the lock a claim writes its
            // files with, so a claim recorded or begun meanwhile keeps them.
            let _files = self.runner.jobs().job_files().await;
            if self.runner.jobs().claiming(&id) || self.job_claimed(&id).await? {
                continue;
            }
            if let Err(error) = std::fs::remove_dir_all(entry.path()) {
                crate::daemon::log!("runner: {}: {error:#}", entry.path().display());
            }
        }
        Ok(())
    }

    /// Whether the runner records a claim of the job `id`.
    async fn job_claimed(&self, id: &QueueEntryId) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                crate::daemon::store::exists(db, "SELECT 1 FROM ci_jobs WHERE id=?1", [id])
            })
            .await
    }

    /// Cancel on the forge the run of the job running in the workspace
    /// `selector` names, as `cause` stops it, returning the run's URL, or
    /// `None` when the workspace runs no job.
    pub(crate) async fn cancel_job_in(
        &self,
        selector: &SpaceSelector,
        cause: JobStop,
    ) -> Result<Option<String>> {
        let space = self.space(selector).await?;
        let Some(job) = self.workspace_claim(&space.id).await? else {
            return Ok(None);
        };
        Ok(Some(self.cancel_for_stop(&job, cause).await?.to_string()))
    }

    /// Cancel on the forge the run of a job still running in the workspace
    /// `workspace_id` before `cause` has the daemon stop its runner,
    /// returning whether the workspace holds a job; a forge that refuses is
    /// told to the user, and the run times out there.
    pub(crate) async fn cancel_job_before_stopping(
        &self,
        workspace_id: &SpaceId,
        cause: JobStop,
    ) -> bool {
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
        if let Err(error) = self.cancel_for_stop(&job, cause).await {
            self.notify_cancel_failed(&job, &error).await;
        }
        true
    }

    /// Cancel on the forge the run of the job the execution of `caller`
    /// runs, if it runs one, before `signal`, which ends its session, has
    /// its wrapper stop the runner. Another execution in the job's
    /// workspace, such as a step's command, cancels nothing.
    pub(crate) async fn cancel_job_for_signal(
        &self,
        caller: Option<&Caller>,
        signal: SessionSignal,
    ) -> Result<()> {
        let caller = caller
            .filter(|caller| caller.execution_id().is_some())
            .context("only an execution's wrapper reports the signal that ends its session")?;
        let Some(job) = self.workspace_claim(&caller.space_id).await? else {
            return Ok(());
        };
        // The runner may run before its claim records its execution.
        let runner = (job.execution_id.as_ref()).is_none_or(|id| caller.execution_id() == Some(id));
        if !runner {
            return Ok(());
        }
        if let Err(error) = self.cancel_for_stop(&job, JobStop::Signal(signal)).await {
            self.notify_cancel_failed(&job, &error).await;
            return Err(error);
        }
        Ok(())
    }

    /// Whether the space `space_id` runs a claimed job, whose runner's
    /// wrapper reports the signal that ends its session.
    pub(crate) async fn runs_job(&self, space_id: &SpaceId) -> bool {
        self.workspace_claim(space_id)
            .await
            .is_ok_and(|job| job.is_some())
    }

    /// Tell the user that stopping `job` could not cancel its run.
    async fn notify_cancel_failed(&self, job: &ClaimedJob, error: &anyhow::Error) {
        self.notify(
            None,
            AlertKind::RunnerFailed,
            format!(
                "Stopping CI job {} could not cancel {} on the forge, where it times out: {error:#}",
                job.job.job.name, job.job.run.url
            ),
        )
        .await;
    }

    /// The workspaces of the claimed jobs, none when they cannot be read.
    pub(crate) async fn job_workspaces(&self) -> HashSet<SpaceId> {
        match self.runner.claimed_jobs().await {
            Ok(jobs) => jobs.into_iter().map(|job| job.workspace_id).collect(),
            Err(error) => {
                crate::daemon::log!("runner: claimed jobs unavailable: {error:#}");
                HashSet::new()
            }
        }
    }

    /// The job claimed in the workspace `workspace_id`, if any.
    async fn workspace_claim(&self, workspace_id: &SpaceId) -> Result<Option<ClaimedJob>> {
        Ok(self
            .runner
            .claimed_jobs()
            .await?
            .into_iter()
            .find(|job| job.workspace_id == *workspace_id))
    }

    /// Record in Shoal's log that `cause` stopped the runner of `job`, with
    /// the `outcome` for its run.
    pub(super) fn log_job_stop(&self, job: &ClaimedJob, cause: JobStop, outcome: &str) {
        crate::daemon::log!(
            "runner: {} of run {} stopped by {cause}; {outcome}",
            job.job.job.name,
            job.job.run.url
        );
        #[cfg(test)]
        self.runner
            .jobs()
            .stops
            .lock()
            .unwrap()
            .push((job.workspace_id.clone(), cause));
    }

    /// Cancel `job`'s run as `cause` stops its runner, recording in Shoal's
    /// log what stopped it and whether the run was cancelled, returning the
    /// cancelled run's URL.
    async fn cancel_for_stop(&self, job: &ClaimedJob, cause: JobStop) -> Result<ItemRef> {
        let cancelled = self.cancel_job_run(job).await;
        let outcome = match &cancelled {
            Ok(_) => "cancelled its run".to_owned(),
            Err(error) => format!("could not cancel its run: {error:#}"),
        };
        self.log_job_stop(job, cause, &outcome);
        cancelled
    }

    /// Cancel `job`'s run on the forge its URL names, or the run its
    /// runner reported taking a job of, also once its runner is gone,
    /// returning the cancelled run's URL.
    pub(super) async fn cancel_job_run(&self, job: &ClaimedJob) -> Result<ItemRef> {
        let forge = self.item_forge(&job.job.run.url).await?;
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
    async fn job_execution_runs(&self, workspace_id: &SpaceId, id: &ExecutionId) -> Result<bool> {
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

    /// Whether the daemon still records the execution `id`, running or
    /// awaiting reattachment.
    async fn execution_recorded(&self, id: &ExecutionId) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                crate::daemon::store::exists(db, "SELECT 1 FROM executions WHERE id=?1", [id])
            })
            .await
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
