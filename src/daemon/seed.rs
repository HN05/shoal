//! Keep each repository's seed build at its default branch's tip and start
//! new workspaces from it. Seed failures never fail the operation that met
//! them: a workspace without a seed still builds, only from scratch.
use std::{
    collections::BTreeMap, ffi::OsString, fs, io::Write as _, path::Path, process::Stdio,
    sync::Arc, time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use tokio::{process::Command, sync::watch};

use crate::{
    config::Effective,
    daemon::{
        log::log,
        notifications::NotificationKind,
        workspace::{
            Manager,
            capacity::{self, MemoryState},
            identity,
        },
    },
    git::{
        self,
        default_branch::{self, DefaultBranchLookup},
    },
    model::{Repository, Workspace},
    seed::{
        Layout, Snapshot,
        build::{self, Build, Step},
    },
    state::WorkspaceState,
};

/// How often the daemon looks for a default branch that moved.
const REFRESH_INTERVAL: Duration = Duration::from_secs(60);
/// Longest a seed build may run before it is killed and reported.
const BUILD_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);

/// What one repository's seed builds, from its resolved `[seed]` table.
struct Plan {
    branch: String,
    build: Build,
    paths: Vec<String>,
    environment: BTreeMap<String, OsString>,
    caches: BTreeMap<String, crate::config::repo::Cache>,
}

impl Plan {
    /// The seed `settings` describe, or `None` when they configure none.
    async fn new(repo: &Repository, layout: &Layout, settings: &Effective) -> Result<Option<Self>> {
        let presets = settings.presets.as_deref().unwrap_or_default();
        let Some(build) = Build::of(&settings.seed, presets, &layout.worktree()) else {
            return Ok(None);
        };
        let branch = default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await?;
        // Builds see the workspaces' [env], so their outputs stay reusable.
        let identity = Workspace::new_record(
            repo.id.clone(),
            ".seed".into(),
            layout.worktree(),
            branch.clone(),
            WorkspaceState::Ready,
        );
        Ok(Some(Self {
            build,
            paths: build::outputs(&settings.seed, presets),
            environment: crate::env::render_configured_environment(
                &settings.env,
                &identity,
                crate::repository::name(repo),
                &[],
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

impl Manager {
    /// Rebuild, one at a time, each seed whose default branch moved since its
    /// snapshot. A commit whose build failed is not retried until the branch
    /// moves again or the daemon restarts.
    pub async fn refresh_seeds(&self) {
        let repositories = match self.repositories().await {
            Ok(repositories) => repositories,
            Err(error) => return log!("seed refresh: {error:#}"),
        };
        for repo in repositories {
            if let Err(error) = self.refresh_seed(&repo).await {
                let name = crate::repository::name(&repo);
                self.notify(
                    None,
                    NotificationKind::SeedFailed,
                    format!("Seed build for {name} failed: {error:#}"),
                )
                .await;
            }
        }
    }

    async fn refresh_seed(&self, repo: &Repository) -> Result<()> {
        let Some(layout) = Layout::of(repo) else {
            return Ok(());
        };
        let Some(plan) = self.seed_plan(repo, &layout).await? else {
            return self.discard_seed(repo, &layout).await;
        };
        let tip = git::seed::branch_commit(&repo.path, &plan.branch).await?;
        let build = (tip, plan.settings()?);
        let current = Snapshot::open(&layout)?
            .is_some_and(|snapshot| (snapshot.commit, snapshot.settings) == build);
        // Checked first: seeing any other build, a current one included,
        // forgets the failed one.
        let failed = self.repeats_seed_failure(&repo.id, &build);
        if failed || current || !self.seed_headroom(&layout) {
            return Ok(());
        }
        let result = self.build_seed(repo, &layout, &plan, &build).await;
        self.record_seed_result(&repo.id, result.is_err().then_some(build));
        result
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
        let _git = self.lock_repository_git(&repo.id).await;
        self.ensure_repository_available(&repo.id).await?;
        self.remove_seed(repo).await?;
        self.record_seed_result(&repo.id, None);
        Ok(())
    }

    /// Check the seed worktree out at `tip`, build it and freeze the outputs.
    /// Repository removal cancels the build and waits for it to return.
    async fn build_seed(
        &self,
        repo: &Repository,
        layout: &Layout,
        plan: &Plan,
        (tip, settings): &(String, String),
    ) -> Result<()> {
        let slot = self.seed_slot(&repo.id);
        let _build = slot.gate.lock().await;
        if self.repository(&repo.id).await.is_err() {
            return Ok(());
        }
        {
            let _git = self.lock_repository_git(&repo.id).await;
            self.ensure_repository_available(&repo.id).await?;
            check_out_seed(repo, layout, tip).await?;
        }
        // Under the gate, which keeps cache emptying out until the build ends.
        crate::caches::directories(repo, &plan.caches)?;
        log!("seed build for {} at {tip}", repo.path.display());
        run_build(
            plan,
            &layout.worktree(),
            &layout.log(),
            slot.cancel.subscribe(),
        )
        .await?;
        // Creation clones under the same gate, so it never sees a partial swap.
        let _git = self.lock_repository_git(&repo.id).await;
        self.ensure_repository_available(&repo.id).await?;
        let worktree = layout.worktree();
        ensure!(
            git::seed::head(&worktree).await? == *tip,
            "the seed worktree moved during its build"
        );
        let files = git::seed::unmodified_files(&worktree).await?;
        Snapshot::freeze(
            layout,
            tip.to_owned(),
            settings.to_owned(),
            &plan.paths,
            files,
        )
        .await
    }

    /// Start a new worktree from its repository's seed snapshot when the
    /// worktree's own settings describe the build that made it. The caller
    /// holds the repository's Git gate.
    pub(super) async fn apply_seed(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        settings: &Effective,
    ) {
        let Some(layout) = Layout::of(repo) else {
            return;
        };
        let started = async {
            let Some(plan) = Plan::new(repo, &layout, settings).await? else {
                return Ok(());
            };
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
            start_from_snapshot(&layout, &snapshot, &workspace.path, &plan.paths).await
        };
        if let Err(error) = started.await {
            self.notify(
                Some(&workspace.name),
                NotificationKind::SeedFailed,
                format!("Workspace starts without the seed build: {error:#}"),
            )
            .await;
        }
    }

    /// Stop the repository's seed build, if one runs, and keep new ones out
    /// until the returned guard drops. Take it before the Git gate, which a
    /// build takes while holding its own.
    pub(super) async fn stop_seed_build(&self, repository_id: &str) -> SeedStop {
        let slot = self.seed_slot(repository_id);
        slot.cancel.send_replace(true);
        let guard = slot.gate.clone().lock_owned().await;
        SeedStop {
            _guard: guard,
            slot,
        }
    }

    /// Keep the repository's seed builds from starting until the returned
    /// guard drops, or `None` while one runs.
    pub(super) fn pause_idle_seed(
        &self,
        repository_id: &str,
    ) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        self.seed_slot(repository_id)
            .gate
            .clone()
            .try_lock_owned()
            .ok()
    }

    fn seed_slot(&self, repository_id: &str) -> Arc<SeedSlot> {
        self.seed_builds
            .lock()
            .expect("seed builds lock")
            .entry(repository_id.to_owned())
            .or_default()
            .clone()
    }

    /// Delete the repository's seed. The caller holds a [`SeedStop`] and the
    /// repository's Git gate.
    pub(super) async fn remove_seed(&self, repo: &Repository) -> Result<()> {
        let Some(layout) = Layout::of(repo) else {
            return Ok(());
        };
        let worktree = layout.worktree();
        if worktree.symlink_metadata().is_ok() {
            ensure_seed_worktree(repo, &layout).await?;
            git::seed::remove_worktree(&repo.path, &worktree).await?;
        }
        tokio::task::spawn_blocking(move || layout.remove_owned()).await?
    }

    /// Whether memory and disk space are clear of their overload warning
    /// thresholds, so a seed build adds no load to a strained host. A failed
    /// reading defers the build.
    fn seed_headroom(&self, layout: &Layout) -> bool {
        let overload = &self.config().overload;
        let sample = capacity::sample(&[self.paths.state.as_path(), layout.root()], overload, None);
        let disk_floor = if overload.warning.enabled {
            overload.warning.disk_free_bytes()
        } else {
            0
        };
        sample.memory == MemoryState::Healthy
            && sample.disk_free.is_some_and(|free| free >= disk_floor)
    }

    /// Whether `build` is the one that last failed; any other build makes
    /// the failed one worth retrying when it comes back.
    fn repeats_seed_failure(&self, repository_id: &str, build: &(String, String)) -> bool {
        let mut failures = self.seed_failures.lock().expect("seed failures lock");
        if failures.get(repository_id) == Some(build) {
            return true;
        }
        failures.remove(repository_id);
        false
    }

    /// Remember the build that failed, or forget an earlier failure.
    fn record_seed_result(&self, repository_id: &str, failed: Option<(String, String)>) {
        let mut failures = self.seed_failures.lock().expect("seed failures lock");
        match failed {
            Some(build) => failures.insert(repository_id.to_owned(), build),
            None => failures.remove(repository_id),
        };
    }
}

async fn check_out_seed(repo: &Repository, layout: &Layout, tip: &str) -> Result<()> {
    layout.claim()?;
    let worktree = layout.worktree();
    if worktree.symlink_metadata().is_ok() {
        ensure_seed_worktree(repo, layout).await?;
        return git::seed::check_out(&worktree, tip).await;
    }
    git::seed::add_worktree(&repo.path, &worktree, tip, &layout.lock_reason()).await?;
    identity::mark_owner(&git::git_dir(&worktree).await?, &layout.worktree_owner())
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
    let reached = git::worktree_root(&worktree)
        .await
        .is_ok_and(|root| fs::canonicalize(root.trim()).ok() == fs::canonicalize(&worktree).ok());
    if !reached || git::common_dir(&worktree).await? != git::common_dir(&repo.path).await? {
        return Ok(false);
    }
    let owner = identity::marked_owner(&git::git_dir(&worktree).await?)?;
    Ok(owner == Some(layout.worktree_owner()))
}

async fn start_from_snapshot(
    layout: &Layout,
    snapshot: &Snapshot,
    worktree: &Path,
    paths: &[String],
) -> Result<()> {
    // Restored times are only safe beside the outputs built after them: on
    // outputs the worktree already had, they would hide changed sources.
    if snapshot
        .clone_outputs(layout, worktree, paths)
        .await?
        .is_empty()
    {
        return Ok(());
    }
    let changed = git::seed::changed_between(worktree, &snapshot.commit, "HEAD").await?;
    snapshot.restore_times(worktree, &changed)
}

/// One repository's seed build: held while a build runs, and set to cancel it.
pub(crate) struct SeedSlot {
    gate: Arc<tokio::sync::Mutex<()>>,
    cancel: watch::Sender<bool>,
}

impl Default for SeedSlot {
    fn default() -> Self {
        Self {
            gate: Default::default(),
            cancel: watch::channel(false).0,
        }
    }
}

/// Keeps seed builds out of a repository; dropping it allows them again.
pub(super) struct SeedStop {
    _guard: tokio::sync::OwnedMutexGuard<()>,
    slot: Arc<SeedSlot>,
}

impl Drop for SeedStop {
    fn drop(&mut self) {
        self.slot.cancel.send_replace(false);
    }
}

/// Run the seed build's commands in its worktree, in order, writing their
/// output to `log`, until one fails, the build times out or `cancel` is set.
async fn run_build(
    plan: &Plan,
    worktree: &Path,
    log: &Path,
    mut cancel: watch::Receiver<bool>,
) -> Result<()> {
    let mut output = fs::File::create(log).with_context(|| format!("create {}", log.display()))?;
    let deadline = tokio::time::Instant::now() + BUILD_TIMEOUT;
    for step in plan.build.steps() {
        writeln!(output, "$ {}", step.label)?;
        run_step(&step, plan, worktree, &output, log, deadline, &mut cancel).await?;
    }
    Ok(())
}

/// Run one command of the seed build until it exits, the build's deadline
/// passes or `cancel` is set. Whatever it leaves in its process group is
/// killed before this returns.
async fn run_step(
    step: &Step,
    plan: &Plan,
    worktree: &Path,
    output: &fs::File,
    log: &Path,
    deadline: tokio::time::Instant,
    cancel: &mut watch::Receiver<bool>,
) -> Result<()> {
    let mut child = step_command(step, plan, worktree, output)?
        .spawn()
        .with_context(|| format!("launch {}", step.label))?;
    let group = child.id().context("seed build process ID unavailable")? as i32;
    // Dropping the build, as daemon shutdown does, kills its whole group.
    let mut guard = GroupGuard(Some(group));
    let stopped = tokio::select! {
        exited = tokio::time::timeout_at(deadline, exited(group)) => match exited {
            Ok(exited) => Ok(exited?),
            Err(_) => Err(format!(
                "the seed build timed out after {} minutes in {}; output is in {}",
                BUILD_TIMEOUT.as_secs() / 60,
                step.label,
                log.display()
            )),
        },
        _ = cancel.wait_for(|cancelled| *cancelled) => Err("seed build cancelled".to_owned()),
    };
    // The leader is not reaped yet, so its ID still names this group alone:
    // nothing else can have taken it.
    // SAFETY: killpg takes no pointers; a group without members is ESRCH.
    unsafe { libc::killpg(group, libc::SIGKILL) };
    let status = child.wait().await?;
    group_gone(group).await;
    guard.0 = None;
    if let Err(reason) = stopped {
        bail!(reason);
    }
    ensure!(
        status.success(),
        "{} exited with {}; output is in {}",
        step.label,
        crate::execution::exit_code(status),
        log.display()
    );
    Ok(())
}

/// `step` as its own process group in the seed worktree, with the
/// repository's `[env]` and none of the daemon's Shoal variables or secrets.
fn step_command(step: &Step, plan: &Plan, worktree: &Path, output: &fs::File) -> Result<Command> {
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
        .stdout(output.try_clone()?)
        .stderr(output.try_clone()?)
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
    while group_alive(group) {
        if !warned && started.elapsed() >= GROUP_EXIT_WARNING {
            log!("seed build group {group} survives SIGKILL; waiting for it to exit");
            warned = true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn group_alive(group: i32) -> bool {
    // SAFETY: signal 0 only checks whether the group has members.
    if unsafe { libc::killpg(group, 0) } != 0 {
        return false;
    }
    match fs::read_dir("/proc") {
        // Linux lists each member's state and group after its command name.
        Ok(entries) => entries.flatten().any(|entry| {
            fs::read_to_string(entry.path().join("stat"))
                .is_ok_and(|stat| live_member(&stat, group))
        }),
        // Elsewhere launchd reaps orphans, so members disappear.
        Err(_) => true,
    }
}

/// Whether a `/proc/<pid>/stat` line is a live member of `group`: after the
/// command name come the state, the parent PID and the process group.
fn live_member(stat: &str, group: i32) -> bool {
    let rest = stat.rsplit_once(") ").map_or("", |(_, rest)| rest);
    let mut fields = rest.split(' ');
    let state = fields.next();
    let group_id = fields.nth(1);
    state != Some("Z") && group_id == Some(group.to_string().as_str())
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
                return Err(error).context("wait for seed.cmd");
            }
        }
    })
    .await?
}

#[cfg(test)]
mod tests;
