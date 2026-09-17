//! Automatic removal of idle, clean, fully pushed worktrees, and of workspaces
//! whose directory was deleted outside Shoal.
use anyhow::Result;
use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    path::Path,
    sync::Arc,
    time::Duration,
};
use tokio::time::{Instant, sleep};

use crate::{model::Workspace, state::WorkspaceState, workspace::Manager};

const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

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
    /// for `delay`. `None` means the workspace is not removable and resets it.
    pub fn observe(
        &mut self,
        id: &str,
        snapshot: Option<u64>,
        now: Instant,
        delay: Duration,
    ) -> bool {
        let Some(snapshot) = snapshot else {
            self.idle.remove(id);
            return false;
        };
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

/// `idle` is the delay before idle removal; `None` disables it, leaving only
/// deleted-directory cleanup.
pub async fn sweep(manager: &Manager, timers: &mut Timers, idle: Option<Duration>) -> Result<()> {
    let workspaces = manager.list_workspaces().await?;
    timers
        .idle
        .retain(|id, _| workspaces.iter().any(|workspace| &workspace.id == id));
    for workspace in workspaces {
        // Only a worktree that existed (identity recorded) can have been deleted;
        // a failed creation keeps its error until removed explicitly.
        if matches!(
            workspace.state,
            WorkspaceState::Ready | WorkspaceState::Failed
        ) && workspace.git_dir.is_some()
            && !workspace.path.try_exists()?
        {
            timers.idle.remove(&workspace.id);
            remove_deleted(manager, &workspace).await;
            continue;
        }
        let Some(delay) = idle else { continue };
        let snapshot = if workspace.state == WorkspaceState::Ready {
            match manager.cleanup_snapshot(&workspace.id).await {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    eprintln!("auto cleanup skipped {}: {error:#}", workspace.name);
                    None
                }
            }
        } else {
            None
        };
        if timers.observe(&workspace.id, snapshot, Instant::now(), delay) {
            if let Some(snapshot) = snapshot {
                match manager.remove_idle(&workspace.id, snapshot).await {
                    Ok(()) => eprintln!("auto cleanup removed {}", workspace.name),
                    Err(error) => eprintln!("auto cleanup retained {}: {error:#}", workspace.name),
                }
            }
            timers.idle.remove(&workspace.id);
        }
    }
    Ok(())
}

/// A deleted worktree is forgotten unless it was moved or still has commands
/// Shoal cannot verify; the branch is retained either way.
async fn remove_deleted(manager: &Manager, workspace: &Workspace) {
    match manager.missing_worktree(workspace).await {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(error) => {
            eprintln!("deleted worktree {} retained: {error:#}", workspace.name);
            return;
        }
    }
    match manager.remove_deleted(&workspace.id).await {
        Ok(_) => eprintln!("forgot deleted worktree {}", workspace.name),
        Err(error) => eprintln!("deleted worktree {} retained: {error:#}", workspace.name),
    }
}

pub async fn run(manager: Arc<Manager>, idle: Option<Duration>) {
    let mut timers = Timers::default();
    loop {
        if let Err(error) = sweep(&manager, &mut timers, idle).await {
            eprintln!("auto cleanup: {error:#}");
        }
        sleep(SWEEP_INTERVAL).await;
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
        assert!(!timers.observe("w", Some(1), now, delay));
        assert!(!timers.observe("w", Some(2), now + delay, delay));
        assert!(!timers.observe("w", None, now + delay * 2, delay));
        assert!(!timers.observe("w", Some(2), now + delay * 3, delay));
        assert!(timers.observe("w", Some(2), now + delay * 4, delay));
    }

    #[tokio::test]
    async fn cleanup_preserves_work_and_rechecks_activity_before_deleting() {
        use crate::{git, paths::Paths, ports::PortRequest, resources::ResourceRequest};
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let repository_dir = temp.path().join("repo");
        fs::create_dir(&repository_dir).unwrap();
        let git_cmd = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&repository_dir)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git_cmd(&["init", "-b", "main"]);
        fs::write(repository_dir.join(".gitignore"), "ignored/\n").unwrap();
        git_cmd(&["add", "."]);
        git_cmd(&[
            "-c",
            "user.name=Shoal Test",
            "-c",
            "user.email=shoal@example.invalid",
            "commit",
            "-m",
            "initial",
        ]);
        let paths = Paths {
            home: temp.path().into(),
            state: temp.path().join("state"),
            socket: temp.path().join("state/daemon.sock"),
        };
        let mut manager = Manager::open(paths).await.unwrap();
        std::sync::Arc::get_mut(&mut manager)
            .unwrap()
            .config
            .resources
            .insert(
                "test-lock".into(),
                crate::resources::ResourceConfig::default(),
            );
        let repo = manager
            .register_repository(repository_dir.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "idle".into(), None)
            .await
            .unwrap();
        let snapshot = |manager: &Arc<Manager>| {
            let manager = manager.clone();
            let id = workspace.id.clone();
            async move { manager.cleanup_snapshot(&id).await.unwrap() }
        };
        let lease = |pool: &str, mode: Option<crate::resources::LockMode>| ResourceRequest {
            mode,
            pool: pool.into(),
            name: "default".into(),
            resource: None,
            reason: None,
        };
        assert!(
            snapshot(&manager).await.is_none(),
            "unpushed work must be retained"
        );
        git_cmd(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        manager
            .acquire_resource(&workspace.id, lease("test-lock", None))
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
            crate::resources::LockMode::Read,
            crate::resources::LockMode::Write,
        ] {
            manager
                .acquire_resource(&workspace.id, lease("cache", Some(mode)))
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
            .reserve_port(
                &workspace.id,
                "web".into(),
                PortRequest {
                    reason: Some("cleanup test".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        fs::write(workspace.path.join("dirty"), "retain me").unwrap();
        assert!(snapshot(&manager).await.is_none());
        assert!(manager.remove_idle(&workspace.id, original).await.is_err());
        fs::remove_file(workspace.path.join("dirty")).unwrap();
        let before_command = snapshot(&manager).await.unwrap();
        let (plan, _) = manager
            .begin_execution(
                &workspace.id,
                None,
                crate::workspace::ExecutionKind::Command,
            )
            .await
            .unwrap();
        assert!(snapshot(&manager).await.is_none());
        manager
            .finish_execution(plan.id, crate::workspace::ExecutionKind::Command, Some(0))
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
        let mut timers = Timers::default();
        timers.idle.insert(
            workspace.id.clone(),
            Idle {
                snapshot: final_snapshot,
                since: Instant::now() - Duration::from_secs(600),
            },
        );
        sweep(&manager, &mut timers, Some(Duration::from_secs(600)))
            .await
            .unwrap();
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
}
