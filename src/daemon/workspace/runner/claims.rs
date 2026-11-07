//! Claiming the jobs admission admitted: each runs the forge's runner,
//! which claims the job by its handle, as a tracked execution in a workspace
//! at the job's commit that starts from the seed and holds a permit from the
//! repository's `runner.permit` pool when it names one. A PR's job whose PR
//! moved to a newer head is passed over instead, as is one that cannot
//! start, and either stays with the forge for other runners, as do the jobs
//! of a repository whose runner was turned off. A job admitted as the system
//! shuts down waits in the queue again.
use std::{
    io::Write as _,
    os::unix::fs::OpenOptionsExt as _,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use rusqlite::params;

use super::{Registration, runner_labels};
use crate::client::space_names::SpaceSelector;
use crate::{
    client::resources::ResourceRequest,
    config::Runner,
    daemon::{allocation::Allocation, notifications::NotificationKind, workspace::Manager},
    forge::{
        ForgeRepo,
        ci::runner::{Installation, JobCommand, JobLaunch},
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
        if forge.run_superseded(&self.user_account(), &job.run).await? {
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
        let command = self.job_command(&claim, job).await?;
        let runner = command.runner;
        let launched = self.launch_claimed(&claim, command).await;
        if launched.is_err()
            && let Some(runner) = runner
            && let Err(error) = claim
                .forge
                .delete_runner(&self.user_account(), runner)
                .await
        {
            crate::daemon::log!("runner: {error:#}");
        }
        launched.map(|()| Claim::Started)
    }

    /// Put in place what `command` needs and run it for `claim`'s job,
    /// keeping the runner it registered for the job alone with the claim.
    async fn launch_claimed(&self, claim: &Claimed<'_>, command: JobCommand) -> Result<()> {
        if let Some(runner) = command.runner {
            self.record_job_runner(&claim.entry.id, runner).await?;
        }
        {
            // The sweep of unclaimed directories waits while one is filled.
            let _files = self.runners.job_files().await;
            prepare_job_files(claim.workspace, &command).await?;
        }
        let log = self.job_directory(&claim.entry.id).join("job.log");
        let execution = self
            .launch_job_command(claim.workspace, log, command)
            .await?;
        self.record_job_execution(&claim.entry.id, execution).await
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
        match self
            .acquire_resource(&SpaceSelector::id(&workspace.id), request, None)
            .await?
        {
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
                    params![id, repository, workspace, job, crate::time::UnixTime::now()],
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
                    params![id, execution, crate::time::UnixTime::now()],
                )?;
                Ok(())
            })
            .await
    }

    /// The command that runs `claim`'s job, with its files in the job's
    /// directory.
    async fn job_command(&self, claim: &Claimed<'_>, job: &QueuedJob) -> Result<JobCommand> {
        let program = self.runner_program(claim.forge)?;
        let labels = runner_labels(&claim.runner.labels);
        // Named for the claim: a job claimed again, after an earlier runner
        // took another job, may find that runner still registered.
        let claim_id: String = claim.entry.id.chars().take(8).collect();
        let name = format!("shoal-{}-{claim_id}", crate::process::machine::hostname());
        let launch = JobLaunch {
            runner: claim.registration.runner.as_ref(),
            program: &program,
            name: &name,
            work_dir: &self.job_directory(&claim.entry.id),
            workspace: &claim.workspace.path,
            labels: &labels,
            job: &job.job,
            env: &job_env(claim.workspace),
        };
        claim.forge.job_command(&self.user_account(), &launch).await
    }

    /// The directory a claimed job's runner keeps its files in.
    pub(super) fn job_directory(&self, id: &str) -> PathBuf {
        self.paths.state.join("runner").join(id)
    }

    /// Run `command` in `workspace` as a tracked execution that outlives the
    /// daemon, with its output in `log`, returning the execution's ID.
    #[cfg(not(test))]
    async fn launch_job_command(
        &self,
        workspace: &Workspace,
        log: PathBuf,
        command: JobCommand,
    ) -> Result<String> {
        let env: Vec<(&str, String)> = command
            .env
            .iter()
            .map(|(name, value)| (name.as_str(), value.clone()))
            .collect();
        let launch = crate::execution::launch_detached(
            &self.paths,
            workspace,
            log,
            command.argv,
            &[],
            &env,
            None,
        )
        .await?;
        Ok(launch.execution_id)
    }

    /// Tests run no command; they record it with an execution they end.
    #[cfg(test)]
    async fn launch_job_command(
        &self,
        workspace: &Workspace,
        _log: PathBuf,
        command: JobCommand,
    ) -> Result<String> {
        super::tests::launch(self, workspace, command).await
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
            .write(move |tx| {
                tx.execute("DELETE FROM agent_queue WHERE id=?1", [&id])?;
                tx.execute(
                    "INSERT OR IGNORE INTO ci_passed_jobs(repository_id,handle) VALUES (?1,?2)",
                    params![repository, handle],
                )?;
                Ok(())
            })
            .await
    }

    /// Remove a job's workspace whatever it holds; its claim goes with it.
    pub(super) async fn remove_job_workspace(&self, workspace_id: &str) -> Result<()> {
        self.remove_workspace(
            &SpaceSelector::id(workspace_id),
            crate::removal::UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await?;
        Ok(())
    }
}

/// Put in place what `command` needs before it runs: its files, readable by
/// the user alone, a copy of the runner's installation, and the links to the
/// job's `workspace` where the runner looks for its working directory.
async fn prepare_job_files(workspace: &Workspace, command: &JobCommand) -> Result<()> {
    for (path, contents) in &command.files {
        write_private(path, contents)?;
    }
    if let Some(installation) = &command.installation {
        copy_installation(installation).await?;
    }
    for link in &command.workspace_links {
        link_workspace(workspace, link)?;
    }
    Ok(())
}

/// Make `link` a symbolic link to `workspace`, with the directories above it.
fn link_workspace(workspace: &Workspace, link: &Path) -> Result<()> {
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::os::unix::fs::symlink(&workspace.path, link)
        .with_context(|| format!("link {}", link.display()))
}

fn write_private(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| file.write_all(contents.as_bytes()))
        .with_context(|| format!("write {}", path.display()))
}

/// Copy the entries of `installation`, cloned where the filesystem clones
/// files and copied otherwise.
async fn copy_installation(installation: &Installation) -> Result<()> {
    std::fs::create_dir_all(&installation.destination)
        .with_context(|| format!("create {}", installation.destination.display()))?;
    for entry in &installation.entries {
        let (source, destination) = (
            installation.source.join(entry),
            installation.destination.join(entry),
        );
        if crate::fsutil::clone_tree(&source, &destination)
            .await
            .is_err()
        {
            let mut copy = tokio::process::Command::new("cp");
            copy.arg("-pPR").arg("--").arg(&source).arg(&destination);
            crate::subprocess::output(copy)
                .await
                .with_context(|| format!("copy {}", source.display()))?;
        }
    }
    Ok(())
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
