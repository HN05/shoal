//! Automatic removal of idle, clean, fully pushed worktrees, of merged ones,
//! and of workspaces whose directory was deleted outside Shoal; the user can
//! remove the idle candidates on request.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    os::unix::fs::MetadataExt,
    path::Path,
    sync::{Arc, PoisonError},
    time::Duration,
};
use tokio::time::{Instant, sleep};

use crate::{
    daemon::{
        caches::Emptying, events::EventCause, log, notifications::NotificationKind,
        workspace::Manager,
    },
    model::{CleanupReason, Workspace},
    state::WorkspaceState,
    time::{local_seconds, unix_seconds},
};

mod grace;
mod kept_branches;
mod merged;

const SWEEP_INTERVAL: Duration = Duration::from_secs(30);
/// A pass normally finishes within seconds; none for this long means the
/// loop is stuck or has stopped.
const STALLED_AFTER: Duration = Duration::from_secs(10 * 60);

/// The outcome of recent passes, which workspace status reports.
pub struct Health {
    started: Moment,
    finished: Option<Moment>,
    failure: Option<Failure>,
}

/// A point in time measured both ways: the monotonic clock excludes sleep, so
/// a Mac waking up does not look like a stalled loop, and the Unix seconds
/// are what messages show.
#[derive(Clone, Copy)]
struct Moment {
    at: Instant,
    unix: u64,
}

impl Moment {
    fn now() -> Self {
        Self {
            at: Instant::now(),
            unix: unix_seconds(),
        }
    }
}

/// Consecutive failed passes.
struct Failure {
    since: u64,
    passes: u64,
    error: String,
    notified: bool,
}

/// [`Health`] as the sweep records it and status reads it; each operation
/// holds the lock only while it runs.
#[derive(Default)]
pub(crate) struct CleanupHealth(std::sync::Mutex<Health>);

impl CleanupHealth {
    fn lock(&self) -> std::sync::MutexGuard<'_, Health> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn problem(&self, now: Instant) -> Option<String> {
        self.lock().problem(now)
    }

    fn record(&self, now: Moment, result: &Result<()>) -> Option<String> {
        self.lock().record(now, result)
    }

    /// The user was told about the current failure.
    fn failure_notified(&self) {
        if let Some(failure) = &mut self.lock().failure {
            failure.notified = true;
        }
    }
}

impl Default for Health {
    fn default() -> Self {
        Self {
            started: Moment::now(),
            finished: None,
            failure: None,
        }
    }
}

impl Health {
    /// A short description of a failing or stalled loop for workspace status.
    fn problem(&self, now: Instant) -> Option<String> {
        if let Some(failure) = &self.failure {
            Some(format!(
                "Automatic cleanup failed {} consecutive passes since {}: {}",
                failure.passes,
                local_seconds(failure.since as i64),
                failure.error
            ))
        } else if self.stalled(now) {
            Some(format!(
                "{}; the daemon log has details",
                self.stalled_message()
            ))
        } else {
            None
        }
    }

    fn stalled(&self, now: Instant) -> bool {
        now.duration_since(self.finished.unwrap_or(self.started).at) >= STALLED_AFTER
    }

    fn stalled_message(&self) -> String {
        match self.finished {
            Some(finished) => format!(
                "Automatic cleanup has not finished a pass since {}",
                local_seconds(finished.unix as i64)
            ),
            None => format!(
                "Automatic cleanup has not finished a pass since the daemon started at {}",
                local_seconds(self.started.unix as i64)
            ),
        }
    }

    /// Record a finished pass; returns the error to tell the user about when
    /// a failure has not been reported yet.
    fn record(&mut self, now: Moment, result: &Result<()>) -> Option<String> {
        self.finished = Some(now);
        let Err(error) = result else {
            self.failure = None;
            return None;
        };
        let failure = self.failure.get_or_insert(Failure {
            since: now.unix,
            passes: 0,
            error: String::new(),
            notified: false,
        });
        failure.passes += 1;
        failure.error = format!("{error:#}");
        (!failure.notified).then(|| failure.error.clone())
    }
}

impl Manager {
    /// Why automatic cleanup is not working, if it is not.
    pub fn cleanup_problem(&self) -> Option<String> {
        self.cleanup_health.problem(Instant::now())
    }

    /// Notify the user when passes start failing. A notification that cannot
    /// be recorded, as when the disk is full, is retried on the next pass.
    async fn record_sweep(&self, result: Result<()>) {
        let Some(error) = self.cleanup_health.record(Moment::now(), &result) else {
            return;
        };
        let message = format!("automatic cleanup failed: {error}");
        match self
            .record_notification(Default::default(), NotificationKind::CleanupFailed, message)
            .await
        {
            Ok(_) => self.cleanup_health.failure_notified(),
            Err(error) => log!("notification not recorded: {error:#}"),
        }
    }
}

/// The rules that made a workspace a cleanup candidate, rechecked during removal.
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

/// What automatic cleanup must see unchanged before removing a workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub rule: Rule,
    /// HEAD and recorded activity, cheap enough to compare on every pass.
    pub signals: u64,
    /// The metadata of every file in the worktree, which takes a full walk.
    pub tree: u64,
}

/// When a workspace was last seen unchanged in a removable state.
struct Idle {
    snapshot: Snapshot,
    since: Instant,
}

#[derive(Default)]
pub struct Timers {
    idle: HashMap<String, Idle>,
    merged: merged::State,
    kept_branches: kept_branches::State,
}

impl Timers {
    /// Whether the timer has run for `delay` with unchanged `signals`; a
    /// change, or no timer, needs a full snapshot to start from.
    fn expired(&self, id: &str, signals: u64, now: Instant, delay: Duration) -> Expiry {
        match self.idle.get(id) {
            Some(idle) if idle.snapshot.signals != signals => Expiry::Restart,
            Some(idle) if now.duration_since(idle.since) < delay => Expiry::Running,
            Some(idle) => Expiry::Expired(idle.snapshot),
            None => Expiry::Restart,
        }
    }

    fn start(&mut self, id: &str, snapshot: Snapshot, now: Instant) {
        self.idle.insert(
            id.into(),
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

/// HEAD and the activity counter of a cleanup candidate.
pub fn signals(head: &str, activity: u64) -> u64 {
    let mut hash = DefaultHasher::new();
    head.hash(&mut hash);
    activity.hash(&mut hash);
    hash.finish()
}

/// Metadata only, including ignored files; never follow symlinks out of the tree.
/// Git administrative storage is shared and inspected separately through HEAD.
/// Change times only move forward and cannot be set, unlike modification
/// times, so two equal fingerprints mean no file changed between them.
pub fn fingerprint(root: &Path) -> Result<u64> {
    fn visit(path: &Path, hash: &mut DefaultHasher) -> Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        path.hash(hash);
        metadata.len().hash(hash);
        metadata.modified()?.hash(hash);
        (metadata.ctime(), metadata.ctime_nsec()).hash(hash);
        if metadata.is_dir() {
            let mut children = fs::read_dir(path)?
                .map(|entry| entry.map(|e| e.path()))
                .collect::<std::io::Result<Vec<_>>>()?;
            children.sort();
            for child in children {
                if child.file_name().is_some_and(|name| name == ".git") {
                    continue;
                }
                visit(&child, hash)?;
            }
        }
        Ok(())
    }
    let mut hash = DefaultHasher::new();
    visit(root, &mut hash)?;
    Ok(hash.finish())
}

/// One pass of every automatic cleanup. A failed step does not skip the
/// steps after it; the error names each step that failed.
pub async fn sweep(manager: &Manager, timers: &mut Timers) -> Result<()> {
    let mut failed = Vec::new();
    let mut step = |name: &str, result: Result<()>| {
        if let Err(error) = result {
            failed.push(format!("{name}: {error:#}"));
        }
    };
    step(
        "ownership recheck",
        manager.recheck_startup_failures().await,
    );
    step("grace periods", manager.begin_grace_pass().await);
    step("branch observation", observe_branches(manager).await);
    step("completion cleanup", manager.sweep_completed().await);
    step("PR cleanup", manager.sweep_prs().await);
    step("idle cleanup", sweep_idle(manager, timers).await);
    step(
        "merged cleanup",
        merged::sweep(manager, &mut timers.merged).await,
    );
    step(
        "kept branches",
        kept_branches::sweep(manager, &mut timers.kept_branches).await,
    );
    step("checkpoint expiry", manager.expire_checkpoints().await);
    step("contributions", manager.sweep_contributions().await);
    step("proposal files", manager.sweep_proposal_files().await);
    step("grace periods", manager.end_unclaimed_grace().await);
    ensure!(failed.is_empty(), "{}", failed.join("; "));
    Ok(())
}

async fn observe_branches(manager: &Manager) -> Result<()> {
    for workspace in manager.list_workspaces().await? {
        if workspace.state == WorkspaceState::Ready
            && workspace.path.is_dir()
            && let Err(error) = manager.observe_workspace_branch(&workspace).await
        {
            log!("branch observation skipped {}: {error:#}", workspace.name);
        }
    }
    Ok(())
}

/// Each workspace's idle delay comes from its own layered config; a disabled
/// one gets only deleted-directory cleanup. A failure in one workspace does
/// not skip the others.
async fn sweep_idle(manager: &Manager, timers: &mut Timers) -> Result<()> {
    let workspaces = manager.list_workspaces().await?;
    timers
        .idle
        .retain(|id, _| workspaces.iter().any(|workspace| &workspace.id == id));
    let mut failed = Vec::new();
    for workspace in workspaces {
        if let Err(error) = sweep_workspace(manager, timers, &workspace).await {
            failed.push(format!("{}: {error:#}", workspace.name));
        }
    }
    ensure!(failed.is_empty(), "{}", failed.join("; "));
    Ok(())
}

async fn sweep_workspace(
    manager: &Manager,
    timers: &mut Timers,
    workspace: &Workspace,
) -> Result<()> {
    // Only a worktree that existed (identity recorded) can have been deleted;
    // a failed creation keeps its error until removed explicitly.
    if matches!(
        workspace.state,
        WorkspaceState::Ready | WorkspaceState::Failed
    ) && workspace.git_dir.is_some()
        && !workspace.path.try_exists()?
    {
        timers.idle.remove(&workspace.id);
        remove_deleted(manager, workspace).await;
        return Ok(());
    }
    let Some((delay, signals)) = observe(manager, workspace).await else {
        timers.idle.remove(&workspace.id);
        return Ok(());
    };
    // Each pass compares only the signals; the tree is walked when the timer
    // starts and again when it expires.
    let now = Instant::now();
    let started = match timers.expired(&workspace.id, signals, now, delay) {
        Expiry::Running => return Ok(()),
        Expiry::Restart => None,
        Expiry::Expired(snapshot) => Some(snapshot),
    };
    let Some(snapshot) = candidate(manager, workspace).await else {
        timers.idle.remove(&workspace.id);
        return Ok(());
    };
    if started != Some(snapshot) {
        timers.start(&workspace.id, snapshot, now);
        return Ok(());
    }
    let removal = manager.remove_idle(&workspace.id, snapshot).await;
    if matches!(removal, Ok(false)) {
        return Ok(());
    }
    timers.idle.remove(&workspace.id);
    let (kind, message, recorded) = match removal {
        Ok(_) => {
            log!("auto cleanup removed {}", workspace.name);
            (
                NotificationKind::WorkspaceRemoved,
                crate::daemon::workspace::removal_notice(
                    &workspace.name,
                    "removed by idle cleanup",
                ),
                Ok(()),
            )
        }
        Err(error) => {
            log!("auto cleanup retained {}: {error:#}", workspace.name);
            let recorded = manager
                .record_retained(&workspace.id, EventCause::Idle, &error)
                .await;
            (
                NotificationKind::CleanupFailed,
                format!("idle cleanup retained the workspace: {error:#}"),
                recorded,
            )
        }
    };
    manager.notify(Some(&workspace.name), kind, message).await;
    recorded
}

/// The idle delay of a ready workspace, or `None` when idle cleanup is
/// disabled for it.
async fn idle_delay(manager: &Manager, workspace: &Workspace) -> Result<Option<Duration>> {
    if workspace.state != WorkspaceState::Ready {
        return Ok(None);
    }
    let settings = manager.workspace_settings(workspace).await?;
    Ok(settings.cleanup.auto.delay())
}

/// The idle delay and cleanup signals of a ready, removable workspace;
/// `None` when it is not, its idle cleanup is disabled, or its config cannot
/// be read.
async fn observe(manager: &Manager, workspace: &Workspace) -> Option<(Duration, u64)> {
    let observed = async {
        let Some(delay) = idle_delay(manager, workspace).await? else {
            return Ok(None);
        };
        let signals = manager.cleanup_signals(&workspace.id).await?;
        Ok(signals.map(|signals| (delay, signals)))
    }
    .await;
    skip_failed(workspace, observed)
}

/// The full snapshot of a workspace [`observe`] accepts.
pub(super) async fn candidate(manager: &Manager, workspace: &Workspace) -> Option<Snapshot> {
    let observed = async {
        if idle_delay(manager, workspace).await?.is_none() {
            return Ok(None);
        }
        manager.cleanup_snapshot(&workspace.id).await
    }
    .await;
    skip_failed(workspace, observed)
}

fn skip_failed<T>(workspace: &Workspace, observed: Result<Option<T>>) -> Option<T> {
    observed.unwrap_or_else(|error| {
        log!("auto cleanup skipped {}: {error:#}", workspace.name);
        None
    })
}

/// The workspaces a manual cleanup removed, or would remove in a dry run,
/// and those it retained because inspection or removal failed. A dry run
/// also lists why it keeps each other workspace.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ManualCleanup {
    pub dry_run: bool,
    pub removed: Vec<String>,
    pub retained: Vec<Retained>,
    #[serde(default)]
    pub kept: Vec<Kept>,
    #[serde(default)]
    pub caches: Vec<CacheCleanup>,
}

impl ManualCleanup {
    /// A dry run lists why it keeps each workspace, and a request naming
    /// workspaces says why it kept those.
    fn keep(&mut self, workspace: String, reason: CleanupReason, named: bool) {
        if self.dry_run || named {
            self.kept.push(Kept { workspace, reason });
        }
    }
}

/// Which items of one kind a cleanup request covers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pick {
    #[default]
    None,
    All,
    /// Workspaces, or the repositories whose caches to empty, by name or ID.
    Only(Vec<String>),
}

impl Pick {
    fn covers(&self, name: &str, id: &str) -> bool {
        match self {
            Self::None => false,
            Self::All => true,
            Self::Only(names) => names.iter().any(|n| n == name || n == id),
        }
    }

    /// Fails for a name none of `items`, as `(name, id)` pairs, has.
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
                    "{selector} is not a workspace or repository this cleanup covers"
                );
            }
        }
        Ok(())
    }
}

/// What a cleanup request covers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    #[serde(default)]
    pub workspaces: Pick,
    #[serde(default)]
    pub caches: Pick,
    /// Only this repository's workspaces and caches, by any repository selector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Retained {
    pub workspace: String,
    pub error: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Kept {
    pub workspace: String,
    pub reason: CleanupReason,
}

/// One repository's shared caches on a cleanup request.
#[derive(Debug, Serialize, Deserialize)]
pub struct CacheCleanup {
    pub repository: String,
    pub repository_id: String,
    #[serde(flatten)]
    pub emptying: Emptying,
}

/// Remove the selected workspaces idle or merged cleanup would remove, now:
/// the user's request replaces their idle periods and applies where they are
/// disabled. Then empty the selected shared caches nothing is using.
pub async fn on_request(
    manager: &Manager,
    dry_run: bool,
    selection: &Selection,
) -> Result<ManualCleanup> {
    let targets = Targets::of(manager, selection).await?;
    let mut report = ManualCleanup {
        dry_run,
        ..ManualCleanup::default()
    };
    remove_on_request(
        manager,
        &mut report,
        targets.workspaces,
        &selection.workspaces,
    )
    .await?;
    report.caches =
        empty_caches_on_request(manager, dry_run, targets.repositories, &selection.caches).await?;
    Ok(report)
}

/// The workspaces and repositories a cleanup request can reach.
struct Targets {
    workspaces: Vec<Workspace>,
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
        let within = |id: &str| repository.as_deref().is_none_or(|only| only == id);
        let targets = Self {
            workspaces: manager
                .list_workspaces()
                .await?
                .into_iter()
                .filter(|workspace| within(&workspace.repository_id))
                .collect(),
            repositories: manager
                .repositories()
                .await?
                .into_iter()
                .filter(|repo| within(&repo.id))
                .collect(),
        };
        selection.workspaces.ensure_known(
            targets
                .workspaces
                .iter()
                .map(|w| (w.name.as_str(), w.id.as_str())),
        )?;
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
        if !pick.covers(crate::repository::name(&repo), &repo.id) {
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

async fn remove_on_request(
    manager: &Manager,
    report: &mut ManualCleanup,
    workspaces: Vec<Workspace>,
    pick: &Pick,
) -> Result<()> {
    let named = matches!(pick, Pick::Only(_));
    let mut fetches = merged::Fetches::default();
    for workspace in workspaces {
        if !pick.covers(&workspace.name, &workspace.id) {
            continue;
        }
        if workspace.state != WorkspaceState::Ready {
            let state = workspace.state;
            report.keep(workspace.name, CleanupReason::NotReady { state }, named);
            continue;
        }
        let snapshot = match requested_candidacy(manager, &workspace, &mut fetches).await {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(reason)) => {
                report.keep(workspace.name, reason, named);
                continue;
            }
            Err(error) => {
                log!("manual cleanup skipped {}: {error:#}", workspace.name);
                report.retained.push(Retained {
                    workspace: workspace.name,
                    error: format!("{error:#}"),
                });
                continue;
            }
        };
        if report.dry_run {
            report.removed.push(workspace.name);
            continue;
        }
        match manager.remove_on_request(&workspace.id, snapshot).await {
            Ok(()) => {
                log!("manual cleanup removed {}", workspace.name);
                report.removed.push(workspace.name);
            }
            Err(error) => {
                manager
                    .record_retained(&workspace.id, EventCause::Manual, &error)
                    .await?;
                log!("manual cleanup retained {}: {error:#}", workspace.name);
                report.retained.push(Retained {
                    workspace: workspace.name,
                    error: format!("{error:#}"),
                });
            }
        }
    }
    Ok(())
}

/// The snapshot of an idle or merged cleanup candidate, or why idle cleanup
/// keeps the workspace. Merged candidates need a freshly fetched default branch.
async fn requested_candidacy(
    manager: &Manager,
    workspace: &Workspace,
    fetches: &mut merged::Fetches,
) -> Result<Result<Snapshot, CleanupReason>> {
    let reason = match manager.cleanup_candidacy(&workspace.id).await? {
        Ok(snapshot) => return Ok(Ok(snapshot)),
        Err(reason) => reason,
    };
    if !fetches.fresh(manager, &workspace.repository_id).await {
        return Ok(Err(reason));
    }
    Ok(manager.merged_snapshot(&workspace.id).await?.ok_or(reason))
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
    let (kind, message) = match manager.remove_deleted(&workspace.id).await {
        Ok(_) => {
            log!("forgot deleted worktree {}", workspace.name);
            (
                NotificationKind::WorkspaceRemoved,
                "forgotten after its directory was deleted; branch retained".to_owned(),
            )
        }
        Err(error) => {
            if let Err(record_error) = manager
                .record_retained(&workspace.id, EventCause::MissingDirectory, &error)
                .await
            {
                log!(
                    "record retained workspace {}: {record_error:#}",
                    workspace.name
                );
            }
            log!("deleted worktree {} retained: {error:#}", workspace.name);
            (
                NotificationKind::CleanupFailed,
                format!("deleted worktree retained: {error:#}"),
            )
        }
    };
    manager.notify(Some(&workspace.name), kind, message).await;
}

pub async fn run(manager: Arc<Manager>) {
    let mut timers = Timers::default();
    loop {
        {
            let _operation = manager.gates.operation().await;
            let result = sweep(&manager, &mut timers).await;
            if let Err(error) = &result {
                log!("auto cleanup: {error:#}");
            }
            manager.record_sweep(result).await;
        }
        tokio::select! {
            _ = sleep(SWEEP_INTERVAL) => {},
            _ = manager.signals.cleanup_woken() => {},
        }
    }
}

#[cfg(test)]
mod tests;
