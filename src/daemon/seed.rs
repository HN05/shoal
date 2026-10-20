//! Keep each repository's seed build at its default branch's tip and start
//! new workspaces from it. Seed failures never fail the operation that met
//! them: a workspace without a seed still builds, only from scratch.
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
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
    protocol::ConfigTarget,
    seed::{Layout, Snapshot},
    state::WorkspaceState,
};

/// How often the daemon looks for a default branch that moved.
const REFRESH_INTERVAL: Duration = Duration::from_secs(60);
/// Longest a seed build may run before it is killed and reported.
const BUILD_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);

/// What one repository's seed builds, from its resolved `[seed]` table.
struct Plan {
    branch: String,
    command: PathBuf,
    paths: Vec<String>,
    environment: BTreeMap<String, OsString>,
}

impl Plan {
    /// The seed `settings` describe, or `None` when they configure none.
    async fn new(repo: &Repository, layout: &Layout, settings: &Effective) -> Result<Option<Self>> {
        let (Some(command), false) = (&settings.seed.cmd, settings.seed.paths.is_empty()) else {
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
            command: layout.worktree().join(command),
            paths: settings.seed.paths.clone(),
            environment: crate::env::render_configured_environment(
                &settings.env,
                &identity,
                crate::forge::repository::name(repo),
                &[],
                &crate::caches::directories(repo, &settings.caches)?,
            ),
            branch,
        }))
    }

    /// What a build depends on besides the commit, so a snapshot built under
    /// other settings is rebuilt.
    fn settings(&self) -> Result<String> {
        Ok(serde_json::to_string(&(
            &self.command,
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
                let name = crate::forge::repository::name(&repo);
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
        let layers = self
            .config_layers_for(ConfigTarget::Repository(repo.id.clone().into()))
            .await?;
        let settings = self.config().resolve(&layers)?;
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

/// Run the seed command in its worktree, writing its output to `log`, until
/// it exits, times out or `cancel` is set. Whatever it leaves in its process
/// group is killed before this returns.
async fn run_build(
    plan: &Plan,
    worktree: &Path,
    log: &Path,
    mut cancel: watch::Receiver<bool>,
) -> Result<()> {
    let output = fs::File::create(log).with_context(|| format!("create {}", log.display()))?;
    let mut command = Command::new(&plan.command);
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
    let mut child = command
        .current_dir(worktree)
        .envs(&plan.environment)
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output)
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("launch seed.cmd {}", plan.command.display()))?;
    let group = child.id().context("seed build process ID unavailable")? as i32;
    // Dropping the build, as daemon shutdown does, kills its whole group.
    let mut guard = GroupGuard(Some(group));
    let stopped = tokio::select! {
        exited = tokio::time::timeout(BUILD_TIMEOUT, exited(group)) => match exited {
            Ok(exited) => Ok(exited?),
            Err(_) => Err(format!(
                "seed.cmd timed out after {} minutes; output is in {}",
                BUILD_TIMEOUT.as_secs() / 60,
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
        "seed.cmd exited with {}; output is in {}",
        crate::execution::exit_code(status),
        log.display()
    );
    Ok(())
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
mod tests {
    use super::*;
    use crate::test_support::{commit, git, manager, repository};
    use std::os::unix::fs::PermissionsExt as _;

    fn modified(path: &Path) -> std::time::SystemTime {
        fs::metadata(path).unwrap().modified().unwrap()
    }

    /// Builds wait under host pressure; tests choose the pressure they see.
    async fn configure(manager: &Manager, text: &str) {
        let config = crate::config::Config::path(&manager.paths);
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, text).unwrap();
        manager.reload_config().await.unwrap();
    }

    const NO_PRESSURE: &str =
        "[overload.memory]\nenabled = false\n[overload.disk]\nenabled = false\n";

    #[tokio::test]
    async fn new_workspaces_start_from_the_default_branch_build() {
        let (temp, manager) = manager().await;
        configure(&manager, NO_PRESSURE).await;
        let checkout = repository(temp.path(), "repo");
        // The build edits a tracked file, which must not lend its time.
        let script = checkout.join("seed.sh");
        fs::write(
            &script,
            "#!/bin/sh\nmkdir -p out\ncat other > out/built\necho edit >> tracked\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            checkout.join(".shoal.toml"),
            "[seed]\ncmd = 'seed.sh'\npaths = ['out']\n",
        )
        .unwrap();
        fs::write(checkout.join("other"), "first\n").unwrap();
        for file in ["seed.sh", ".shoal.toml", "other"] {
            commit(&checkout, file);
        }
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        manager.refresh_seeds().await;
        let layout = Layout::of(&repo).unwrap();
        let Some(snapshot) = Snapshot::open(&layout).unwrap() else {
            // Without copy-on-write cloning the seed is reported, never copied.
            let notifications = manager.notifications(true, 10).await.unwrap();
            assert!(
                notifications
                    .iter()
                    .any(|notification| notification.kind == NotificationKind::SeedFailed),
                "{notifications:?}"
            );
            return;
        };
        let built = git(&checkout, &["rev-parse", "main"]);
        assert_eq!(snapshot.commit, built.trim());

        fs::write(checkout.join("other"), "second\n").unwrap();
        commit(&checkout, "other");
        let workspace = manager
            .create_workspace(&repo.id, "feature".into(), None, None, None)
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(workspace.path.join("out/built")).unwrap(),
            "first\n"
        );
        let seed = layout.worktree();
        assert_eq!(
            modified(&workspace.path.join("seed.sh")),
            modified(&seed.join("seed.sh"))
        );
        for changed in ["other", "tracked"] {
            assert_ne!(
                modified(&workspace.path.join(changed)),
                modified(&seed.join(changed)),
                "{changed}"
            );
        }

        // The moved default branch rebuilds the seed, restoring the edited file.
        manager.refresh_seeds().await;
        let snapshot = Snapshot::open(&layout).unwrap().unwrap();
        assert_eq!(
            snapshot.commit,
            git(&checkout, &["rev-parse", "main"]).trim()
        );
        assert_eq!(
            fs::read_to_string(seed.join("tracked")).unwrap(),
            "initial\nedit\n"
        );
        let checks = manager.diagnose().await.unwrap();
        assert!(
            !checks
                .iter()
                .any(|check| check.message.contains("not tracked by Shoal")),
            "{checks:?}"
        );

        manager.remove_repository(&repo.id).await.unwrap();
        assert!(!layout.root().exists());
    }

    #[tokio::test]
    async fn a_failed_build_is_reported_once_per_commit() {
        let (temp, manager) = manager().await;
        configure(&manager, NO_PRESSURE).await;
        let checkout = repository(temp.path(), "repo");
        let script = checkout.join("seed.sh");
        fs::write(&script, "#!/bin/sh\necho broken\nexit 3\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            checkout.join(".shoal.toml"),
            "[seed]\ncmd = 'seed.sh'\npaths = ['out']\n",
        )
        .unwrap();
        for file in ["seed.sh", ".shoal.toml"] {
            commit(&checkout, file);
        }
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let layout = Layout::of(&repo).unwrap();
        // A warning threshold above any free space defers the build.
        configure(
            &manager,
            "[overload.memory]\nenabled = false\n[overload.warning]\ndisk_free_gib = 1000000\n",
        )
        .await;
        manager.refresh_seeds().await;
        assert!(!layout.log().exists(), "the build waits for headroom");
        configure(&manager, NO_PRESSURE).await;
        manager.refresh_seeds().await;
        assert_eq!(fs::read_to_string(layout.log()).unwrap(), "broken\n");
        fs::remove_file(layout.log()).unwrap();
        manager.refresh_seeds().await;
        assert!(!layout.log().exists(), "the failed commit is not rebuilt");
        // Other settings at the same commit are a new build.
        fs::write(
            checkout.join(".shoal.toml"),
            "[seed]\ncmd = 'seed.sh'\npaths = ['out']\n[env]\nMODE = 'seed'\n",
        )
        .unwrap();
        manager.refresh_seeds().await;
        assert!(layout.log().exists(), "changed settings rebuild");
        // Returning to the failed settings retries them.
        fs::remove_file(layout.log()).unwrap();
        fs::write(
            checkout.join(".shoal.toml"),
            "[seed]\ncmd = 'seed.sh'\npaths = ['out']\n",
        )
        .unwrap();
        manager.refresh_seeds().await;
        assert!(layout.log().exists(), "a revisited failure is retried");
        let notifications = manager.notifications(true, 10).await.unwrap();
        let failure = notifications
            .iter()
            .find(|notification| notification.kind == NotificationKind::SeedFailed)
            .unwrap();
        assert!(failure.message.contains("exited with 3"), "{failure:?}");
        assert!(Snapshot::open(&layout).unwrap().is_none());
    }

    /// A repository whose checkout configures `script` as its seed.
    async fn seeded(manager: &Manager, root: &Path, script: &str) -> (PathBuf, Repository) {
        let checkout = repository(root, "repo");
        let path = checkout.join("seed.sh");
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            checkout.join(".shoal.toml"),
            "[seed]\ncmd = 'seed.sh'\npaths = ['out']\n",
        )
        .unwrap();
        for file in ["seed.sh", ".shoal.toml"] {
            commit(&checkout, file);
        }
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        (checkout, repo)
    }

    #[tokio::test]
    async fn repository_removal_cancels_a_running_build() {
        let (temp, manager) = manager().await;
        configure(&manager, NO_PRESSURE).await;
        let (_, repo) = seeded(
            &manager,
            temp.path(),
            "#!/bin/sh\ntouch started\nexec sleep 600\n",
        )
        .await;
        let layout = Layout::of(&repo).unwrap();
        let refresh = tokio::spawn({
            let manager = manager.clone();
            async move { manager.refresh_seeds().await }
        });
        // The command starts only while its build holds the slot.
        let started = layout.worktree().join("started");
        tokio::time::timeout(Duration::from_secs(60), async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the build starts");
        manager.remove_repository(&repo.id).await.unwrap();
        assert!(!layout.root().exists());
        tokio::time::timeout(Duration::from_secs(60), refresh)
            .await
            .expect("the cancelled build returns")
            .unwrap();
    }

    #[tokio::test]
    async fn a_removed_seed_is_neither_applied_nor_kept() {
        let (temp, manager) = manager().await;
        configure(&manager, NO_PRESSURE).await;
        let (checkout, repo) = seeded(
            &manager,
            temp.path(),
            "#!/bin/sh\nmkdir -p out\ntouch out/built\n",
        )
        .await;
        manager.refresh_seeds().await;
        let layout = Layout::of(&repo).unwrap();
        if Snapshot::open(&layout).unwrap().is_none() {
            return; // No copy-on-write cloning here; covered above.
        }
        // Outputs built under other settings are not applied.
        fs::write(
            checkout.join(".shoal.toml"),
            "[seed]\ncmd = 'seed.sh'\npaths = ['out']\n[env]\nMODE = 'other'\n",
        )
        .unwrap();
        commit(&checkout, ".shoal.toml");
        let changed = manager
            .create_workspace(&repo.id, "changed".into(), None, None, None)
            .await
            .unwrap();
        assert!(!changed.path.join("out").exists());
        git(&checkout, &["rm", "-q", ".shoal.toml"]);
        git(&checkout, &["commit", "-q", "-m", "Drop the seed"]);
        let workspace = manager
            .create_workspace(&repo.id, "feature".into(), None, None, None)
            .await
            .unwrap();
        assert!(!workspace.path.join("out").exists());
        manager.refresh_seeds().await;
        assert!(!layout.root().exists());
    }

    #[tokio::test]
    async fn removal_keeps_a_seed_worktree_shoal_did_not_create() {
        let (temp, manager) = manager().await;
        let (_, repo) = seeded(&manager, temp.path(), "#!/bin/sh\n").await;
        let layout = Layout::of(&repo).unwrap();
        fs::create_dir_all(layout.worktree()).unwrap();
        fs::write(layout.worktree().join("keep"), "user data").unwrap();
        let checks = manager.diagnose().await.unwrap();
        assert!(
            checks.iter().any(|check| check
                .message
                .contains("Seed path is not Shoal's seed worktree")),
            "{checks:?}"
        );
        let error = manager.remove_repository(&repo.id).await.unwrap_err();
        assert!(
            format!("{error:#}").contains("not the seed worktree"),
            "{error:#}"
        );
        assert!(layout.worktree().join("keep").exists());
    }

    #[tokio::test]
    async fn processes_a_build_leaves_behind_are_stopped() {
        let (temp, manager) = manager().await;
        configure(&manager, NO_PRESSURE).await;
        let (_, repo) = seeded(
            &manager,
            temp.path(),
            "#!/bin/sh\nsleep 600 &\necho $! > ../leftover\nmkdir -p out\n",
        )
        .await;
        manager.refresh_seeds().await;
        let layout = Layout::of(&repo).unwrap();
        let pid: i32 = fs::read_to_string(layout.root().join("leftover"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // A killed orphan stays a zombie where nothing reaps it, as in a
        // container without an init process; that still counts as stopped.
        let running = || {
            let zombie = fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
                stat.rsplit_once(") ")
                    .is_some_and(|(_, rest)| rest.starts_with('Z'))
            });
            // SAFETY: signal 0 only checks that the process exists.
            let exists = unsafe { libc::kill(pid, 0) } == 0;
            exists && !zombie
        };
        tokio::time::timeout(Duration::from_secs(60), async {
            while running() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the leftover process is stopped");
    }

    #[tokio::test]
    async fn removal_keeps_a_symlinked_seed_worktree() {
        let (temp, manager) = manager().await;
        configure(&manager, NO_PRESSURE).await;
        let (_, repo) = seeded(&manager, temp.path(), "#!/bin/sh\nmkdir -p out\n").await;
        manager.refresh_seeds().await;
        let layout = Layout::of(&repo).unwrap();
        if !layout.worktree().exists() {
            return; // The seed was not created on this filesystem.
        }
        // The real worktree moves away and a symlink takes its place.
        let moved = temp.path().join("moved");
        fs::rename(layout.worktree(), &moved).unwrap();
        git(&repo.path, &["worktree", "repair", moved.to_str().unwrap()]);
        std::os::unix::fs::symlink(&moved, layout.worktree()).unwrap();
        assert!(manager.remove_repository(&repo.id).await.is_err());
        assert!(moved.join("seed.sh").exists());
    }

    #[test]
    fn group_membership_reads_the_group_field() {
        // pid 40, command "a) b", state S, parent 7, group 30.
        let stat = "40 (a) b) S 7 30 30 0 -1";
        assert!(live_member(stat, 30));
        assert!(!live_member(stat, 7), "the parent PID is not the group");
        assert!(
            !live_member("40 (a) Z 7 30 30 0 -1", 30),
            "zombies are gone"
        );
    }
}
