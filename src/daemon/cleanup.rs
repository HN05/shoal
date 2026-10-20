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
        caches::Emptying,
        doctor::{Check, CheckStatus},
        events::EventCause,
        log,
        notifications::NotificationKind,
        workspace::Manager,
    },
    model::{CleanupReason, Workspace},
    state::WorkspaceState,
    time::{local_seconds, unix_seconds},
};

mod grace;
mod merged;

const SWEEP_INTERVAL: Duration = Duration::from_secs(30);
/// A pass normally finishes within seconds; none for this long means the
/// loop is stuck or has stopped.
const STALLED_AFTER: Duration = Duration::from_secs(10 * 60);

/// The outcome of recent passes, reported by doctor and workspace status.
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
    fn check(&self, now: Instant) -> Check {
        let (status, message) = if let Some(failure) = &self.failure {
            (
                CheckStatus::Error,
                format!(
                    "Automatic cleanup failed {} consecutive passes since {}: {}",
                    failure.passes,
                    local_seconds(failure.since as i64),
                    failure.error
                ),
            )
        } else if self.stalled(now) {
            (CheckStatus::Error, self.stalled_message())
        } else if let Some(finished) = self.finished {
            (
                CheckStatus::Ok,
                format!(
                    "Automatic cleanup last finished a pass at {}",
                    local_seconds(finished.unix as i64)
                ),
            )
        } else {
            (
                CheckStatus::Ok,
                "Automatic cleanup has not finished its first pass yet".to_owned(),
            )
        };
        Check::new("cleanup", status, message)
    }

    /// A short description of a failing or stalled loop for workspace status.
    fn problem(&self, now: Instant) -> Option<String> {
        if let Some(failure) = &self.failure {
            Some(format!(
                "Automatic cleanup failing since {}; run shoal doctor",
                local_seconds(failure.since as i64)
            ))
        } else if self.stalled(now) {
            Some(format!("{}; run shoal doctor", self.stalled_message()))
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
    fn cleanup_health(&self) -> std::sync::MutexGuard<'_, Health> {
        self.cleanup_health
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub fn cleanup_check(&self) -> Check {
        self.cleanup_health().check(Instant::now())
    }

    /// Why automatic cleanup is not working, if it is not.
    pub fn cleanup_problem(&self) -> Option<String> {
        self.cleanup_health().problem(Instant::now())
    }

    /// Notify the user when passes start failing. A notification that cannot
    /// be recorded, as when the disk is full, is retried on the next pass.
    async fn record_sweep(&self, result: Result<()>) {
        let Some(error) = self.cleanup_health().record(Moment::now(), &result) else {
            return;
        };
        let message = format!("automatic cleanup failed: {error}");
        match self
            .record_notification(None, NotificationKind::CleanupFailed, message)
            .await
        {
            Ok(()) => {
                if let Some(failure) = &mut self.cleanup_health().failure {
                    failure.notified = true;
                }
            }
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
            // Unsaved files go into the restore point. A base stays until PR
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
    step("grace periods", manager.begin_grace_pass().await);
    step("branch observation", observe_branches(manager).await);
    step("completion cleanup", manager.sweep_completed().await);
    step("PR cleanup", manager.sweep_prs().await);
    step("idle cleanup", sweep_idle(manager, timers).await);
    step(
        "merged cleanup",
        merged::sweep(manager, &mut timers.merged).await,
    );
    step("restore expiry", manager.expire_restore_points().await);
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
    fn keep(&mut self, workspace: String, reason: CleanupReason) {
        if self.dry_run {
            self.kept.push(Kept { workspace, reason });
        }
    }
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
    #[serde(flatten)]
    pub emptying: Emptying,
}

/// Remove every workspace idle or merged cleanup would remove, now: the
/// user's request replaces their idle periods and applies where they are
/// disabled. With `caches`, empty the shared caches nothing is using afterwards.
pub async fn on_request(manager: &Manager, dry_run: bool, caches: bool) -> Result<ManualCleanup> {
    let mut report = remove_on_request(manager, dry_run).await?;
    if caches {
        report.caches = empty_caches_on_request(manager, dry_run).await?;
    }
    Ok(report)
}

async fn empty_caches_on_request(manager: &Manager, dry_run: bool) -> Result<Vec<CacheCleanup>> {
    let mut report = Vec::new();
    for repo in manager.repositories().await? {
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
            repository: crate::forge::repository::name(&repo).to_owned(),
            emptying,
        });
    }
    Ok(report)
}

async fn remove_on_request(manager: &Manager, dry_run: bool) -> Result<ManualCleanup> {
    let mut report = ManualCleanup {
        dry_run,
        ..ManualCleanup::default()
    };
    let mut fetches = merged::Fetches::default();
    for workspace in manager.list_workspaces().await? {
        if workspace.state != WorkspaceState::Ready {
            let state = workspace.state;
            report.keep(workspace.name, CleanupReason::NotReady { state });
            continue;
        }
        let snapshot = match requested_candidacy(manager, &workspace, &mut fetches).await {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(reason)) => {
                report.keep(workspace.name, reason);
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
        if dry_run {
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
    Ok(report)
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
            let _operation = manager.background_operations.read().await;
            let result = sweep(&manager, &mut timers).await;
            if let Err(error) = &result {
                log!("auto cleanup: {error:#}");
            }
            manager.record_sweep(result).await;
        }
        tokio::select! {
            _ = sleep(SWEEP_INTERVAL) => {},
            _ = manager.cleanup_notify.notified() => {},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_signals_restart_timer_and_expiry_returns_its_snapshot() {
        let mut timers = Timers::default();
        let now = Instant::now();
        let delay = Duration::from_secs(600);
        let snapshot = Snapshot {
            rule: Rule::Idle,
            signals: 1,
            tree: 7,
        };
        assert!(matches!(
            timers.expired("w", 1, now, delay),
            Expiry::Restart
        ));
        timers.start("w", snapshot, now);
        assert!(matches!(
            timers.expired("w", 1, now + delay / 2, delay),
            Expiry::Running
        ));
        assert!(matches!(
            timers.expired("w", 2, now + delay, delay),
            Expiry::Restart
        ));
        assert!(matches!(
            timers.expired("w", 1, now + delay, delay),
            Expiry::Expired(expired) if expired == snapshot
        ));
    }

    #[test]
    fn fingerprint_sees_rewrites_that_restore_the_modification_time() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("build-output");
        fs::write(&file, "before").unwrap();
        let metadata = fs::metadata(&file).unwrap();
        let modified = metadata.modified().unwrap();
        let change_time = |metadata: fs::Metadata| (metadata.ctime(), metadata.ctime_nsec());
        let changed = change_time(metadata);
        let before = fingerprint(root.path()).unwrap();
        // Filesystem timestamps are coarse; rewrite until the change time moves.
        while change_time(fs::metadata(&file).unwrap()) == changed {
            fs::write(&file, "after!").unwrap();
            fs::File::options()
                .write(true)
                .open(&file)
                .unwrap()
                .set_modified(modified)
                .unwrap();
        }
        assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), modified);
        assert_ne!(fingerprint(root.path()).unwrap(), before);
    }

    #[test]
    fn health_reports_failing_and_stalled_cleanup() {
        let mut health = Health::default();
        let (start, unix) = (health.started.at, health.started.unix);
        let after = |seconds| start + Duration::from_secs(seconds);
        // Wall time runs on while the Mac sleeps; the monotonic clock does not.
        let moment = |seconds, slept| Moment {
            at: after(seconds),
            unix: unix + seconds + slept,
        };
        assert_eq!(health.check(start).status, CheckStatus::Ok);
        assert_eq!(health.problem(start), None);
        let stalled = start + STALLED_AFTER;
        assert_eq!(health.check(stalled).status, CheckStatus::Error);
        assert!(
            health
                .problem(stalled)
                .unwrap()
                .contains("since the daemon started")
        );

        assert_eq!(health.record(moment(30, 0), &Ok(())), None);
        assert_eq!(health.record(moment(60, 3600), &Ok(())), None);
        assert_eq!(health.check(after(90)).status, CheckStatus::Ok);

        let failed = Err(anyhow::anyhow!("completion cleanup: disk is full"));
        assert_eq!(
            health.record(moment(90, 3600), &failed).as_deref(),
            Some("completion cleanup: disk is full")
        );
        assert_eq!(
            health.record(moment(120, 3600), &failed).as_deref(),
            Some("completion cleanup: disk is full"),
            "an unrecorded notification is retried"
        );
        health.failure.as_mut().unwrap().notified = true;
        assert_eq!(health.record(moment(150, 3600), &failed), None);
        let check = health.check(after(150));
        assert_eq!(check.status, CheckStatus::Error);
        assert!(
            check.message.contains("failed 3 consecutive passes"),
            "{}",
            check.message
        );
        assert!(check.message.ends_with("completion cleanup: disk is full"));
        assert!(
            health
                .problem(after(150))
                .unwrap()
                .contains("run shoal doctor")
        );

        assert_eq!(health.record(moment(180, 3600), &Ok(())), None);
        assert_eq!(health.check(after(180)).status, CheckStatus::Ok);
        assert_eq!(health.problem(after(180)), None);
        assert_eq!(
            health.check(after(180) + STALLED_AFTER).status,
            CheckStatus::Error
        );
    }

    #[tokio::test]
    async fn failing_passes_notify_once_and_show_in_status() {
        use crate::test_support::{manager, repository};
        let (temp, manager) = manager().await;
        let checkout = repository(temp.path(), "repo");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "done".into(), None, None, None)
            .await
            .unwrap();
        for _ in 0..2 {
            manager
                .record_sweep(Err(anyhow::anyhow!("completion cleanup: disk is full")))
                .await;
        }
        let notifications = manager.notifications(true, 100).await.unwrap();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].workspace, None);
        assert_eq!(notifications[0].kind, NotificationKind::CleanupFailed);
        assert_eq!(
            notifications[0].message,
            "automatic cleanup failed: completion cleanup: disk is full"
        );
        assert_eq!(manager.cleanup_check().status, CheckStatus::Error);
        let status = manager.workspace_status(&workspace.id).await.unwrap();
        assert!(status.cleanup_error.unwrap().contains("failing since"));

        manager.record_sweep(Ok(())).await;
        assert_eq!(manager.cleanup_check().status, CheckStatus::Ok);
        let status = manager.workspace_status(&workspace.id).await.unwrap();
        assert_eq!(status.cleanup_error, None);
    }

    #[tokio::test]
    async fn deleted_parent_sweep_releases_leases_and_retains_branch() {
        use crate::{
            daemon::{ports::PortRequest, resources::ResourceRequest},
            test_support::{git, manager, repository},
        };
        for pending_rename in [false, true] {
            let (temp, manager) = manager().await;
            let checkout = repository(temp.path(), "repo");
            fs::write(checkout.join(".shoal.toml"), "[resources.lock]\n").unwrap();
            crate::test_support::commit(&checkout, ".shoal.toml");
            let repo = manager
                .register_repository(checkout.to_str().unwrap().into(), None, None)
                .await
                .unwrap();
            let workspace = manager
                .create_workspace(&repo.id, "deleted-parent".into(), None, None, None)
                .await
                .unwrap();
            manager
                .acquire_port(&workspace.id, "web".into(), PortRequest::default(), None)
                .await
                .unwrap();
            manager
                .acquire_resource(
                    &workspace.id,
                    ResourceRequest {
                        mode: None,
                        kind: None,
                        pool: "lock".into(),
                        name: "lock".into(),
                        resource: None,
                        reason: None,
                        queue: false,
                    },
                    None,
                )
                .await
                .unwrap();
            if pending_rename {
                git(&workspace.path, &["branch", "-m", "new/topic"]);
                let id = workspace.id.clone();
                manager.store.run(move |db| {
                db.execute("INSERT INTO workspace_renames(workspace_id,name,branch) VALUES (?1,'new-topic','new/topic')", [&id])?;
                db.execute("UPDATE workspaces SET state='failed' WHERE id=?1", [&id])?;
                Ok(())
            }).await.unwrap();
            }
            fs::remove_dir_all(workspace.path.parent().unwrap()).unwrap();

            sweep(&manager, &mut Timers::default()).await.unwrap();

            assert!(manager.workspace(&workspace.id).await.is_err());
            assert!(manager.list_ports(None).await.unwrap().is_empty());
            assert!(manager.list_resources(None).await.unwrap().is_empty());
            let branch = if pending_rename {
                "refs/heads/new/topic"
            } else {
                "refs/heads/deleted-parent"
            };
            assert!(!git(&checkout, &["rev-parse", branch]).is_empty());
            let remaining = manager
                .store
                .run(|db| {
                    Ok(
                        db.query_row("SELECT COUNT(*) FROM workspace_renames", [], |row| {
                            row.get::<_, i64>(0)
                        })?,
                    )
                })
                .await
                .unwrap();
            assert_eq!(remaining, 0);
            assert!(
                !git(&checkout, &["worktree", "list", "--porcelain"])
                    .contains(workspace.path.to_str().unwrap())
            );
        }
    }

    #[tokio::test]
    async fn failed_step_does_not_skip_later_steps_and_is_named() {
        use crate::test_support::{manager, repository};
        let (temp, manager) = manager().await;
        let checkout = repository(temp.path(), "repo");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let done = manager
            .create_workspace(&repo.id, "done".into(), None, None, None)
            .await
            .unwrap();
        let idle = manager
            .create_workspace(&repo.id, "idle".into(), None, None, None)
            .await
            .unwrap();
        // Completion cleanup retains the dirty workspace and cannot record why.
        fs::write(done.path.join("work"), "retain").unwrap();
        manager.mark_done(&done.id, true).await.unwrap();
        manager
            .store
            .run(|db| {
                db.execute_batch(
                    "CREATE TEMP TRIGGER full_disk BEFORE UPDATE ON workspace_completion
                     BEGIN SELECT RAISE(ABORT, 'database or disk is full'); END;",
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let mut timers = Timers::default();
        timers.idle.insert(
            idle.id.clone(),
            Idle {
                snapshot: manager.cleanup_snapshot(&idle.id).await.unwrap().unwrap(),
                since: Instant::now() - Duration::from_secs(3600),
            },
        );
        let error = sweep(&manager, &mut timers).await.unwrap_err().to_string();
        assert!(
            error.starts_with("completion cleanup: ") && error.contains("disk is full"),
            "{error}"
        );
        assert!(!idle.path.exists());
        assert!(done.path.exists());
    }

    #[tokio::test]
    async fn invalid_pr_record_does_not_block_other_workspace_cleanup() {
        use crate::{
            forge::pr::Action,
            test_support::{manager, repository},
        };
        let (temp, manager) = manager().await;
        let repository_dir = repository(temp.path(), "repo");
        let repo = manager
            .register_repository(repository_dir.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        // Names put the invalid record first in the sweep.
        let invalid = manager
            .create_workspace(&repo.id, "a-invalid".into(), None, None, None)
            .await
            .unwrap();
        let merged = manager
            .create_workspace(&repo.id, "b-merged".into(), None, None, None)
            .await
            .unwrap();
        let idle = manager
            .create_workspace(&repo.id, "c-idle".into(), None, None, None)
            .await
            .unwrap();
        manager
            .set_pr(&merged.id, Action::Acknowledge)
            .await
            .unwrap();
        let id = invalid.id.clone();
        let record = r#"{"url":null,"head":null,"error":null}"#;
        manager
            .store
            .run(move |db| {
                db.execute(
                    "INSERT INTO pr_cleanup(workspace_id,record) VALUES (?1,?2)",
                    rusqlite::params![id, record],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let mut timers = Timers::default();
        timers.idle.insert(
            idle.id.clone(),
            Idle {
                snapshot: manager.cleanup_snapshot(&idle.id).await.unwrap().unwrap(),
                since: Instant::now() - Duration::from_secs(3600),
            },
        );
        sweep(&manager, &mut timers).await.unwrap();
        assert!(invalid.path.exists());
        assert!(!merged.path.exists());
        assert!(!idle.path.exists());
        assert_eq!(manager.list_workspaces().await.unwrap().len(), 1);
        // Keep the invalid bytes for diagnosis and explicit clear.
        let id = invalid.id.clone();
        let stored: String = manager
            .store
            .run(move |db| {
                Ok(db.query_row(
                    "SELECT record FROM pr_cleanup WHERE workspace_id=?1",
                    [id],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(stored, record);
        sweep(&manager, &mut timers).await.unwrap();
        let notifications = manager.notifications(true, 100).await.unwrap();
        let failures: Vec<_> = notifications
            .iter()
            .filter(|notification| notification.workspace.as_deref() == Some(&invalid.name))
            .collect();
        assert_eq!(
            failures.len(),
            1,
            "repeated invalid records collapse until read"
        );
        assert_eq!(failures[0].kind, NotificationKind::CleanupFailed);
        assert!(
            failures[0]
                .message
                .contains("expected exactly one of url or head")
        );
    }

    #[tokio::test]
    async fn status_names_the_first_reason_cleanup_keeps_a_workspace() {
        use crate::test_support::{commit, git, manager, repository};
        let (temp, manager) = manager().await;
        let checkout = repository(temp.path(), "repo");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "kept".into(), None, None, None)
            .await
            .unwrap();
        let reason = || async { manager.cleanup_reason(&workspace).await };
        assert_eq!(reason().await, CleanupReason::Idle { minutes: 10 });

        fs::write(workspace.path.join("work"), "work").unwrap();
        assert_eq!(reason().await, CleanupReason::Dirty);
        commit(&workspace.path, "work");
        assert_eq!(reason().await, CleanupReason::Unpushed { commits: 1 });
        git(&checkout, &["merge", "--ff-only", "kept"]);

        manager
            .acquire_hold(&workspace.id, "review".into(), None)
            .await
            .unwrap();
        manager.mark_done(&workspace.id, false).await.unwrap();
        assert_eq!(reason().await, CleanupReason::Held);
        manager
            .release_hold(&workspace.id, "review".into())
            .await
            .unwrap();
        assert_eq!(reason().await, CleanupReason::Completed);
        manager.undo_done(&workspace.id).await.unwrap();

        manager
            .set_repository_config(&repo.id, Some("[cleanup.auto]\nenabled = false\n".into()))
            .await
            .unwrap();
        let status = manager.workspace_status(&workspace.id).await.unwrap();
        assert_eq!(status.cleanup_reason, CleanupReason::IdleDisabled);
    }

    #[tokio::test]
    async fn cleanup_preserves_work_and_rechecks_activity_before_deleting() {
        use crate::{
            daemon::{ports::PortRequest, resources::ResourceRequest},
            git,
            test_support::{commit, git as git_in, manager, repository},
        };
        let (temp, manager) = manager().await;
        let repository_dir = repository(temp.path(), "repo");
        fs::write(repository_dir.join(".gitignore"), "ignored/\n").unwrap();
        commit(&repository_dir, ".gitignore");
        let config = crate::config::Config::path(&manager.paths);
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(
            config,
            "[cleanup]\ngrace_minutes = 0\n[resources.test-lock]\n",
        )
        .unwrap();
        manager.reload_config().await.unwrap();
        let repo = manager
            .register_repository(repository_dir.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "idle".into(), None, None, None)
            .await
            .unwrap();
        let snapshot = |manager: &Arc<Manager>| {
            let manager = manager.clone();
            let id = workspace.id.clone();
            async move { manager.cleanup_snapshot(&id).await.unwrap() }
        };
        let lease =
            |pool: &str, mode: Option<crate::daemon::resources::LockMode>| ResourceRequest {
                mode,
                kind: None,
                pool: pool.into(),
                name: "default".into(),
                resource: None,
                reason: None,
                queue: false,
            };
        fs::write(workspace.path.join("work"), "landed later\n").unwrap();
        commit(&workspace.path, "work");
        assert!(
            snapshot(&manager).await.is_none(),
            "unpushed work must be retained"
        );
        // Work on the local default branch is retained without any remote.
        git_in(&repository_dir, &["merge", "--ff-only", "idle"]);
        manager
            .acquire_resource(&workspace.id, lease("test-lock", None), None)
            .await
            .unwrap();
        assert!(
            snapshot(&manager).await.is_none(),
            "a resource lease must prevent automatic removal"
        );
        manager
            .release_resource(&workspace.id, "test-lock".into(), "default".into())
            .await
            .unwrap();
        let rwlock_config = workspace.path.join(".shoal.toml");
        fs::write(&rwlock_config, "[resources.cache]\nkind='rwlock'\n").unwrap();
        for mode in [
            crate::daemon::resources::LockMode::Read,
            crate::daemon::resources::LockMode::Write,
        ] {
            manager
                .acquire_resource(&workspace.id, lease("cache", Some(mode)), None)
                .await
                .unwrap();
            // Remove config so only the lease can keep this clean/pushed worktree alive.
            fs::remove_file(&rwlock_config).unwrap();
            assert!(snapshot(&manager).await.is_none());
            manager
                .release_resource(&workspace.id, "cache".into(), "default".into())
                .await
                .unwrap();
            fs::write(&rwlock_config, "[resources.cache]\nkind='rwlock'\n").unwrap();
        }
        fs::remove_file(&rwlock_config).unwrap();
        let original = snapshot(&manager).await.unwrap();
        manager
            .acquire_port(
                &workspace.id,
                "web".into(),
                PortRequest {
                    reason: Some("cleanup test".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        fs::write(workspace.path.join("dirty"), "retain me").unwrap();
        assert!(snapshot(&manager).await.is_none());
        assert!(manager.remove_idle(&workspace.id, original).await.is_err());
        fs::remove_file(workspace.path.join("dirty")).unwrap();
        let before_command = snapshot(&manager).await.unwrap();
        let started = manager
            .begin_execution(
                &workspace.id,
                None,
                crate::daemon::workspace::ExecutionKind::Command,
                None,
            )
            .await
            .unwrap();
        assert!(snapshot(&manager).await.is_none());
        manager
            .finish_execution(
                started.plan.id,
                crate::daemon::workspace::ExecutionKind::Command,
                Some(0),
            )
            .await
            .unwrap();
        let after_command = snapshot(&manager).await.unwrap();
        assert_ne!(
            before_command, after_command,
            "short executions must reset the timer"
        );
        fs::create_dir(workspace.path.join("ignored")).unwrap();
        fs::write(workspace.path.join("ignored/build-output"), "build").unwrap();
        assert!(
            manager
                .remove_idle(&workspace.id, after_command)
                .await
                .is_err(),
            "ignored file updates must reset the timer"
        );
        let final_snapshot = snapshot(&manager).await.unwrap();
        // The repository's own idle delay applies before the global default.
        manager
            .set_repository_config(&repo.id, Some("[cleanup.auto]\nidle_minutes = 60\n".into()))
            .await
            .unwrap();
        let idle_for = |seconds| Idle {
            snapshot: final_snapshot,
            since: Instant::now() - Duration::from_secs(seconds),
        };
        let mut timers = Timers::default();
        timers.idle.insert(workspace.id.clone(), idle_for(600));
        sweep(&manager, &mut timers).await.unwrap();
        assert!(
            workspace.path.exists(),
            "the repository's hour outlasts the global ten minutes"
        );
        timers.idle.insert(workspace.id.clone(), idle_for(3600));
        sweep(&manager, &mut timers).await.unwrap();
        assert!(!workspace.path.exists());
        assert!(manager.list_workspaces().await.unwrap().is_empty());
        assert!(manager.list_ports(None).await.unwrap().is_empty());
        assert_eq!(
            git::run(
                &repository_dir,
                &[
                    "for-each-ref",
                    "--format=%(refname)",
                    &format!("refs/heads/{}", workspace.branch)
                ],
            )
            .await
            .unwrap(),
            ""
        );
    }

    #[tokio::test]
    async fn requested_cleanup_removes_candidates_without_idle_delay() {
        use crate::test_support::{manager, repository};
        let (temp, manager) = manager().await;
        let checkout = repository(temp.path(), "repo");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        // The request applies where idle cleanup is disabled.
        manager
            .set_repository_config(&repo.id, Some("[cleanup.auto]\nenabled = false\n".into()))
            .await
            .unwrap();
        for name in ["broken", "dirty", "held", "idle"] {
            manager
                .create_workspace(&repo.id, name.into(), None, None, None)
                .await
                .unwrap();
        }
        // An inspection failure is reported rather than read as nothing to do.
        let broken = manager.workspace("broken").await.unwrap();
        fs::remove_file(broken.path.join(".git")).unwrap();
        let dirty = manager.workspace("dirty").await.unwrap();
        fs::write(dirty.path.join("work"), "retain me").unwrap();
        let held = manager.workspace("held").await.unwrap();
        manager
            .acquire_hold(&held.id, "keep".into(), None)
            .await
            .unwrap();
        let names = |manager: &Arc<Manager>| {
            let manager = manager.clone();
            async move {
                manager
                    .list_workspaces()
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|workspace| workspace.name)
                    .collect::<Vec<_>>()
            }
        };

        let retained = |report: &ManualCleanup| {
            report
                .retained
                .iter()
                .map(|retained| retained.workspace.clone())
                .collect::<Vec<_>>()
        };

        let preview = on_request(&manager, true, false).await.unwrap();
        assert_eq!(preview.removed, ["idle"]);
        assert_eq!(retained(&preview), ["broken"]);
        let kept = preview
            .kept
            .iter()
            .map(|kept| (kept.workspace.as_str(), kept.reason.clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            kept,
            [
                ("dirty", CleanupReason::Dirty),
                ("held", CleanupReason::Held)
            ]
        );
        assert_eq!(names(&manager).await, ["broken", "dirty", "held", "idle"]);

        let report = on_request(&manager, false, false).await.unwrap();
        assert!(!report.dry_run);
        assert_eq!(report.removed, ["idle"]);
        assert_eq!(retained(&report), ["broken"]);
        assert!(
            report.kept.is_empty(),
            "only a dry run lists kept workspaces"
        );
        assert_eq!(names(&manager).await, ["broken", "dirty", "held"]);
    }

    #[tokio::test]
    async fn holds_block_idle_sweeps_and_release_restores_eligibility() {
        use crate::test_support::{git, manager, repository};
        let (temp, manager) = manager().await;
        let checkout = repository(temp.path(), "repo");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "held".into(), None, None, None)
            .await
            .unwrap();
        let snapshot = manager
            .cleanup_snapshot(&workspace.id)
            .await
            .unwrap()
            .unwrap();
        let mut timers = Timers::default();
        timers.idle.insert(
            workspace.id.clone(),
            Idle {
                snapshot,
                since: Instant::now() - Duration::from_secs(3600),
            },
        );
        for name in ["app-one", "app-two"] {
            manager
                .acquire_hold(&workspace.id, name.into(), None)
                .await
                .unwrap();
        }
        sweep(&manager, &mut timers).await.unwrap();
        assert!(workspace.path.exists());
        assert!(!git(&checkout, &["branch", "--list", "held"]).is_empty());
        assert!(!timers.idle.contains_key(&workspace.id));
        manager
            .release_hold(&workspace.id, "app-one".into())
            .await
            .unwrap();
        sweep(&manager, &mut timers).await.unwrap();
        assert!(
            manager
                .cleanup_snapshot(&workspace.id)
                .await
                .unwrap()
                .is_none()
        );
        manager
            .release_hold(&workspace.id, "app-two".into())
            .await
            .unwrap();
        let snapshot = manager
            .cleanup_snapshot(&workspace.id)
            .await
            .unwrap()
            .unwrap();
        timers.idle.insert(
            workspace.id.clone(),
            Idle {
                snapshot,
                since: Instant::now() - Duration::from_secs(3600),
            },
        );
        sweep(&manager, &mut timers).await.unwrap();
        assert!(!workspace.path.exists());
        assert!(git(&checkout, &["branch", "--list", "held"]).is_empty());
        assert!(!manager.has_holds(&workspace.id).await.unwrap());
    }

    #[tokio::test]
    async fn ignored_changes_during_idle_delay_restart_timer_at_expiry() {
        use crate::test_support::{commit, manager, repository};
        let (temp, manager) = manager().await;
        let checkout = repository(temp.path(), "repo");
        fs::write(checkout.join(".gitignore"), "ignored/\n").unwrap();
        commit(&checkout, ".gitignore");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "idle".into(), None, None, None)
            .await
            .unwrap();
        let mut timers = Timers::default();
        let expire = |timers: &mut Timers| {
            timers.idle.get_mut(&workspace.id).unwrap().since -= Duration::from_secs(600);
        };
        sweep(&manager, &mut timers).await.unwrap();
        let started = timers.idle[&workspace.id].snapshot;
        fs::create_dir(workspace.path.join("ignored")).unwrap();
        fs::write(workspace.path.join("ignored/build-output"), "build").unwrap();
        // Passes before expiry compare only HEAD and activity.
        sweep(&manager, &mut timers).await.unwrap();
        assert_eq!(timers.idle[&workspace.id].snapshot, started);
        expire(&mut timers);
        sweep(&manager, &mut timers).await.unwrap();
        assert!(
            workspace.path.exists(),
            "the ignored change restarts the timer"
        );
        assert_ne!(timers.idle[&workspace.id].snapshot, started);
        expire(&mut timers);
        sweep(&manager, &mut timers).await.unwrap();
        assert!(!workspace.path.exists());
    }
}
