//! Keep each repository's seed build at its default branch's tip and start
//! new workspaces from it. Seed failures never fail the operation that met
//! them: a workspace without a seed still builds, only from scratch.
use shoal_ids::repository_ids::RepositoryId;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fmt, fs,
    io::{Read as _, Seek as _, Write as _},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use shoal_refs::{BranchName, CommitId};
use tokio::{process::Command, sync::watch};

use crate::{
    config::Effective,
    daemon::{
        capacity::{self, MemoryState},
        log,
        manager::Manager,
        notifications::AlertKind,
    },
    fsutil::{
        off_runtime,
        subvolume::{self, Btrfs},
    },
    git::{
        self,
        default_branch::{self, DefaultBranchLookup},
    },
    model::{Repository, Workspace},
    seed::{
        Layout, Snapshot, Staged,
        build::{self, Build, Previous, Step, Toolchains},
    },
};

/// How often the daemon looks for a default branch that moved.
const REFRESH_INTERVAL: Duration = Duration::from_secs(60);
/// Longest a seed build may run before it is killed and reported.
const BUILD_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);

/// What one repository's seed builds, from its resolved `[setup.seed]` table.
struct Plan {
    branch: BranchName,
    build: Build,
    paths: Vec<String>,
    environment: BTreeMap<String, OsString>,
    caches: BTreeMap<String, crate::config::repo::Cache>,
}

impl Plan {
    /// The seed `settings` describe, or `None` when they configure none.
    async fn new(repo: &Repository, layout: &Layout, settings: &Effective) -> Result<Option<Self>> {
        let presets = settings.setup.presets.as_deref().unwrap_or_default();
        let Some(build) = Build::of(&settings.setup.seed, presets, &layout.worktree()) else {
            return Ok(None);
        };
        let branch = default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await?;
        // Builds see the workspaces' [env], so their outputs stay reusable.
        Ok(Some(Self {
            build,
            paths: build::outputs(&settings.setup.seed, presets),
            environment: crate::env::render_seed_environment(
                &settings.env,
                &layout.worktree(),
                branch.as_str(),
                repo.display_name(),
                &crate::caches::locate(repo, &settings.caches),
            ),
            caches: settings.caches.clone(),
            branch,
        }))
    }

    /// What a build depends on besides the commit, so a snapshot built under
    /// other settings is rebuilt.
    fn settings(&self) -> Result<String> {
        Ok(serde_json::to_string(&(
            &self.build,
            &self.paths,
            &self.environment,
        ))?)
    }
}

pub async fn run(manager: Arc<Manager>) {
    loop {
        manager.refresh_seeds().await;
        tokio::time::sleep(REFRESH_INTERVAL).await;
    }
}

/// Refill spares apart from builds, which can run for minutes: when a
/// workspace took one or a build froze a snapshot, and every
/// [`REFRESH_INTERVAL`] after a failure or a daemon restart.
pub async fn run_spares(manager: Arc<Manager>) {
    loop {
        manager.refill_spares().await;
        tokio::select! {
            _ = tokio::time::sleep(REFRESH_INTERVAL) => {}
            _ = manager.seeds.spare_wanted() => {}
        }
    }
}

impl Manager {
    /// Rebuild, one at a time, each seed whose default branch moved since its
    /// snapshot. A commit whose build failed is not retried until the branch
    /// moves again or the daemon restarts; one whose soft steps alone failed
    /// built a current snapshot.
    pub async fn refresh_seeds(&self) {
        let repositories = match self.repositories.list().await {
            Ok(repositories) => repositories,
            Err(error) => return log!("seed refresh: {error:#}"),
        };
        for repo in repositories {
            if let Err(error) = self.refresh_seed(&repo).await {
                let name = repo.display_name();
                self.notify(
                    None,
                    AlertKind::SeedFailed,
                    format!("Seed build for {name} failed: {error:#}"),
                )
                .await;
            }
        }
    }

    async fn refresh_seed(&self, repo: &Repository) -> Result<()> {
        let layout = Layout::of(repo);
        let Some(plan) = self.seed_plan(repo, &layout).await? else {
            return self.discard_seed(repo, &layout).await;
        };
        let tip = git::seed::branch_commit(&repo.path, &plan.branch).await?;
        let build = (tip, plan.settings()?);
        let current = Snapshot::open(&layout)?
            .is_some_and(|snapshot| (snapshot.commit, snapshot.settings) == build);
        // Checked first: seeing any other build, a current one included,
        // forgets the failed one.
        let failed = self.seeds.repeats_failure(&repo.id, &build);
        if failed || current || !self.seed_headroom(&layout) {
            return Ok(());
        }
        let result = self.build_seed(repo, &layout, &plan, &build).await;
        self.seeds
            .record_result(&repo.id, result.is_err().then_some(build));
        self.report_soft_failures(repo, &result?).await;
        Ok(())
    }

    /// Tell the user which soft steps of the repository's installed seed
    /// failed, so new workspaces start without their outputs.
    async fn report_soft_failures(&self, repo: &Repository, failures: &[StepFailure]) {
        let name = repo.display_name();
        for failure in failures {
            self.notify(
                None,
                AlertKind::SeedFailed,
                format!(
                    "Seed build for {name} kept the outputs of its other steps: {failure}; \
                     new workspaces start without its outputs"
                ),
            )
            .await;
        }
    }

    /// The repository's seed settings, or `None` when it configures no seed.
    async fn seed_plan(&self, repo: &Repository, layout: &Layout) -> Result<Option<Plan>> {
        let settings = self.repository_settings(&repo.id).await?;
        Plan::new(repo, layout, &settings).await
    }

    /// Delete the seed of a repository that no longer configures one.
    async fn discard_seed(&self, repo: &Repository, layout: &Layout) -> Result<()> {
        if !layout.root().exists() {
            return Ok(());
        }
        let _seed = self.stop_seed_build(&repo.id).await;
        let _git = self.gates.lock_git(&repo.id).await;
        self.repositories.ensure_available(&repo.id).await?;
        self.remove_seed(repo).await?;
        self.seeds.record_result(&repo.id, None);
        Ok(())
    }

    /// Check the seed worktree out at `tip`, build it and freeze the outputs.
    /// Returns how its soft steps failed. Repository removal cancels the
    /// build and waits for it to return.
    async fn build_seed(
        &self,
        repo: &Repository,
        layout: &Layout,
        plan: &Plan,
        build: &(CommitId, String),
    ) -> Result<Vec<StepFailure>> {
        let tip = &build.0;
        let slot = self.seeds.slot(&repo.id);
        let _build = slot.gate.lock().await;
        if self.repositories.repository(&repo.id).await.is_err() {
            return Ok(Vec::new());
        }
        {
            let _git = self.gates.lock_git(&repo.id).await;
            self.repositories.ensure_available(&repo.id).await?;
            check_out_seed(repo, layout, tip).await?;
        }
        // Under the gate, which keeps cache emptying out until the build ends.
        crate::caches::directories(repo, &plan.caches)?;
        let previous = previous_build(layout, tip).await?;
        log!("seed build for {} at {tip}", repo.path.display());
        let built = run_build(
            plan,
            previous.as_ref(),
            &layout.worktree(),
            &layout.log(),
            slot.cancel.subscribe(),
        )
        .await?;
        self.freeze_seed(repo, layout, plan, build, built.toolchains)
            .await?;
        // The new snapshot needs its own spare.
        self.seeds.want_spare();
        Ok(built.soft_failures)
    }

    /// Replace the snapshot with the outputs the seed worktree built with
    /// `toolchains`, cloning them while clones of the current snapshot go on
    /// and deleting the replaced one once nothing reads it. The caller holds the build slot,
    /// which keeps every other change to the snapshots out.
    async fn freeze_seed(
        &self,
        repo: &Repository,
        layout: &Layout,
        plan: &Plan,
        build: &(CommitId, String),
        toolchains: Toolchains,
    ) -> Result<()> {
        let staged = Snapshot::stage(layout, &plan.paths).await?;
        self.install_snapshot(repo, layout, staged, build, toolchains)
            .await?;
        let retired = layout.clone();
        off_runtime::run(move || retired.remove_retired()).await
    }

    /// Make `staged` the snapshot of the seed worktree built at `tip` with
    /// `toolchains`, checked under the Git gate while the repository is
    /// available, once the clones of the current snapshot end.
    async fn install_snapshot(
        &self,
        repo: &Repository,
        layout: &Layout,
        staged: Staged,
        (tip, settings): &(CommitId, String),
        toolchains: Toolchains,
    ) -> Result<()> {
        let slot = self.seeds.slot(&repo.id);
        let _snapshot = slot.snapshot.write().await;
        let _git = self.gates.lock_git(&repo.id).await;
        self.repositories.ensure_available(&repo.id).await?;
        let worktree = layout.worktree();
        ensure!(
            git::seed::head(&worktree).await? == *tip,
            "the seed worktree moved during its build"
        );
        let files = git::seed::unmodified_files(&worktree).await?;
        staged.install(
            layout,
            tip.to_owned().into(),
            settings.to_owned(),
            toolchains,
            files,
        )
    }

    /// Give each repository's current snapshot a spare clone of its outputs,
    /// which the next new workspace takes instead of cloning them.
    pub async fn refill_spares(&self) {
        let repositories = match self.repositories.list().await {
            Ok(repositories) => repositories,
            Err(error) => return log!("seed spares: {error:#}"),
        };
        for repo in repositories {
            if let Err(error) = self.refill_spare(&repo.id).await {
                let name = repo.display_name();
                log!("seed spare for {name}: {error:#}");
            }
        }
    }

    /// Clone a spare of the repository's snapshot when it has none, its
    /// settings still describe the snapshot and the host has headroom for it.
    /// Starts clone and take the spare meanwhile.
    async fn refill_spare(&self, id: &RepositoryId) -> Result<()> {
        let slot = self.seeds.slot(id);
        let _snapshot = slot.snapshot.read().await;
        // Read under the lock: a move or removal of the repository holds it.
        let Ok(repo) = self.repositories.repository(id).await else {
            return Ok(());
        };
        let layout = Layout::of(&repo);
        let Some(snapshot) = Snapshot::open(&layout)? else {
            return Ok(());
        };
        // A start meanwhile clones, as it would without spares.
        if layout.has_spare() || !self.seed_headroom(&layout) {
            return Ok(());
        }
        let Some(plan) = self.seed_plan(&repo, &layout).await? else {
            return Ok(());
        };
        if snapshot.settings == plan.settings()? {
            snapshot.refill_spare(&layout).await?;
        }
        Ok(())
    }

    /// Start a new worktree from its repository's seed snapshot when the
    /// worktree's own settings describe the build that made it. The caller
    /// holds the workspace's creation gate and not the repository's Git gate.
    pub(super) async fn apply_seed(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        settings: &Effective,
    ) {
        let layout = Layout::of(repo);
        let started = async {
            let Some(plan) = Plan::new(repo, &layout, settings).await? else {
                return Ok(());
            };
            let slot = self.seeds.slot(&repo.id);
            let _snapshot = slot.snapshot.read().await;
            let Some(snapshot) = Snapshot::open(&layout)? else {
                return Ok(());
            };
            layout.ensure_owned()?;
            if snapshot.settings != plan.settings()? {
                log!(
                    "{} starts without the seed: its settings differ",
                    workspace.name
                );
                return Ok(());
            }
            self.start_from_snapshot(&slot, &layout, &snapshot, &workspace.path, &plan.paths)
                .await
        };
        if let Err(error) = started.await {
            self.notify_space(
                &workspace.name,
                &workspace.id,
                AlertKind::SeedFailed,
                format!("Workspace starts without the seed build: {error:#}"),
            )
            .await;
        }
    }

    /// Give the new `worktree` the snapshot's outputs among `paths`: the
    /// spare's, which the spare task then replaces, or else a clone of them.
    /// The caller reads the snapshot under `slot`.
    async fn start_from_snapshot(
        &self,
        slot: &SeedSlot,
        layout: &Layout,
        snapshot: &Snapshot,
        worktree: &Path,
        paths: &[String],
    ) -> Result<()> {
        let spare = {
            let _taking = slot.spare.lock().await;
            snapshot.take_spare(layout, worktree, paths)?
        };
        let outputs = match spare {
            Some(taken) => {
                self.seeds.want_spare();
                taken
            }
            None => snapshot.clone_outputs(layout, worktree, paths).await?,
        };
        // Restored times are only safe beside the outputs built after them: on
        // outputs the worktree already had, they would hide changed sources.
        if outputs.is_empty() {
            return Ok(());
        }
        let changed =
            git::seed::changed_between(worktree, snapshot.commit.as_str(), "HEAD").await?;
        snapshot.restore_times(worktree, &changed)
    }

    /// Stop the repository's seed build, if one runs, and keep new ones and
    /// spare refills out until the returned guard drops. Take it before the
    /// Git gate, which a build takes while holding its own.
    pub(super) async fn stop_seed_build(&self, repository_id: &RepositoryId) -> SeedStop {
        self.pause_seed_build(repository_id).await.stop().await
    }

    /// Stop the repository's seed build, if one runs, and keep new ones out
    /// until the returned guard drops, while new worktrees still clone the
    /// snapshot. Take it before the Git gate, which a build takes while
    /// holding its own.
    pub(super) async fn pause_seed_build(&self, repository_id: &RepositoryId) -> SeedPause {
        let slot = self.seeds.slot(repository_id);
        slot.cancel.send_replace(true);
        let guard = slot.gate.clone().lock_owned().await;
        SeedPause {
            _guard: guard,
            slot,
        }
    }

    /// Keep the repository's seed builds from starting until the returned
    /// guard drops, or `None` while one runs.
    pub(super) fn pause_idle_seed(
        &self,
        repository_id: &RepositoryId,
    ) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        self.seeds
            .slot(repository_id)
            .gate
            .clone()
            .try_lock_owned()
            .ok()
    }

    /// Delete the repository's seed. The caller holds a [`SeedStop`] and the
    /// repository's Git gate.
    pub(super) async fn remove_seed(&self, repo: &Repository) -> Result<()> {
        let layout = Layout::of(repo);
        let worktree = layout.worktree();
        if worktree.symlink_metadata().is_ok() {
            ensure_seed_worktree(repo, &layout).await?;
            let outputs = layout.clone();
            off_runtime::run(move || {
                outputs.destroy_worktree_outputs();
                Ok(())
            })
            .await?;
            git::seed::remove_worktree(&repo.path, &worktree).await?;
        }
        tokio::task::spawn_blocking(move || layout.remove_owned()).await?
    }

    /// Destroy the subvolumes among the seed outputs `workspace`'s settings
    /// name, which a worktree started from the seed holds, so removing the
    /// worktree need not delete their files one by one. An output holding a
    /// tracked file stays, so Git still finds a clean worktree clean, and
    /// settings or files it cannot read leave every output to that delete.
    pub(super) async fn destroy_seed_outputs(&self, workspace: &Workspace) {
        let Ok(settings) = self.workspace_settings(workspace).await else {
            return;
        };
        let presets = settings.setup.presets.as_deref().unwrap_or_default();
        let outputs = build::outputs(&settings.setup.seed, presets);
        let Ok(outputs) = git::seed::untracked_outputs(&workspace.path, &outputs).await else {
            return;
        };
        let root = workspace.path.clone();
        let destroyed = off_runtime::run(move || {
            subvolume::destroy_outputs(&Btrfs, &root, &outputs);
            Ok(())
        });
        let _ = destroyed.await;
    }

    /// Whether memory and disk space are clear of their overload warning
    /// thresholds, so a seed build or spare adds no load to a strained host.
    /// A failed reading defers it.
    fn seed_headroom(&self, layout: &Layout) -> bool {
        let overload = &self.config().machine.overload;
        let paths = [self.paths.state.as_path(), layout.root()];
        let sample = capacity::sample(&paths, overload, self.work_limit());
        let disk_floor = if overload.warning.enabled {
            overload.warning.disk_free_bytes()
        } else {
            0
        };
        sample.memory == MemoryState::Healthy
            && sample.disk_free.is_some_and(|free| free >= disk_floor)
    }
}

/// What the current snapshot was built from, with the files changed between
/// its commit and `tip`, or `None` without a snapshot. The caller holds the
/// build slot, which keeps the snapshot from changing.
async fn previous_build(layout: &Layout, tip: &CommitId) -> Result<Option<Previous>> {
    let Some(snapshot) = Snapshot::open(layout)? else {
        return Ok(None);
    };
    let worktree = layout.worktree();
    let commit = snapshot.commit.as_str();
    let changed = if git::has_commit(&worktree, commit).await? {
        Some(git::seed::changed_between(&worktree, commit, tip.as_str()).await?)
    } else {
        None
    };
    Ok(Some(Previous {
        settings: snapshot.settings,
        toolchains: snapshot.toolchains,
        changed,
    }))
}

async fn check_out_seed(repo: &Repository, layout: &Layout, tip: &CommitId) -> Result<()> {
    layout.claim()?;
    let worktree = layout.worktree();
    if worktree.symlink_metadata().is_ok() {
        ensure_seed_worktree(repo, layout).await?;
        return git::seed::check_out(&worktree, tip.as_str()).await;
    }
    git::seed::add_worktree(&repo.path, &worktree, tip.as_str(), &layout.lock_reason()).await?;
    shoal_daemon_base::ownership::mark_owner(
        &git::git_dir(&worktree).await?,
        &layout.worktree_owner(),
    )
}

/// Refuse to change or delete a `.seed/worktree` unless it is the worktree
/// Shoal created for this repository's seed.
async fn ensure_seed_worktree(repo: &Repository, layout: &Layout) -> Result<()> {
    ensure!(
        is_seed_worktree(repo, layout).await?,
        "{} is not the seed worktree Shoal created; move it away to rebuild or remove the seed",
        layout.worktree().display()
    );
    Ok(())
}

/// Whether `.seed` is this repository's marked directory, `.seed/worktree`
/// is a real directory, Git run in
/// `.seed/worktree` reaches that worktree of this repository, and its admin
/// directory carries Shoal's seed marker, which Git keeps across moves and
/// drops when the worktree is re-created.
pub(super) async fn is_seed_worktree(repo: &Repository, layout: &Layout) -> Result<bool> {
    let worktree = layout.worktree();
    let real = fs::symlink_metadata(&worktree).is_ok_and(|metadata| metadata.is_dir());
    if !real || layout.ensure_owned().is_err() {
        return Ok(false);
    }
    let Ok(dirs) = git::checkout_dirs(&worktree).await else {
        return Ok(false);
    };
    if fs::canonicalize(&worktree).ok().as_ref() != Some(&dirs.root)
        || dirs.common_dir != git::common_dir(&repo.path).await?
    {
        return Ok(false);
    }
    let owner = shoal_daemon_base::ownership::marked_owner(&dirs.git_dir)?;
    Ok(owner == Some(layout.worktree_owner()))
}

/// One repository's seed build: held while a build runs, and set to cancel it.
pub(crate) struct SeedSlot {
    gate: Arc<tokio::sync::Mutex<()>>,
    cancel: watch::Sender<bool>,
    /// Read while a clone of the snapshot's outputs runs, and written while
    /// the snapshot is swapped, which a build does only once it has built,
    /// or deleted.
    snapshot: Arc<tokio::sync::RwLock<()>>,
    /// Held while a new worktree takes the spare, which has one taker.
    spare: tokio::sync::Mutex<()>,
}

impl Default for SeedSlot {
    fn default() -> Self {
        Self {
            gate: Default::default(),
            cancel: watch::channel(false).0,
            snapshot: Default::default(),
            spare: Default::default(),
        }
    }
}

/// Keeps seed builds out of a repository; dropping it allows them again.
pub(super) struct SeedPause {
    _guard: tokio::sync::OwnedMutexGuard<()>,
    slot: Arc<SeedSlot>,
}

impl SeedPause {
    /// Keep spare refills and clones of the snapshot out too, once those
    /// under way end. Only clones hold the snapshot while builds are
    /// paused, so the Git gate may be held already.
    pub(super) async fn stop(self) -> SeedStop {
        let snapshot = self.slot.snapshot.clone().write_owned().await;
        SeedStop {
            _pause: self,
            _snapshot: snapshot,
        }
    }
}

impl Drop for SeedPause {
    fn drop(&mut self) {
        self.slot.cancel.send_replace(false);
    }
}

/// Keeps seed builds, spare refills and clones of the snapshot out of a
/// repository; dropping it allows them again.
pub(super) struct SeedStop {
    _pause: SeedPause,
    _snapshot: tokio::sync::OwnedRwLockWriteGuard<()>,
}

/// What a seed build that went to its end built with, and how its soft
/// steps failed.
struct Built {
    toolchains: Toolchains,
    soft_failures: Vec<StepFailure>,
}

/// A step that ran to its end and exited non-zero.
struct StepFailure {
    label: String,
    code: i32,
    log: PathBuf,
}

impl fmt::Display for StepFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { label, code, log } = self;
        write!(
            f,
            "{label} exited with {code}; output is in {}",
            log.display()
        )
    }
}

/// Run the seed build in its worktree, writing its output to `log`: name
/// each preset's toolchain, reset the outputs `previous` left that this build
/// would not reuse, then run the build's commands in order, until one that
/// is not soft fails, the build times out or `cancel` is set.
async fn run_build(
    plan: &Plan,
    previous: Option<&Previous>,
    worktree: &Path,
    log: &Path,
    cancel: watch::Receiver<bool>,
) -> Result<Built> {
    let mut steps = Steps::new(plan, worktree, log, cancel)?;
    let toolchains = identify_toolchains(&mut steps, &plan.build).await?;
    let resets = plan.build.resets(previous, &plan.settings()?, &toolchains);
    let mut soft_failures = Vec::new();
    for step in plan.build.steps(&resets) {
        soft_failures.extend(steps.run(&step).await?);
    }
    Ok(Built {
        toolchains,
        soft_failures,
    })
}

/// Run each preset's toolchain command and keep what it printed.
async fn identify_toolchains(steps: &mut Steps<'_>, build: &Build) -> Result<Toolchains> {
    let mut toolchains = Toolchains::new();
    for (preset, step) in build.toolchain_steps() {
        toolchains.insert(preset, steps.capture(&step).await?);
    }
    Ok(toolchains)
}

/// Runs a seed build's commands one at a time in its worktree, each in the
/// log, until the build's deadline.
struct Steps<'a> {
    plan: &'a Plan,
    worktree: &'a Path,
    log: &'a Path,
    output: fs::File,
    deadline: tokio::time::Instant,
    cancel: watch::Receiver<bool>,
}

impl<'a> Steps<'a> {
    fn new(
        plan: &'a Plan,
        worktree: &'a Path,
        log: &'a Path,
        cancel: watch::Receiver<bool>,
    ) -> Result<Self> {
        Ok(Self {
            plan,
            worktree,
            log,
            output: fs::File::create(log).with_context(|| format!("create {}", log.display()))?,
            deadline: tokio::time::Instant::now() + BUILD_TIMEOUT,
            cancel,
        })
    }

    /// Run `step`, writing its output to the log. Returns how a soft step
    /// failed; any other failure is an error.
    async fn run(&mut self, step: &Step) -> Result<Option<StepFailure>> {
        writeln!(self.output, "$ {}", step.label)?;
        let stdout = self.output.try_clone()?;
        let status = self.wait(step, &stdout).await?;
        match self.failure(step, status) {
            Some(failure) if !step.soft => bail!("{failure}"),
            soft => Ok(soft),
        }
    }

    /// Run `step` and return what it printed to standard output, which the
    /// log gets too.
    async fn capture(&mut self, step: &Step) -> Result<String> {
        writeln!(self.output, "$ {}", step.label)?;
        let mut stdout = tempfile::tempfile()?;
        let waited = self.wait(step, &stdout).await;
        let mut printed = Vec::new();
        stdout.seek(std::io::SeekFrom::Start(0))?;
        stdout.read_to_end(&mut printed)?;
        self.output.write_all(&printed)?;
        if let Some(failure) = self.failure(step, waited?) {
            bail!("{failure}");
        }
        Ok(String::from_utf8_lossy(&printed).into_owned())
    }

    /// How `step` failed when it exited with `status`, or `None` when it
    /// succeeded.
    fn failure(&self, step: &Step, status: ExitStatus) -> Option<StepFailure> {
        (!status.success()).then(|| StepFailure {
            label: step.label.clone(),
            code: crate::execution::exit_code(status),
            log: self.log.to_owned(),
        })
    }

    /// Run `step`, its standard output to `stdout` and its errors to the
    /// log, until it exits, the build's deadline passes or `cancel` is set,
    /// and return how it exited. Whatever it leaves in its process group is
    /// killed before this returns.
    async fn wait(&mut self, step: &Step, stdout: &fs::File) -> Result<ExitStatus> {
        let mut child = step_command(step, self.plan, self.worktree, stdout, &self.output)?
            .spawn()
            .with_context(|| format!("launch {}", step.label))?;
        let group = child.id().context("seed build process ID unavailable")? as i32;
        // Dropping the build, as daemon shutdown does, kills its whole group.
        let mut guard = GroupGuard(Some(group));
        let stopped = tokio::select! {
            exited = tokio::time::timeout_at(self.deadline, exited(group)) => match exited {
                Ok(exited) => Ok(exited?),
                Err(_) => Err(format!(
                    "the seed build timed out after {} minutes in {}; output is in {}",
                    BUILD_TIMEOUT.as_secs() / 60,
                    step.label,
                    self.log.display()
                )),
            },
            _ = self.cancel.wait_for(|cancelled| *cancelled) => {
                Err("seed build cancelled".to_owned())
            }
        };
        // The leader is not reaped yet, so its ID still names this group
        // alone: nothing else can have taken it.
        // SAFETY: killpg takes no pointers; a group without members is ESRCH.
        unsafe { libc::killpg(group, libc::SIGKILL) };
        let status = child.wait().await?;
        group_gone(group).await;
        guard.0 = None;
        if let Err(reason) = stopped {
            bail!(reason);
        }
        Ok(status)
    }
}

/// `step` as its own process group in the seed worktree, with the
/// repository's `[env]` and none of the daemon's Shoal variables or secrets,
/// printing to `stdout` and `stderr`.
fn step_command(
    step: &Step,
    plan: &Plan,
    worktree: &Path,
    stdout: &fs::File,
    stderr: &fs::File,
) -> Result<Command> {
    let (program, args) = step.argv.split_first().context("empty seed command")?;
    let mut command = Command::new(program);
    command.args(args);
    // The daemon's own Shoal variables describe no workspace of the seed's.
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with(crate::env::PREFIX) {
            command.env_remove(name);
        }
    }
    for name in crate::env::inherited_secret_exports() {
        command.env_remove(name);
    }
    if let Some(path) = crate::env::path_without_secret_tools() {
        command.env("PATH", path);
    }
    crate::env::apply_configured(&mut command, &plan.environment);
    command
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .process_group(0)
        .kill_on_drop(true);
    Ok(command)
}

/// Kills a seed build's process group when dropped before the build has
/// confirmed the group is empty. Until then a member or the unreaped leader
/// keeps the group ID, so the signal cannot reach another process.
struct GroupGuard(Option<i32>);

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(group) = self.0 {
            // SAFETY: killpg takes no pointers.
            unsafe { libc::killpg(group, libc::SIGKILL) };
        }
    }
}

/// How long killed members of a seed build's group may take to exit before
/// the wait is logged.
const GROUP_EXIT_WARNING: Duration = Duration::from_secs(30);

/// Wait until no live process remains in `group`, so nothing writes to the
/// seed worktree once this returns; the caller keeps the seed slot meanwhile,
/// so removal waits too. A zombie counts as gone: it runs no code, and in a
/// container without an init process nothing may ever reap it.
async fn group_gone(group: i32) {
    let started = tokio::time::Instant::now();
    let mut warned = false;
    let mut check_failed = false;
    loop {
        match crate::process::identity::group_alive(group as u32).await {
            Ok(false) => return,
            Ok(true) => {
                if !warned && started.elapsed() >= GROUP_EXIT_WARNING {
                    log!("seed build group {group} survives SIGKILL; waiting for it to exit");
                    warned = true;
                }
            }
            // A failed check counts as alive, so nothing writes once this
            // returns.
            Err(error) => {
                if !check_failed {
                    log!(
                        "cannot check seed build group {group}; waiting for it to exit: {error:#}"
                    );
                    check_failed = true;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Wait for the process `pid` to exit without reaping it.
async fn exited(pid: i32) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        loop {
            // SAFETY: `info` is a valid, writable siginfo_t for waitid to fill.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let flags = libc::WEXITED | libc::WNOWAIT;
            if unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, flags) } == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error).context("wait for setup.seed.command");
            }
        }
    })
    .await?
}

mod state;
#[cfg(test)]
mod tests;

pub(crate) use state::Seeds;
