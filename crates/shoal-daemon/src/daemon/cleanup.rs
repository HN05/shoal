//! Space cleanup: automatic removal of idle spaces, clean and fully pushed
//! where they have a worktree, of merged ones, and of worktrees whose
//! directory was deleted outside Shoal; the user can remove the idle
//! candidates on request. A space without a worktree meets every condition
//! on one, and goes once its agent stopped and nothing keeps it.
use crate::client::repository_ids::RepositoryId;
use crate::client::space_ids::SpaceId;
use anyhow::{Result, ensure};
use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    os::unix::fs::MetadataExt,
    path::Path,
    sync::Arc,
    time::Duration,
};
use tokio::time::{Instant, sleep};

pub(crate) use crate::client::cleanup::{
    CacheCleanup, Kept, ManualCleanup, Pick, Retained, Selection,
};
use crate::client::space_names::SpaceName;
use crate::{
    daemon::{
        caches::Emptying,
        events::EventCause,
        file_access, log,
        spaces::removal_notices::Cleanup,
        workspace::{Manager, not_ready},
    },
    fsutil::off_runtime,
    model::{CleanupReason, PresentSpace, Workspace},
    process::DirectoryUsers,
    state::WorkspaceState,
    time::UnixTime,
};

mod failures;
mod grace;
mod health;
mod kept_branches;
mod merged;
mod runs;

use failures::LoggedFailures;
pub(crate) use failures::{SpaceFailures, SweepFailures};
pub(crate) use health::CleanupHealth;
pub(crate) use runs::Removed;

const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// The rules that made a space a cleanup candidate, rechecked during removal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// Clean, unused, every commit pushed or merged, and no assignment state.
    Idle,
    /// Merged into the default branch and unused, whatever its assignment.
    Merged,
}

impl Rule {
    /// Whether the worktree's state still satisfies the rule.
    pub fn allows(self, check: &crate::removal::RemovalCheck) -> bool {
        match self {
            Rule::Idle => check.safe(),
            // Unsaved files go into the checkpoint. A base stays until PR
            // cleanup has moved the workspaces stacked on it.
            Rule::Merged => {
                check.unused()
                    && check.matches_default_branch
                    && check.workspace.stacked_workspaces.is_empty()
            }
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Rule::Idle => "idle, clean and fully pushed",
            Rule::Merged => "unused and merged",
        }
    }
}

/// What automatic cleanup must see unchanged before removing a space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub rule: Rule,
    /// What the space records, with its worktree's HEAD, cheap enough to
    /// compare on every pass.
    pub signals: u64,
    /// The metadata of every file in its worktree, which takes a full walk,
    /// or of none.
    pub tree: u64,
}

/// When a space was last seen unchanged in a removable state.
struct Idle {
    snapshot: Snapshot,
    since: Instant,
}

#[derive(Default)]
pub struct Timers {
    idle: HashMap<SpaceId, Idle>,
    merged: merged::State,
    kept_branches: kept_branches::State,
}

impl Timers {
    /// Whether the timer has run for `delay` with unchanged `signals`; a
    /// change, or no timer, needs a full snapshot to start from.
    fn expired(&self, id: &SpaceId, signals: u64, now: Instant, delay: Duration) -> Expiry {
        match self.idle.get(id) {
            Some(idle) if idle.snapshot.signals != signals => Expiry::Restart,
            Some(idle) if now.duration_since(idle.since) < delay => Expiry::Running,
            Some(idle) => Expiry::Expired(idle.snapshot),
            None => Expiry::Restart,
        }
    }

    fn start(&mut self, id: &SpaceId, snapshot: Snapshot, now: Instant) {
        self.idle.insert(
            id.clone(),
            Idle {
                snapshot,
                since: now,
            },
        );
    }
}

enum Expiry {
    Running,
    Restart,
    Expired(Snapshot),
}

/// What a cleanup candidate records: its activity counter and when its
/// agent last stopped, with its worktree's HEAD where it has one.
pub fn signals(activity: u64, stopped_at: Option<UnixTime>, head: Option<&str>) -> u64 {
    let mut hash = DefaultHasher::new();
    activity.hash(&mut hash);
    stopped_at.hash(&mut hash);
    head.hash(&mut hash);
    hash.finish()
}

/// What one walk of a worktree's files sees.
#[derive(Clone, Copy, Debug)]
pub struct Tree {
    /// The metadata of every file. Change times only move forward and cannot
    /// be set, unlike modification times, so two equal fingerprints mean no
    /// file changed between them.
    pub fingerprint: u64,
    /// The newest change or modification time of any file.
    pub changed: UnixTime,
}

impl Tree {
    /// What a space without a worktree has: no files.
    pub const EMPTY: Self = Self {
        fingerprint: 0,
        changed: UnixTime::EPOCH,
    };
}

/// The later of a file's change and modification times.
pub fn change_time(metadata: &fs::Metadata) -> UnixTime {
    UnixTime::from_seconds(metadata.ctime().max(metadata.mtime()))
}

/// Metadata only, including ignored files; never follow symlinks out of the tree.
/// Git administrative storage is shared and inspected separately through HEAD.
pub fn walk(root: &Path) -> Result<Tree> {
    fn visit(path: &Path, hash: &mut DefaultHasher, changed: &mut UnixTime) -> Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        path.hash(hash);
        metadata.len().hash(hash);
        metadata.modified()?.hash(hash);
        (metadata.ctime(), metadata.ctime_nsec()).hash(hash);
        *changed = (*changed).max(change_time(&metadata));
        if metadata.is_dir() {
            let mut children = fs::read_dir(path)?
                .map(|entry| entry.map(|e| e.path()))
                .collect::<std::io::Result<Vec<_>>>()?;
            children.sort();
            for child in children {
                if child.file_name().is_some_and(|name| name == ".git") {
                    continue;
                }
                visit(&child, hash, changed)?;
            }
        }
        Ok(())
    }
    let mut hash = DefaultHasher::new();
    let mut changed = UnixTime::EPOCH;
    visit(root, &mut hash, &mut changed)?;
    Ok(Tree {
        fingerprint: hash.finish(),
        changed,
    })
}

/// One pass of every automatic cleanup. A failed step does not skip the
/// steps after it; the error names each step that failed, in the workspace
/// it failed in where it concerns one.
pub async fn sweep(manager: &Manager, timers: &mut Timers) -> Result<(), SweepFailures> {
    manager.cleanup_orphan_sessions().await;
    let mut failed = SweepFailures::default();
    let mut step = |name: &str, result: Result<()>| failed.step(name, result);
    step(
        "ownership recheck",
        manager.recheck_startup_failures().await,
    );
    step("grace periods", manager.begin_grace_pass().await);
    step("branch observation", observe_branches(manager).await);
    step("completion cleanup", manager.sweep_completed().await);
    step("PR cleanup", manager.sweep_prs().await);
    // Every step that asks which processes use a worktree reads one listing.
    let users = DirectoryUsers::default();
    step("idle cleanup", sweep_idle(manager, timers, &users).await);
    step("idle agents", manager.stop_idle_agents().await);
    step(
        "merged cleanup",
        merged::sweep(manager, &mut timers.merged, &users).await,
    );
    step(
        "kept branches",
        kept_branches::sweep(manager, &mut timers.kept_branches).await,
    );
    step("checkpoint expiry", manager.expire_checkpoints().await);
    step("result expiry", manager.expire_results().await);
    step("event expiry", manager.expire_events().await);
    step(
        "earlier attached files",
        manager.record_earlier_files().await,
    );
    step("contributions", manager.sweep_contributions().await);
    step("proposal files", manager.sweep_proposal_files().await);
    step("temporary directories", manager.sweep_tmp_dirs().await);
    step("release copies", manager.sweep_releases().await);
    step("leftovers", manager.find_leftovers().await);
    step("grace periods", manager.end_unclaimed_grace().await);
    failed.into_result()
}

/// Observe the branch of each ready worktree. A failure is the pass's in
/// the workspace it concerns, so the log has it once while it repeats,
/// except in a directory whose file access stalled, which is reported as
/// such and skipped.
async fn observe_branches(manager: &Manager) -> Result<()> {
    let mut failed = SpaceFailures::default();
    for workspace in manager.list_workspaces().await? {
        if workspace.state != WorkspaceState::Ready || !is_dir(&workspace.path).await {
            continue;
        }
        let observed = manager.observe_workspace_branch(&workspace);
        if let Err(error) = manager.check_access(&workspace.path, observed).await
            && !manager.sweep_skips(&workspace.path).await
        {
            failed.push(&workspace.id, &workspace.name, &error);
        }
    }
    failed.into_result()
}

/// Whether `path` is a directory, read off the runtime's threads and within
/// a check's time limit, since stalled file access can hold the read. The daemon's
/// own read says nothing about the processes it starts there, so it records
/// no stall.
async fn is_dir(path: &std::path::Path) -> bool {
    matches!(
        tokio::time::timeout(file_access::LIMIT, off_runtime::is_dir(path)).await,
        Ok(Ok(true))
    )
}

/// Each space's idle delay comes from its own layered config, a chat's from
/// its repository's; a disabled worktree gets only deleted-directory
/// cleanup. A failure in one space does not skip the others.
async fn sweep_idle(manager: &Manager, timers: &mut Timers, users: &DirectoryUsers) -> Result<()> {
    let spaces = manager.present_spaces().await?;
    timers
        .idle
        .retain(|id, _| spaces.iter().any(|space| space.id() == id));
    let mut failed = SpaceFailures::default();
    for space in spaces {
        if let Err(error) = sweep_space(manager, timers, &space, users).await {
            failed.push(space.id(), space.name(), &error);
        }
    }
    failed.into_result()
}

/// Remove `space` once idle cleanup has seen it unchanged for its idle
/// delay and its grace period ended, or forget its deleted worktree.
async fn sweep_space(
    manager: &Manager,
    timers: &mut Timers,
    space: &PresentSpace,
    users: &DirectoryUsers,
) -> Result<()> {
    if let Some(workspace) = space.worktree() {
        // Its idle timer stays for the pass after a check there finishes.
        if manager.sweep_skips(&workspace.path).await {
            return Ok(());
        }
        if deleted(workspace).await? {
            timers.idle.remove(&workspace.id);
            remove_deleted(manager, workspace).await;
            return Ok(());
        }
    }
    let observed = observe(manager, space, users).await;
    let Ok(Some((delay, signals))) = observed else {
        timers.idle.remove(space.id());
        return observed.map(drop);
    };
    // Each pass compares only the signals; a worktree's Git state is
    // inspected and its files walked when the timer starts and again when
    // it expires.
    let now = Instant::now();
    let started = match timers.expired(space.id(), signals, now, delay) {
        Expiry::Running => return Ok(()),
        Expiry::Restart => None,
        Expiry::Expired(snapshot) => Some(snapshot),
    };
    let snapshot = candidate(manager, space, users).await;
    let Ok(Some(snapshot)) = snapshot else {
        timers.idle.remove(space.id());
        return snapshot.map(drop);
    };
    if started != Some(snapshot) {
        timers.start(space.id(), snapshot, now);
        return Ok(());
    }
    let removal = (manager.remove_unused(space, snapshot, EventCause::Idle)).await;
    if matches!(removal, Ok(false)) {
        return Ok(());
    }
    timers.idle.remove(space.id());
    (manager.announce_removal(space, Cleanup::Idle, &removal)).await
}

/// Whether the worktree of `workspace` was deleted: only one that existed
/// (identity recorded) can have been, and a failed creation keeps its
/// error until removed explicitly.
async fn deleted(workspace: &Workspace) -> Result<bool> {
    Ok(matches!(
        workspace.state,
        WorkspaceState::Ready | WorkspaceState::Failed
    ) && workspace.git_dir.is_some()
        && !off_runtime::try_exists(&workspace.path).await?)
}

/// The idle delay of `space`, or `None` when idle cleanup is disabled for
/// it or its worktree is not ready.
async fn idle_delay(manager: &Manager, space: &PresentSpace) -> Result<Option<Duration>> {
    if space.worktree().and_then(not_ready).is_some() {
        return Ok(None);
    }
    let settings = manager.present_space_settings(space).await?;
    Ok(settings.cleanup.auto.delay())
}

/// The idle delay and timer signals of `space` when nothing recorded and no
/// process keeps it; `None` when something does or its idle cleanup is
/// disabled.
async fn observe(
    manager: &Manager,
    space: &PresentSpace,
    users: &DirectoryUsers,
) -> Result<Option<(Duration, u64)>> {
    let Some(delay) = idle_delay(manager, space).await? else {
        return Ok(None);
    };
    let signals = manager.idle_timer_signals(space, users).await?;
    Ok(signals.map(|signals| (delay, signals)))
}

/// The full snapshot of an idle cleanup candidate, with the processes in
/// its worktree read from `users`.
pub(super) async fn candidate(
    manager: &Manager,
    space: &PresentSpace,
    users: &DirectoryUsers,
) -> Result<Option<Snapshot>> {
    if idle_delay(manager, space).await?.is_none() {
        return Ok(None);
    }
    (manager.candidate_snapshot(space, Rule::Idle, &[], users)).await
}

/// Operations on the client type [`ManualCleanup`] that only the `shoal` command uses.
trait ManualCleanupExt {
    /// A dry run lists why it keeps each space, and a request naming spaces
    /// says why it kept those.
    fn keep(&mut self, space: SpaceName, reason: CleanupReason, named: bool);

    /// Record that removing the space `space` failed with `error`.
    fn retain(&mut self, space: SpaceName, error: &anyhow::Error);
}

impl ManualCleanupExt for ManualCleanup {
    fn keep(&mut self, space: SpaceName, reason: CleanupReason, named: bool) {
        if self.dry_run || named {
            self.kept.push(Kept { space, reason });
        }
    }

    fn retain(&mut self, space: SpaceName, error: &anyhow::Error) {
        self.retained.push(Retained {
            space,
            error: format!("{error:#}"),
        });
    }
}

/// Operations on the client type [`Pick`] that only the `shoal` command uses.
trait PickExt {
    /// Whether it covers the repository `name`, `id`.
    fn covers(&self, name: &str, id: &str) -> bool;

    /// Whether it covers the space `id`; names repeat, so it names spaces
    /// by ID alone.
    fn covers_space(&self, id: &SpaceId) -> bool;

    /// Fails for a name none of `items`, as `(name, id)` pairs, has.
    fn ensure_known<'a>(
        &self,
        items: impl Iterator<Item = (&'a str, &'a str)> + Clone,
    ) -> Result<()>;
}

impl PickExt for Pick {
    fn covers(&self, name: &str, id: &str) -> bool {
        match self {
            Self::None => false,
            Self::All => true,
            Self::Only(names) => names.iter().any(|n| n == name || n == id),
        }
    }

    fn covers_space(&self, id: &SpaceId) -> bool {
        self.covers("", id.as_str())
    }

    fn ensure_known<'a>(
        &self,
        items: impl Iterator<Item = (&'a str, &'a str)> + Clone,
    ) -> Result<()> {
        if let Self::Only(names) = self {
            for selector in names {
                ensure!(
                    items
                        .clone()
                        .any(|(name, id)| selector == name || selector == id),
                    "{selector} is not a space or repository this cleanup covers"
                );
            }
        }
        Ok(())
    }
}

/// Remove the selected spaces idle or merged cleanup would remove, now: the
/// user's request replaces their idle periods and applies where they are
/// disabled. Then empty the selected shared caches nothing is using.
pub async fn on_request(
    manager: &Manager,
    dry_run: bool,
    selection: &Selection,
) -> Result<ManualCleanup> {
    let targets = Targets::of(manager, selection).await?;
    if !dry_run {
        manager.cleanup_orphan_sessions().await;
    }
    let mut report = ManualCleanup {
        dry_run,
        ..ManualCleanup::default()
    };
    remove_on_request(manager, &mut report, targets.spaces, &selection.spaces).await?;
    report.caches =
        empty_caches_on_request(manager, dry_run, targets.repositories, &selection.caches).await?;
    Ok(report)
}

/// The spaces and repositories a cleanup request can reach.
struct Targets {
    spaces: Vec<PresentSpace>,
    repositories: Vec<crate::model::Repository>,
}

impl Targets {
    /// Those of the selected repository, or all, once every name the
    /// selection gives is among them.
    async fn of(manager: &Manager, selection: &Selection) -> Result<Self> {
        let repository = match &selection.repository {
            Some(selector) => Some(manager.repository(selector).await?.id),
            None => None,
        };
        let within = |id: &RepositoryId| repository.as_ref().is_none_or(|only| only == id);
        // A chat without a repository is in none of them.
        let space_within = |space: &PresentSpace| match space.repository_id() {
            Some(id) => within(id),
            None => repository.is_none(),
        };
        let targets = Self {
            spaces: (manager.present_spaces().await?.into_iter())
                .filter(|space| space_within(space))
                .collect(),
            repositories: manager
                .repositories()
                .await?
                .into_iter()
                .filter(|repo| within(&repo.id))
                .collect(),
        };
        let spaces = (targets.spaces.iter()).map(|space| ("", space.id().as_str()));
        selection.spaces.ensure_known(spaces)?;
        selection.caches.ensure_known(
            targets
                .repositories
                .iter()
                .map(|repo| (crate::repository::name(repo), repo.id.as_str())),
        )?;
        Ok(targets)
    }
}

async fn empty_caches_on_request(
    manager: &Manager,
    dry_run: bool,
    repositories: Vec<crate::model::Repository>,
    pick: &Pick,
) -> Result<Vec<CacheCleanup>> {
    let mut report = Vec::new();
    for repo in repositories {
        if !pick.covers(crate::repository::name(&repo), repo.id.as_str()) {
            continue;
        }
        let emptying = match manager.empty_caches(&repo, dry_run).await {
            Ok(None) => continue,
            Ok(Some(emptying)) => emptying,
            Err(error) => {
                log!(
                    "manual cleanup kept caches of {}: {error:#}",
                    repo.path.display()
                );
                Emptying::Failed {
                    error: format!("{error:#}"),
                }
            }
        };
        report.push(CacheCleanup {
            repository: crate::repository::name(&repo).to_owned(),
            repository_id: repo.id.clone(),
            emptying,
        });
    }
    Ok(report)
}

/// Remove the selected spaces idle or merged cleanup would remove, now.
async fn remove_on_request(
    manager: &Manager,
    report: &mut ManualCleanup,
    spaces: Vec<PresentSpace>,
    pick: &Pick,
) -> Result<()> {
    let named = matches!(pick, Pick::Only(_));
    for space in spaces {
        let name = space.name().clone();
        if !pick.covers_space(space.id()) {
            continue;
        }
        if let Some(reason) = space.worktree().and_then(not_ready) {
            report.keep(name, reason, named);
            continue;
        }
        let snapshot = match requested_candidacy(manager, &space).await {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(reason)) => {
                report.keep(name, reason, named);
                continue;
            }
            Err(error) => {
                log!("manual cleanup skipped {name}: {error:#}");
                report.retain(name, &error);
                continue;
            }
        };
        if report.dry_run {
            report.removed.push(name.into());
            report.removed_ids.push(space.id().clone());
            continue;
        }
        match (manager.remove_unused(&space, snapshot, EventCause::Manual)).await {
            Ok(_) => {
                log!("manual cleanup removed {name}");
                report.removed.push(name.into());
                report.removed_ids.push(space.id().clone());
            }
            Err(error) => {
                (manager.record_retained(space.id(), EventCause::Manual, &error)).await?;
                log!("manual cleanup retained {name}: {error:#}");
                report.retain(name, &error);
            }
        }
    }
    Ok(())
}

/// The snapshot of an idle or merged cleanup candidate, or why idle cleanup
/// keeps the space. A merged worktree needs a freshly fetched default
/// branch.
async fn requested_candidacy(
    manager: &Manager,
    space: &PresentSpace,
) -> Result<Result<Snapshot, CleanupReason>> {
    let users = DirectoryUsers::default();
    let reason = match manager.idle_candidacy(space, &users).await? {
        Ok(snapshot) => return Ok(Ok(snapshot)),
        Err(reason) => reason,
    };
    if let Some(workspace) = space.worktree()
        && !manager.default_branch_fresh(&workspace.repository_id).await
    {
        return Ok(Err(reason));
    }
    let merged = manager.candidate_snapshot(space, Rule::Merged, &[], &users);
    Ok(merged.await?.ok_or(reason))
}

/// A deleted worktree is forgotten unless it was moved or still has commands
/// Shoal cannot verify; the branch is retained either way.
async fn remove_deleted(manager: &Manager, workspace: &Workspace) {
    match manager.missing_worktree(workspace).await {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(error) => {
            log!("deleted worktree {} retained: {error:#}", workspace.name);
            return;
        }
    }
    let removal = manager.remove_deleted(&workspace.id).await;
    let announced = manager.announce_removal(workspace, Cleanup::DeletedWorktree, &removal);
    if let Err(error) = announced.await {
        log!("record retained workspace {}: {error:#}", workspace.name);
    }
}

pub async fn run(manager: Arc<Manager>) {
    let mut timers = Timers::default();
    let mut logged = LoggedFailures::default();
    loop {
        {
            let _operation = manager.gates.operation().await;
            let result = sweep(&manager, &mut timers).await;
            let failed = logged.new_failures(&result);
            if let Some(failed) = &failed {
                log!("auto cleanup: {failed}");
            }
            manager.record_sweep(result).await;
            manager.record_cleanup_run(failed).await;
        }
        tokio::select! {
            _ = sleep(SWEEP_INTERVAL) => {},
            _ = manager.signals.cleanup_woken() => {},
        }
    }
}

#[cfg(test)]
mod tests;
