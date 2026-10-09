//! Automatic removal of idle, clean, fully pushed worktrees, and of workspaces
//! whose directory was deleted outside Shoal.
use anyhow::{Result, ensure};
use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    path::Path,
    sync::{Arc, PoisonError},
    time::Duration,
};
use tokio::time::{Instant, sleep};

use crate::{
    daemon::{
        doctor::{Check, CheckStatus},
        events::EventCause,
        log,
        notifications::NotificationKind,
        workspace::Manager,
    },
    model::Workspace,
    state::WorkspaceState,
    time::{local_seconds, unix_seconds},
};

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

/// When a workspace was last seen unchanged in a removable state.
struct Idle {
    snapshot: u64,
    since: Instant,
}

#[derive(Default)]
pub struct Timers {
    idle: HashMap<String, Idle>,
}

impl Timers {
    /// Record the latest snapshot; returns true once it has stayed the same
    /// for `delay`.
    pub fn observe(&mut self, id: &str, snapshot: u64, now: Instant, delay: Duration) -> bool {
        let entry = self.idle.entry(id.into()).or_insert(Idle {
            snapshot,
            since: now,
        });
        if entry.snapshot != snapshot {
            *entry = Idle {
                snapshot,
                since: now,
            };
        }
        now.duration_since(entry.since) >= delay
    }
}

/// Metadata only, including ignored files; never follow symlinks out of the tree.
/// Git administrative storage is shared and inspected separately through HEAD.
pub fn fingerprint(root: &Path, head: &str, activity: u64) -> Result<u64> {
    fn visit(path: &Path, hash: &mut DefaultHasher) -> Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        path.hash(hash);
        metadata.len().hash(hash);
        metadata.modified()?.hash(hash);
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
    head.hash(&mut hash);
    activity.hash(&mut hash);
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
    step("branch observation", observe_branches(manager).await);
    step("issue cleanup", manager.sweep_issues().await);
    step("completion cleanup", manager.sweep_completed().await);
    step("PR cleanup", manager.sweep_prs().await);
    step("idle cleanup", sweep_idle(manager, timers).await);
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
    let Some((delay, snapshot)) = observe(manager, workspace).await else {
        timers.idle.remove(&workspace.id);
        return Ok(());
    };
    if !timers.observe(&workspace.id, snapshot, Instant::now(), delay) {
        return Ok(());
    }
    timers.idle.remove(&workspace.id);
    let (kind, message, recorded) = match manager.remove_idle(&workspace.id, snapshot).await {
        Ok(()) => {
            log!("auto cleanup removed {}", workspace.name);
            (
                NotificationKind::WorkspaceRemoved,
                "removed by idle cleanup".to_owned(),
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

/// The idle delay and snapshot of a ready, removable workspace; `None` when
/// it is not, its idle cleanup is disabled, or its config cannot be read.
pub(super) async fn observe(manager: &Manager, workspace: &Workspace) -> Option<(Duration, u64)> {
    if workspace.state != WorkspaceState::Ready {
        return None;
    }
    let observed = async {
        let settings = manager.workspace_settings(workspace).await?;
        let Some(delay) = settings.auto_cleanup.delay() else {
            return Ok(None);
        };
        let snapshot = manager.cleanup_snapshot(&workspace.id).await?;
        Ok(snapshot.map(|snapshot| (delay, snapshot)))
    }
    .await;
    observed.unwrap_or_else(|error: anyhow::Error| {
        log!("auto cleanup skipped {}: {error:#}", workspace.name);
        None
    })
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
    fn activity_restarts_timer_and_unsafe_state_cancels_it() {
        let mut timers = Timers::default();
        let now = Instant::now();
        let delay = Duration::from_secs(600);
        assert!(!timers.observe("w", 1, now, delay));
        assert!(!timers.observe("w", 2, now + delay, delay));
        timers.idle.remove("w");
        assert!(!timers.observe("w", 2, now + delay * 3, delay));
        assert!(timers.observe("w", 2, now + delay * 4, delay));
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

        let failed = Err(anyhow::anyhow!("issue cleanup: disk is full"));
        assert_eq!(
            health.record(moment(90, 3600), &failed).as_deref(),
            Some("issue cleanup: disk is full")
        );
        assert_eq!(
            health.record(moment(120, 3600), &failed).as_deref(),
            Some("issue cleanup: disk is full"),
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
        assert!(check.message.ends_with("issue cleanup: disk is full"));
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
        let linked = manager
            .create_workspace(&repo.id, "linked".into(), None, None, None)
            .await
            .unwrap();
        let idle = manager
            .create_workspace(&repo.id, "idle".into(), None, None, None)
            .await
            .unwrap();
        let id = linked.id.clone();
        manager
            .store
            .run(move |db| {
                db.execute(
                    "INSERT INTO workspace_issue(workspace_id,url) VALUES (?1,'issue')",
                    [id],
                )?;
                db.execute_batch(
                    "CREATE TEMP TRIGGER full_disk BEFORE UPDATE ON workspace_issue
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
            error.starts_with("issue cleanup: ") && error.contains("disk is full"),
            "{error}"
        );
        assert!(!idle.path.exists());
        assert!(linked.path.exists());
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
        fs::write(config, "[resources.test-lock]\n").unwrap();
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
            .set_repository_config(&repo.id, Some("[auto_cleanup]\nidle_minutes = 60\n".into()))
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
}
