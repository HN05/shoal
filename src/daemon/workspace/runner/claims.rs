//! Claiming the jobs admission admitted: each runs the forge's runner,
//! which claims the job by its handle, as a tracked execution in a workspace
//! at the job's commit that starts from the seed and holds a permit from the
//! repository's `runner.permit` pool when it names one. A PR's job whose PR
//! moved to a newer head is passed over instead, as is one that cannot
//! start, and either stays with the forge for other runners, as do the jobs
//! of a repository whose runner was turned off. A job admitted as the system
//! shuts down waits in the queue again.
use std::{io::Write as _, os::unix::fs::OpenOptionsExt as _, path::PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::params;

use super::{Registration, runner_labels};
use crate::{
    client::resources::ResourceRequest,
    config::Runner,
    daemon::{allocation::Allocation, notifications::NotificationKind, workspace::Manager},
    forge::{
        ForgeRepo,
        ci::runner::{JobLaunch, Trigger},
    },
    model::{QueueEntry, QueuedJob, Workspace},
    removal::InspectionPolicy,
    state::{QueueKind, QueueState},
};

/// The name of the lease a job holds from the repository's permit pool.
const PERMIT: &str = "ci-job";

/// How a claim ended.
enum Claim {
    Started,
    /// The job's PR moved on, so its run is superseded.
    Superseded,
    /// The permit pool is full, so the job waits again.
    PermitsBusy,
    /// The repository's runner was turned off, so the job leaves the queue.
    Withdrawn,
    /// The system shuts down, so the job waits for the queue to admit it again.
    Suspended,
}

impl Manager {
    /// The job entries admission admitted, which the runner claims.
    pub(crate) async fn admitted_jobs(&self) -> Result<Vec<QueueEntry>> {
        Ok(self
            .queue_entries()
            .await?
            .into_iter()
            .filter(|entry| entry.kind == QueueKind::Job && entry.state == QueueState::Starting)
            .collect())
    }

    /// Claim the admitted job `entry`. One that cannot start is passed over,
    /// which the user hears about.
    pub(crate) async fn claim_job(&self, entry: &QueueEntry) {
        let result = self.try_claim_job(entry).await;
        let error = match result {
            Ok(Claim::Started) => return,
            Ok(Claim::Superseded) => None,
            Ok(Claim::Withdrawn) => {
                if let Err(error) = self.forget_job_entries(&entry.repository_id).await {
                    crate::daemon::log!("runner: {error:#}");
                }
                return;
            }
            Ok(Claim::PermitsBusy) => {
                self.runners.found_permits_busy(&entry.repository_id);
                if let Err(error) = self.return_job(&entry.id).await {
                    crate::daemon::log!("runner: {error:#}");
                }
                return;
            }
            Ok(Claim::Suspended) => {
                if let Err(error) = self.return_job(&entry.id).await {
                    crate::daemon::log!("runner: {error:#}");
                }
                return;
            }
            Err(error) => Some(error),
        };
        if let Err(error) = self.pass_over_claimed(entry).await {
            crate::daemon::log!("runner: {error:#}");
        }
        if let Some(error) = error {
            self.notify(
                Some(&entry.name),
                NotificationKind::RunnerFailed,
                format!(
                    "CI job {} did not start; the forge keeps it for other runners: {error:#}",
                    entry.name
                ),
            )
            .await;
        }
    }

    async fn try_claim_job(&self, entry: &QueueEntry) -> Result<Claim> {
        let job = entry.job.as_deref().context("the entry names no job")?;
        let settings = self.repository_settings(&entry.repository_id).await?;
        if !settings.runner.enabled {
            return Ok(Claim::Withdrawn);
        }
        if self.agent_queue.suspended() {
            return Ok(Claim::Suspended);
        }
        let registration = self
            .runner_registration(&entry.repository_id)
            .await?
            .context("the repository's runner is no longer registered")?;
        let forge = ForgeRepo::resolve(&registration.forge_repository, &self.forge_hosts()).await?;
        if self.superseded(&forge, job).await? {
            return Ok(Claim::Superseded);
        }
        let workspace = self
            .create_job_workspace(
                &entry.repository_id,
                &entry.name,
                &job.run.commit,
                job.run.reference.as_deref(),
            )
            .await?;
        // Recorded before the workspace takes a permit or runs anything, so
        // after a restart the claim's end removes it with what it holds.
        let recorded = self.record_claim(entry, &workspace.id, &forge, job).await;
        if let Err(error) = recorded {
            if let Err(removal) = self.remove_job_workspace(&workspace.id).await {
                crate::daemon::log!("runner: {removal:#}");
            }
            return Err(error);
        }
        let claim = Claimed {
            entry,
            workspace: &workspace,
            forge: &forge,
            registration: &registration,
            runner: &settings.runner,
        };
        let started = self.start_claimed(claim).await;
        if !matches!(started, Ok(Claim::Started))
            && let Err(error) = self.remove_job_workspace(&workspace.id).await
        {
            crate::daemon::log!("runner: {error:#}");
        }
        started
    }

    /// Whether `job`'s run is for a PR whose head moved past its commit.
    async fn superseded(&self, forge: &ForgeRepo, job: &QueuedJob) -> Result<bool> {
        let Trigger::Pull { number } = job.run.trigger else {
            return Ok(false);
        };
        let (_, url) = forge.pull(&number.to_string())?;
        Ok(forge.pull_head(&self.user_account(), &url).await? != job.run.commit)
    }

    /// Start `claim`'s job in its workspace once it holds its permit.
    async fn start_claimed(&self, claim: Claimed<'_>) -> Result<Claim> {
        if let Some(pool) = &claim.runner.permit
            && !self.hold_job_permit(claim.workspace, pool).await?
        {
            return Ok(Claim::PermitsBusy);
        }
        let job = claim
            .entry
            .job
            .as_deref()
            .context("the entry names no job")?;
        // Held until the launch is recorded, so a system shutdown, which
        // suspends the queue under the same gate, either stops the job with
        // the rest or finds it not started.
        let _admission = self.agent_queue.lock_admission().await;
        if self.agent_queue.suspended() {
            return Ok(Claim::Suspended);
        }
        self.take_over_entry(&claim.entry.id).await?;
        let argv = {
            // The sweep of unclaimed directories waits while one is written.
            let _files = self.runners.job_files().await;
            self.write_job_files(&claim, job)?
        };
        let log = self.job_directory(&claim.entry.id).join("job.log");
        let execution = self.launch_job_command(claim.workspace, log, argv).await?;
        self.record_job_execution(&claim.entry.id, execution)
            .await?;
        Ok(Claim::Started)
    }

    /// Take one permit from `pool` for the job in `workspace`, returning
    /// whether the pool had one.
    async fn hold_job_permit(&self, workspace: &Workspace, pool: &str) -> Result<bool> {
        let request = ResourceRequest {
            mode: None,
            kind: None,
            pool: pool.to_owned(),
            name: PERMIT.into(),
            resource: None,
            reason: None,
            queue: false,
        };
        match self.acquire_resource(&workspace.id, request, None).await? {
            Allocation::Granted(_) => Ok(true),
            Allocation::Busy(_) => Ok(false),
            Allocation::Approval(_) => {
                bail!("the {pool} pool needs approval, which a CI job cannot ask for")
            }
        }
    }

    /// Record the job of `entry` as claimed in `workspace`, with the
    /// workspace its run links now. Its entry keeps the job's slot until
    /// [`Self::take_over_entry`], so a claim that cannot go on puts the job
    /// back.
    pub(super) async fn record_claim(
        &self,
        entry: &QueueEntry,
        workspace: &str,
        forge: &ForgeRepo,
        job: &QueuedJob,
    ) -> Result<()> {
        let linked = self
            .run_workspace(forge, &entry.repository_id, &job.run)
            .await?;
        let job = serde_json::to_string(&QueuedJob {
            workspace_id: linked,
            ..job.clone()
        })?;
        let (id, repository, workspace) = (
            entry.id.clone(),
            entry.repository_id.clone(),
            workspace.to_owned(),
        );
        self.store
            .run(move |db| {
                db.execute(
                    "INSERT INTO ci_jobs(id,repository_id,workspace_id,job,started_at)
                     VALUES (?1,?2,?3,?4,?5)",
                    params![
                        id,
                        repository,
                        workspace,
                        job,
                        crate::time::unix_seconds() as i64
                    ],
                )?;
                Ok(())
            })
            .await
    }

    /// The claimed job of the entry `id` takes over the slot its entry held.
    async fn take_over_entry(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                db.execute("DELETE FROM agent_queue WHERE id=?1", [id])?;
                Ok(())
            })
            .await
    }

    pub(super) async fn record_job_execution(&self, id: &str, execution: String) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE ci_jobs SET execution_id=?2,launched_at=coalesce(launched_at,?3)
                     WHERE id=?1",
                    params![id, execution, crate::time::unix_seconds() as i64],
                )?;
                Ok(())
            })
            .await
    }

    /// Write the runner's token and the configuration of `claim`'s job into
    /// the job's directory, returning the command that runs it.
    fn write_job_files(
        &self,
        claim: &Claimed<'_>,
        job: &QueuedJob,
    ) -> Result<Vec<std::ffi::OsString>> {
        let directory = self.job_directory(&claim.entry.id);
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        let token_file = directory.join("token");
        let config_file = directory.join("config.yaml");
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&token_file)?
            .write_all(claim.registration.runner.token.as_bytes())?;
        let labels = runner_labels(&claim.runner.labels);
        let command = claim.forge.job_command(&JobLaunch {
            runner: &claim.registration.runner,
            token_file: &token_file,
            config_file: &config_file,
            work_dir: &directory,
            labels: &labels,
            handle: &job.job.handle,
            env: &job_env(claim.workspace),
        })?;
        std::fs::write(&config_file, command.config)?;
        Ok(command.argv)
    }

    /// The directory a claimed job's runner keeps its files in.
    pub(super) fn job_directory(&self, id: &str) -> PathBuf {
        self.paths.state.join("runner").join(id)
    }

    /// Run `argv` in `workspace` as a tracked execution that outlives the
    /// daemon, with its output in `log`, returning the execution's ID.
    #[cfg(not(test))]
    async fn launch_job_command(
        &self,
        workspace: &Workspace,
        log: PathBuf,
        argv: Vec<std::ffi::OsString>,
    ) -> Result<String> {
        let launch =
            crate::execution::launch_detached(&self.paths, workspace, log, argv, &[], &[], None)
                .await?;
        Ok(launch.execution_id)
    }

    /// Tests run no command; they record it with an execution they end.
    #[cfg(test)]
    async fn launch_job_command(
        &self,
        workspace: &Workspace,
        _log: PathBuf,
        argv: Vec<std::ffi::OsString>,
    ) -> Result<String> {
        super::tests::launch(self, workspace, argv).await
    }

    /// Take `repository_id`'s jobs out of the queue, as once its runner is
    /// turned off; the forge keeps them for other runners.
    pub(crate) async fn forget_job_entries(&self, repository_id: &str) -> Result<()> {
        let id = repository_id.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "DELETE FROM agent_queue WHERE repository_id=?1 AND kind=?2",
                    params![id, QueueKind::Job],
                )?;
                Ok(())
            })
            .await
    }

    /// Put the admitted job `id` back in the queue, where it waits again.
    async fn return_job(&self, id: &str) -> Result<()> {
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

    /// Leave the job of `entry`, which the runner did not start, to other
    /// runners.
    async fn pass_over_claimed(&self, entry: &QueueEntry) -> Result<()> {
        let job = entry.job.as_deref().context("the entry names no job")?;
        let (id, repository, handle) = (
            entry.id.clone(),
            entry.repository_id.clone(),
            job.job.handle.clone(),
        );
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                tx.execute("DELETE FROM agent_queue WHERE id=?1", [&id])?;
                tx.execute(
                    "INSERT OR IGNORE INTO ci_passed_jobs(repository_id,handle) VALUES (?1,?2)",
                    params![repository, handle],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// Remove a job's workspace whatever it holds; its claim goes with it.
    pub(super) async fn remove_job_workspace(&self, workspace_id: &str) -> Result<()> {
        self.remove_workspace(
            workspace_id,
            crate::removal::UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await?;
        Ok(())
    }
}

/// What the workflow's `env` context and every step of a job get besides
/// the runner's own environment, so a job's steps can find its workspace
/// and tell it runs on Shoal.
fn job_env(workspace: &Workspace) -> Vec<(String, String)> {
    vec![
        (
            crate::env::WORKSPACE_PATH.into(),
            workspace.path.to_string_lossy().into_owned(),
        ),
        (crate::env::WORKSPACE_ID.into(), workspace.id.clone()),
    ]
}

/// What starting a claimed job needs.
struct Claimed<'a> {
    entry: &'a QueueEntry,
    workspace: &'a Workspace,
    forge: &'a ForgeRepo,
    registration: &'a Registration,
    runner: &'a Runner,
}
