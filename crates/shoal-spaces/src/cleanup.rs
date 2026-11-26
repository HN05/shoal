//! Automatic and requested cleanup of idle and merged spaces, and of
//! worktrees deleted outside Shoal. See `docs/design/cleanup.md`.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::model::EventCause;
use anyhow::{Result, ensure};
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    os::unix::fs::MetadataExt,
    path::Path,
    time::Duration,
};
use tokio::time::Instant;

use crate::{
    fsutil::off_runtime,
    model::{CleanupReason, Space, Workspace},
    process::DirectoryUsers,
    state::WorkspaceState,
    time::UnixTime,
    {file_access, lifecycle::not_ready, log, spaces::removal_notices::Cleanup},
};
use shoal_protocol::cleanup::Emptying;
pub use shoal_protocol::cleanup::{CacheCleanup, Kept, ManualCleanup, Pick, Retained, Selection};
use shoal_protocol::space_names::SpaceName;

pub mod grace;
pub mod health;
pub mod kept_branches;
pub mod merged;
pub mod runs;

pub use health::CleanupHealth;
pub use runs::Removed;
pub use shoal_daemon_base::failures::{LoggedFailures, SpaceFailures, SweepFailures};

pub const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// The rules that made a space a cleanup candidate, rechecked during removal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// Clean, unused, every commit pushed or merged, and no assignment state.
    Idle,
    /// Merged into the default branch and unused, whatever its assignment.
    Merged,
}

impl Rule {
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
    /// What the space records, with its worktree's HEAD: cheap enough to
    /// compare on every pass.
    pub signals: u64,
    /// The metadata of every file in the worktree, which takes a full walk.
    pub tree: u64,
}

/// When a space was last seen unchanged in a removable state.
pub struct Idle {
    pub snapshot: Snapshot,
    pub since: Instant,
}

#[derive(Default)]
pub struct Timers {
    pub idle: HashMap<SpaceId, Idle>,
    pub merged: merged::State,
    pub kept_branches: kept_branches::State,
}

impl Timers {
    /// A change of `signals`, or no timer, needs a full snapshot to start
    /// from.
    pub fn expired(&self, id: &SpaceId, signals: u64, now: Instant, delay: Duration) -> Expiry {
        match self.idle.get(id) {
            Some(idle) if idle.snapshot.signals != signals => Expiry::Restart,
            Some(idle) if now.duration_since(idle.since) < delay => Expiry::Running,
            Some(idle) => Expiry::Expired(idle.snapshot),
            None => Expiry::Restart,
        }
    }

    pub fn start(&mut self, id: &SpaceId, snapshot: Snapshot, now: Instant) {
        self.idle.insert(
            id.clone(),
            Idle {
                snapshot,
                since: now,
            },
        );
    }
}

pub enum Expiry {
    Running,
    Restart,
    Expired(Snapshot),
}

/// Hashes the activity counter, when the agent last stopped, and the
/// worktree's HEAD where there is one.
pub fn signals(activity: u64, stopped_at: Option<UnixTime>, head: Option<&str>) -> u64 {
    let mut hash = DefaultHasher::new();
    activity.hash(&mut hash);
    stopped_at.hash(&mut hash);
    head.hash(&mut hash);
    hash.finish()
}

#[derive(Clone, Copy, Debug)]
pub struct Tree {
    /// The metadata of every file. Change times only move forward and cannot
    /// be set, unlike modification times, so two equal fingerprints mean no
    /// file changed between them.
    pub fingerprint: u64,
    pub changed: UnixTime,
}

impl Tree {
    pub const EMPTY: Self = Self {
        fingerprint: 0,
        changed: UnixTime::EPOCH,
    };
}

pub fn change_time(metadata: &fs::Metadata) -> UnixTime {
    UnixTime::from_seconds(metadata.ctime().max(metadata.mtime()))
}

/// Reads metadata only, ignored files included, and never follows symlinks
/// out of the tree. Git's shared administrative storage is inspected
/// separately, through HEAD.
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

/// A failure belongs to the workspace it concerns, so the log has it once
/// while it repeats. A directory whose file access stalled is reported as
/// such and skipped.
pub async fn observe_branches(daemon: &dyn Daemon) -> Result<()> {
    let mut failed = SpaceFailures::default();
    for workspace in daemon.spaces().list_workspaces().await? {
        if workspace.state != WorkspaceState::Ready || !is_dir(&workspace.path).await {
            continue;
        }
        let observed = daemon.spaces().observe_workspace_branch(daemon, &workspace);
        if let Err(error) = daemon
            .spaces()
            .file_access
            .checked(&daemon.spaces().notifications, &workspace.path, observed)
            .await
            && !daemon
                .spaces()
                .file_access
                .sweep_skips(&daemon.spaces().notifications, &workspace.path)
                .await
        {
            failed.push(&workspace.id, &workspace.name, &error);
        }
    }
    failed.into_result()
}

impl Spaces {
    /// Leaves the branch Shoal owns as it is.
    pub async fn observe_workspace_branch(
        &self,
        daemon: &dyn Daemon,
        workspace: &crate::model::Workspace,
    ) -> Result<()> {
        self.verify_worktree(daemon, workspace).await?;
        let branch = crate::git::checked_out_branch(&workspace.path).await?;
        let id = workspace.id.clone();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET observed_branch=?2 WHERE id=?1 AND observed_branch IS NOT ?2",
                    rusqlite::params![id, branch],
                )?;
                Ok(())
            })
            .await
    }
}

/// Reads off the runtime's threads and within a check's time limit, since
/// stalled file access can hold the read. The daemon's own read says nothing
/// about the processes it starts there, so it records no stall.
async fn is_dir(path: &std::path::Path) -> bool {
    matches!(
        tokio::time::timeout(file_access::LIMIT, off_runtime::is_dir(path)).await,
        Ok(Ok(true))
    )
}

/// A chat reads its idle delay from its repository's config. A disabled
/// worktree gets only deleted-directory cleanup. A failure in one space does
/// not skip the others.
pub async fn sweep_idle(
    daemon: &dyn Daemon,
    timers: &mut Timers,
    users: &DirectoryUsers,
) -> Result<()> {
    let spaces = daemon.spaces().present_spaces(daemon).await?;
    timers
        .idle
        .retain(|id, _| spaces.iter().any(|space| &space.id == id));
    let mut failed = SpaceFailures::default();
    for space in spaces {
        if let Err(error) = sweep_space(daemon, timers, &space, users).await {
            failed.push(&space.id, &space.name, &error);
        }
    }
    failed.into_result()
}

/// Removes the space once its idle delay and grace period passed with it
/// unchanged, or forgets its deleted worktree.
async fn sweep_space(
    daemon: &dyn Daemon,
    timers: &mut Timers,
    space: &Space,
    users: &DirectoryUsers,
) -> Result<()> {
    if let Some(workspace) = space.worktree() {
        // Its idle timer stays for the pass after a check there finishes.
        if daemon
            .spaces()
            .file_access
            .sweep_skips(&daemon.spaces().notifications, &workspace.path)
            .await
        {
            return Ok(());
        }
        if deleted(workspace).await?
            && let Some(workspace) = space.to_workspace()
        {
            timers.idle.remove(&workspace.id);
            remove_deleted(daemon, &workspace).await;
            return Ok(());
        }
    }
    let observed = observe(daemon, space, users).await;
    let Ok(Some((delay, signals))) = observed else {
        timers.idle.remove(&space.id);
        return observed.map(drop);
    };
    // Each pass compares only the signals. Git's state is inspected and the
    // files walked when the timer starts and again when it expires.
    let now = Instant::now();
    let started = match timers.expired(&space.id, signals, now, delay) {
        Expiry::Running => return Ok(()),
        Expiry::Restart => None,
        Expiry::Expired(snapshot) => Some(snapshot),
    };
    let snapshot = candidate(daemon, space, users).await;
    let Ok(Some(snapshot)) = snapshot else {
        timers.idle.remove(&space.id);
        return snapshot.map(drop);
    };
    if started != Some(snapshot) {
        timers.start(&space.id, snapshot, now);
        return Ok(());
    }
    let removal = (daemon
        .spaces()
        .remove_unused(daemon, space, snapshot, EventCause::Idle))
    .await;
    if matches!(removal, Ok(false)) {
        return Ok(());
    }
    timers.idle.remove(&space.id);
    (daemon
        .spaces()
        .announce_removal(daemon, space, Cleanup::Idle, &removal))
    .await
}

/// Only a worktree that existed, with its identity recorded, can have been
/// deleted. A failed creation keeps its error until removed explicitly.
async fn deleted(workspace: &crate::model::Worktree) -> Result<bool> {
    Ok(matches!(
        workspace.state,
        WorkspaceState::Ready | WorkspaceState::Failed
    ) && workspace.checkout.git_dir().is_some()
        && !off_runtime::try_exists(&workspace.path).await?)
}

/// `None` when idle cleanup is disabled for the space or its worktree is not
/// ready.
async fn idle_delay(daemon: &dyn Daemon, space: &Space) -> Result<Option<Duration>> {
    if space.worktree().and_then(not_ready).is_some() {
        return Ok(None);
    }
    let settings = daemon.space_settings(space).await?;
    Ok(settings.cleanup.auto.delay())
}

/// `None` when something recorded or a process keeps the space, or its idle
/// cleanup is disabled.
async fn observe(
    daemon: &dyn Daemon,
    space: &Space,
    users: &DirectoryUsers,
) -> Result<Option<(Duration, u64)>> {
    let Some(delay) = idle_delay(daemon, space).await? else {
        return Ok(None);
    };
    let signals = daemon
        .spaces()
        .idle_timer_signals(daemon, space, users)
        .await?;
    Ok(signals.map(|signals| (delay, signals)))
}

/// Reads the processes in its worktree from `users`.
pub async fn candidate(
    daemon: &dyn Daemon,
    space: &Space,
    users: &DirectoryUsers,
) -> Result<Option<Snapshot>> {
    if idle_delay(daemon, space).await?.is_none() {
        return Ok(None);
    }
    (daemon
        .spaces()
        .candidate_snapshot(daemon, space, Rule::Idle, &[], users))
    .await
}

/// Operations on the client type [`ManualCleanup`] that only the `shoal`
/// command uses.
trait ManualCleanupExt {
    /// A dry run lists why it keeps each space, and a request naming spaces
    /// says why it kept those.
    fn keep(&mut self, space: SpaceName, reason: CleanupReason, named: bool);

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
    fn covers(&self, name: &str, id: &str) -> bool;

    /// Matches by ID alone, since names repeat.
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

/// The user's request replaces the spaces' idle periods and applies where
/// idle cleanup is disabled. Then empties the selected shared caches
/// nothing uses.
pub async fn on_request(
    daemon: &dyn Daemon,
    dry_run: bool,
    selection: &Selection,
) -> Result<ManualCleanup> {
    let targets = Targets::of(daemon, selection).await?;
    if !dry_run {
        daemon.cleanup_orphan_sessions().await;
    }
    let mut report = ManualCleanup {
        dry_run,
        ..ManualCleanup::default()
    };
    remove_on_request(daemon, &mut report, targets.spaces, &selection.spaces).await?;
    report.caches =
        empty_caches_on_request(daemon, dry_run, targets.repositories, &selection.caches).await?;
    Ok(report)
}

struct Targets {
    spaces: Vec<Space>,
    repositories: Vec<crate::model::Repository>,
}

impl Targets {
    /// Refuses until every name the selection gives is among them.
    async fn of(daemon: &dyn Daemon, selection: &Selection) -> Result<Self> {
        let repository = match &selection.repository {
            Some(selector) => Some(
                daemon
                    .repository(crate::repository::Selector::from(selector))
                    .await?
                    .id,
            ),
            None => None,
        };
        let within = |id: &RepositoryId| repository.as_ref().is_none_or(|only| only == id);
        // A chat without a repository is in none of them.
        let space_within = |space: &Space| match space.repository_id.as_ref() {
            Some(id) => within(id),
            None => repository.is_none(),
        };
        let targets = Self {
            spaces: (daemon.spaces().present_spaces(daemon).await?.into_iter())
                .filter(|space| space_within(space))
                .collect(),
            repositories: daemon
                .repositories()
                .await?
                .into_iter()
                .filter(|repo| within(&repo.id))
                .collect(),
        };
        let spaces = (targets.spaces.iter()).map(|space| ("", space.id.as_str()));
        selection.spaces.ensure_known(spaces)?;
        selection.caches.ensure_known(
            targets
                .repositories
                .iter()
                .map(|repo| (repo.display_name(), repo.id.as_str())),
        )?;
        Ok(targets)
    }
}

async fn empty_caches_on_request(
    daemon: &dyn Daemon,
    dry_run: bool,
    repositories: Vec<crate::model::Repository>,
    pick: &Pick,
) -> Result<Vec<CacheCleanup>> {
    let mut report = Vec::new();
    for repo in repositories {
        if !pick.covers(repo.display_name(), repo.id.as_str()) {
            continue;
        }
        let emptying = match daemon.empty_caches(&repo, dry_run).await {
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
            repository: repo.display_name().to_owned(),
            repository_id: repo.id.clone(),
            emptying,
        });
    }
    Ok(report)
}

async fn remove_on_request(
    daemon: &dyn Daemon,
    report: &mut ManualCleanup,
    spaces: Vec<Space>,
    pick: &Pick,
) -> Result<()> {
    let named = matches!(pick, Pick::Only(_));
    for space in spaces {
        let name = space.name.clone();
        if !pick.covers_space(&space.id) {
            continue;
        }
        if let Some(reason) = space.worktree().and_then(not_ready) {
            report.keep(name, reason, named);
            continue;
        }
        let snapshot = match requested_candidacy(daemon, &space).await {
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
            report.removed_ids.push(space.id.clone());
            continue;
        }
        match (daemon
            .spaces()
            .remove_unused(daemon, &space, snapshot, EventCause::Manual))
        .await
        {
            Ok(_) => {
                log!("manual cleanup removed {name}");
                report.removed.push(name.into());
                report.removed_ids.push(space.id.clone());
            }
            Err(error) => {
                (crate::events::record_retained(
                    &daemon.spaces().store,
                    &space.id,
                    EventCause::Manual,
                    &error,
                ))
                .await?;
                log!("manual cleanup retained {name}: {error:#}");
                report.retain(name, &error);
            }
        }
    }
    Ok(())
}

/// A merged worktree needs a freshly fetched default branch.
async fn requested_candidacy(
    daemon: &dyn Daemon,
    space: &Space,
) -> Result<Result<Snapshot, CleanupReason>> {
    let users = DirectoryUsers::default();
    let reason = match daemon
        .spaces()
        .idle_candidacy(daemon, space, &users)
        .await?
    {
        Ok(snapshot) => return Ok(Ok(snapshot)),
        Err(reason) => reason,
    };
    if let Some(repository) = space.worktree().and(space.repository_id.as_ref())
        && !daemon.default_branch_fresh(repository).await
    {
        return Ok(Err(reason));
    }
    let merged = daemon
        .spaces()
        .candidate_snapshot(daemon, space, Rule::Merged, &[], &users);
    Ok(merged.await?.ok_or(reason))
}

/// A deleted worktree is forgotten unless it was moved or still has commands
/// Shoal cannot verify. The branch is kept either way.
async fn remove_deleted(daemon: &dyn Daemon, workspace: &Workspace) {
    match daemon.spaces().missing_worktree(workspace).await {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(error) => {
            log!("deleted worktree {} retained: {error:#}", workspace.name);
            return;
        }
    }
    let removal = daemon.spaces().remove_deleted(daemon, &workspace.id).await;
    let announced =
        daemon
            .spaces()
            .announce_removal(daemon, workspace, Cleanup::DeletedWorktree, &removal);
    if let Err(error) = announced.await {
        log!("record retained workspace {}: {error:#}", workspace.name);
    }
}

#[cfg(test)]
mod tests;
