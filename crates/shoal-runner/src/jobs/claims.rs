//! Claiming the jobs admission admitted: each runs the forge's runner in a
//! workspace of its own.
use std::{
    collections::BTreeMap, ffi::OsString, io::Write as _, os::unix::fs::OpenOptionsExt as _,
    path::Path,
};

use anyhow::{Context, Result};
use rusqlite::params;
use shoal_core::{
    config::{Runner as RunnerSettings, presets},
    fsutil::copy::clone_or_copy_keeping_times,
    process::machine::hostname,
};
use shoal_daemon_base::alerts::{About, NewAlert};
use shoal_forge::ci::runner::{Installation, JobCommand, JobLaunch, RunnerRequests};
use shoal_forge_types::ForgeRepo;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    alerts::AlertKind,
    env,
    model::{QueueEntry, QueuedJob, Workspace},
    space_names::SpaceName,
    state::{QueueKind, QueueState},
};

use super::{Registration, runner_labels};
use crate::{
    Runner,
    daemon::{Daemon, RecordedJobWorkspace},
    log,
    time::UnixTime,
};

/// The name of the lease a job holds from the repository's permit pool.
const PERMIT: &str = "ci-job";

/// The niceness a job's runner runs at.
const JOB_NICENESS: &str = "10";

/// How a claim ended.
enum Claim {
    Started,
    /// The job's PR moved on, so its run is superseded and was cancelled.
    Superseded,
    /// The permit pool is full, so the job waits again.
    PermitsBusy,
    /// The repository's runner was turned off, so the job leaves the queue.
    Withdrawn,
    /// The system shuts down, so the job waits for the queue to admit it again.
    Suspended,
}

/// What starting a claimed job needs.
struct Claimed<'a> {
    entry: &'a QueueEntry,
    workspace: &'a Workspace,
    forge: &'a ForgeRepo,
    registration: &'a Registration,
    runner: &'a RunnerSettings,
}

impl Runner {
    /// The job entries admission admitted, which the runner claims.
    pub async fn admitted_jobs(&self, daemon: &dyn Daemon) -> Result<Vec<QueueEntry>> {
        Ok((daemon.queue_entries().await?)
            .into_iter()
            .filter(|entry| entry.kind == QueueKind::Job && entry.state == QueueState::Starting)
            .collect())
    }

    /// Claim the admitted job `entry`. One that cannot start is passed over,
    /// which the user hears about.
    pub async fn claim_job(&self, daemon: &dyn Daemon, entry: &QueueEntry) {
        let error = match self.try_claim_job(daemon, entry).await {
            Ok(Claim::Started) => return,
            Ok(Claim::Superseded) => None,
            Ok(Claim::Withdrawn) => return self.withdraw_claimed(entry).await,
            Ok(Claim::PermitsBusy) => {
                if let Ok(repository) = entry.job_repository() {
                    self.jobs.found_permits_busy(repository);
                }
                return self.return_claimed(entry).await;
            }
            Ok(Claim::Suspended) => return self.return_claimed(entry).await,
            Err(error) => Some(error),
        };
        if let Err(error) = self.pass_over_claimed(entry).await {
            log!("runner: {error:#}");
        }
        if let Some(error) = error {
            let message = format!(
                "CI job {} did not start; the forge keeps it for other runners: {error:#}",
                entry.name
            );
            let about = About::named(Some(&entry.name));
            let alert = NewAlert::shoal(AlertKind::RunnerFailed, about, message);
            self.notifications.notify(daemon, alert).await;
        }
    }

    async fn try_claim_job(&self, daemon: &dyn Daemon, entry: &QueueEntry) -> Result<Claim> {
        let job = entry.job.as_deref().context("the entry names no job")?;
        let repository = entry.job_repository()?;
        let config = self.config();
        let settings = daemon.repository_settings_from(&config, repository).await?;
        if !settings.runner.enabled {
            return Ok(Claim::Withdrawn);
        }
        if daemon.queue_suspended() {
            return Ok(Claim::Suspended);
        }
        let registration = (self.runner_registration(repository).await?)
            .context("the repository's runner is no longer registered")?;
        let forge = self.forges.forge(&registration.forge_repository).await?;
        if (self.cancel_if_superseded(daemon, &entry.name, &forge, &job.run)).await? {
            return Ok(Claim::Superseded);
        }
        let workspace = (self.create_claimed_workspace(daemon, entry, &forge, job)).await?;
        let claim = Claimed {
            entry,
            workspace: &workspace,
            forge: &forge,
            registration: &registration,
            runner: &settings.runner,
        };
        let started = self.start_claimed(daemon, claim).await;
        if !matches!(started, Ok(Claim::Started)) {
            discard_job_workspace(daemon, &workspace.id).await;
        }
        started
    }

    /// Create the workspace `entry`'s job runs in, claimed by the job, and
    /// remove one whose checkout failed; a removal that fails is tried again
    /// until it succeeds.
    pub async fn create_claimed_workspace(
        &self,
        daemon: &dyn Daemon,
        entry: &QueueEntry,
        forge: &ForgeRepo,
        job: &QueuedJob,
    ) -> Result<Workspace> {
        let recorded = (self.record_claimed_workspace(daemon, entry, forge, job)).await?;
        let id = recorded.id().clone();
        let created = recorded.check_out().await;
        if created.is_err() {
            discard_job_workspace(daemon, &id).await;
        }
        created
    }

    /// Record the workspace `entry`'s job runs in, and the job's claim there.
    /// It records them before the workspace is checked out, takes a permit or
    /// runs anything, so the claim's end removes the workspace with what it
    /// holds, also after a restart.
    pub async fn record_claimed_workspace<'a>(
        &self,
        daemon: &'a dyn Daemon,
        entry: &'a QueueEntry,
        forge: &ForgeRepo,
        job: &'a QueuedJob,
    ) -> Result<Box<dyn RecordedJobWorkspace<'a> + 'a>> {
        let names = (entry.name.as_str(), SpaceName::derive(&job.job.name, "ci"));
        let reference = job.run.reference.as_deref();
        let recorded =
            daemon.record_job_workspace(entry.job_repository()?, names, &job.run.commit, reference);
        let recorded = recorded.await?;
        let id = recorded.id().clone();
        if let Err(error) = self.record_claim(daemon, entry, &id, forge, job).await {
            // The removal takes the repository's Git gate it holds.
            drop(recorded);
            discard_job_workspace(daemon, &id).await;
            return Err(error);
        }
        Ok(recorded)
    }

    async fn start_claimed(&self, daemon: &dyn Daemon, claim: Claimed<'_>) -> Result<Claim> {
        if let Some(pool) = &claim.runner.permit
            && !daemon
                .hold_permit(&claim.workspace.id, pool, PERMIT)
                .await?
        {
            return Ok(Claim::PermitsBusy);
        }
        let job = (claim.entry.job.as_deref()).context("the entry names no job")?;
        // Held until the launch is recorded, so a system shutdown, which
        // suspends the queue under the same gate, either stops the job with
        // the rest or finds it not started.
        let _admission = daemon.lock_admission().await;
        if daemon.queue_suspended() {
            return Ok(Claim::Suspended);
        }
        self.take_over_entry(&claim.entry.id).await?;
        let command = self.job_command(daemon, &claim, job).await?;
        let runner = command.runner;
        let launched = self.launch_claimed(daemon, &claim, command).await;
        if launched.is_err()
            && let Some(runner) = runner
            && let Err(error) = claim
                .forge
                .delete_runner(&self.forges.user_account(), runner)
                .await
        {
            log!("runner: {error:#}");
        }
        launched.map(|()| Claim::Started)
    }

    /// Put in place what `command` needs and run it for `claim`'s job,
    /// keeping the runner it registered for the job alone with the claim.
    async fn launch_claimed(
        &self,
        daemon: &dyn Daemon,
        claim: &Claimed<'_>,
        command: JobCommand,
    ) -> Result<()> {
        if let Some(runner) = command.runner {
            self.record_job_runner(&claim.entry.id, runner).await?;
        }
        {
            // The sweep of unclaimed directories waits while one is filled.
            let _files = self.jobs.job_files().await;
            prepare_job_files(claim.workspace, &command).await?;
        }
        let log = self.job_directory(&claim.entry.id).join("job.log");
        let command = at_low_priority(command);
        let execution = daemon
            .launch_job_command(claim.workspace, log, command)
            .await?;
        self.record_job_execution(&claim.entry.id, execution).await
    }

    /// Record the job of `entry` as claimed in `workspace`, with the
    /// spaces its run links now. Its entry keeps the job's slot until
    /// [`Self::take_over_entry`], so a claim that cannot go on puts the job
    /// back.
    async fn record_claim(
        &self,
        daemon: &dyn Daemon,
        entry: &QueueEntry,
        workspace: &SpaceId,
        forge: &ForgeRepo,
        job: &QueuedJob,
    ) -> Result<()> {
        let repository = entry.job_repository()?;
        let linked = self.link_job(daemon, forge, repository, job.job.clone(), job.run.clone());
        let job = shoal_store::jobs::JobColumns::of(&linked.await?)?;
        let (id, repository, workspace) =
            (entry.id.clone(), repository.clone(), workspace.to_owned());
        self.store
            .run(move |db| {
                db.execute(
                    "INSERT INTO ci_jobs(id,repository_id,workspace_id,job,handle,started_at)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        id,
                        repository,
                        workspace,
                        job.job,
                        job.handle,
                        UnixTime::now()
                    ],
                )?;
                Ok(())
            })
            .await
    }

    /// The claimed job of the entry `id` takes over the slot its entry held.
    async fn take_over_entry(&self, id: &shoal_ids::queue_entry_ids::QueueEntryId) -> Result<()> {
        let (records, id) = (self.records.clone(), id.to_owned());
        self.store
            .run(move |db| records.remove_queue_entry(db, &id))
            .await
    }

    /// The command that runs `claim`'s job, with its files in the job's
    /// directory and the variables its workspace's presets give jobs.
    async fn job_command(
        &self,
        daemon: &dyn Daemon,
        claim: &Claimed<'_>,
        job: &QueuedJob,
    ) -> Result<JobCommand> {
        let mut command = self.forge_job_command(claim, job).await?;
        let environment = preset_job_environment(daemon, claim.workspace).await?;
        command.env.extend(environment);
        // The job's own admission counts its `shoal run` commands, which
        // would otherwise wait for the memory it holds.
        let admitted = (shoal_protocol::env::ADMITTED_RUN.to_owned(), "1".to_owned());
        command.env.extend([admitted]);
        Ok(command)
    }

    async fn forge_job_command(&self, claim: &Claimed<'_>, job: &QueuedJob) -> Result<JobCommand> {
        let program = self.runner_program(claim.forge)?;
        let labels = runner_labels(&claim.runner.labels);
        // Named for the claim: a job claimed again, after an earlier runner
        // took another job, may find that runner still registered.
        let claim_id: String = claim.entry.id.as_str().chars().take(8).collect();
        let name = format!("shoal-{}-{claim_id}", hostname());
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
        claim
            .forge
            .job_command(&self.forges.user_account(), &launch)
            .await
    }

    /// Take the jobs of `entry`'s repository out of the queue, as its runner
    /// was turned off, logging a failure.
    async fn withdraw_claimed(&self, entry: &QueueEntry) {
        let forgotten = match entry.job_repository() {
            Ok(repository) => self.forget_job_entries(repository).await,
            Err(error) => Err(error),
        };
        if let Err(error) = forgotten {
            log!("runner: {error:#}");
        }
    }

    /// Put the admitted job of `entry` back in the queue, where it waits
    /// again, logging a failure.
    async fn return_claimed(&self, entry: &QueueEntry) {
        let (records, id) = (self.records.clone(), entry.id.clone());
        let returned = self.store.run(move |db| records.return_job_entry(db, &id));
        if let Err(error) = returned.await {
            log!("runner: {error:#}");
        }
    }

    /// Leave the job of `entry`, which the runner did not start, to other
    /// runners.
    async fn pass_over_claimed(&self, entry: &QueueEntry) -> Result<()> {
        let job = entry.job.as_deref().context("the entry names no job")?;
        let records = self.records.clone();
        let (id, repository, handle) = (
            entry.id.clone(),
            entry.job_repository()?.clone(),
            job.job.handle.clone(),
        );
        self.store
            .write(move |tx| {
                records.remove_queue_entry(tx, &id)?;
                tx.execute(
                    "INSERT OR IGNORE INTO ci_passed_jobs(repository_id,handle) VALUES (?1,?2)",
                    params![repository, handle],
                )?;
                Ok(())
            })
            .await
    }
}

/// Remove the job workspace `id` that a claim could not use, logging a
/// removal that fails.
async fn discard_job_workspace(daemon: &dyn Daemon, id: &SpaceId) {
    if let Err(error) = daemon.remove_job_workspace(id).await {
        log!("runner: {error:#}");
    }
}

/// The variables the presets of the job workspace `workspace` give its job
/// where `[env]` does not set them. They reach the steps through the
/// runner's own environment, which a workflow's `env` replaces on both
/// forges, unlike the variables [`job_env`] gives the runner.
async fn preset_job_environment(
    daemon: &dyn Daemon,
    workspace: &Workspace,
) -> Result<BTreeMap<String, String>> {
    let settings = daemon.workspace_settings(workspace).await?;
    let names = settings.setup.presets.as_deref().unwrap_or_default();
    Ok(presets::job_environment(names, &settings.env))
}

/// `command` run through `nice` at [`JOB_NICENESS`], which its whole process
/// tree inherits.
fn at_low_priority(mut command: JobCommand) -> JobCommand {
    let nice = ["nice", "-n", JOB_NICENESS].map(OsString::from);
    command.argv.splice(0..0, nice);
    command
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
        clone_or_copy_keeping_times(&source, &destination).await?;
    }
    Ok(())
}

/// The variables the workflow's `env` context and every step of a job get
/// besides the runner's own environment. With them, a job's steps find
/// their workspace and tell that they run on Shoal.
fn job_env(workspace: &Workspace) -> Vec<(String, String)> {
    vec![
        (
            env::WORKSPACE_PATH.into(),
            workspace.path.to_string_lossy().into_owned(),
        ),
        (env::SPACE.into(), workspace.id.to_string()),
    ]
}
