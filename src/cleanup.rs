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

use crate::workspace::Manager;

#[derive(Default)]
pub struct Timers {
    idle: HashMap<String, (u64, Instant)>,
}

impl Timers {
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
        let entry = self.idle.entry(id.into()).or_insert((snapshot, now));
        if entry.0 != snapshot {
            *entry = (snapshot, now);
        }
        now.duration_since(entry.1) >= delay
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

pub async fn sweep(manager: &Manager, timers: &mut Timers, delay: Duration) -> Result<()> {
    let workspaces = manager.list().await?;
    timers
        .idle
        .retain(|id, _| workspaces.iter().any(|workspace| &workspace.id == id));
    for workspace in workspaces {
        let snapshot = if workspace.state == "ready" {
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
                match manager.remove_idle(workspace.id.clone(), snapshot).await {
                    Ok(()) => eprintln!("auto cleanup removed {}", workspace.name),
                    Err(error) => eprintln!("auto cleanup retained {}: {error:#}", workspace.name),
                }
            }
            timers.idle.remove(&workspace.id);
        }
    }
    Ok(())
}

pub async fn run(manager: Arc<Manager>, delay: Duration) {
    let mut timers = Timers::default();
    loop {
        if let Err(error) = sweep(&manager, &mut timers, delay).await {
            eprintln!("auto cleanup: {error:#}");
        }
        sleep(Duration::from_secs(30)).await;
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
        use crate::{paths::Paths, worktrunk};
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let repository_dir = temp.path().join("repo");
        fs::create_dir(&repository_dir).unwrap();
        let git = |args: &[&str]| {
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
        git(&["init", "-b", "main"]);
        fs::write(repository_dir.join(".gitignore"), "ignored/\n").unwrap();
        git(&["add", "."]);
        git(&[
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
        let manager = Manager::open(paths).await.unwrap();
        let repo = manager
            .register(repository_dir.to_str().unwrap().into(), None)
            .await
            .unwrap();
        let workspace = manager.add(repo.id, "idle".into(), None).await.unwrap();
        assert!(
            manager
                .cleanup_snapshot(&workspace.id)
                .await
                .unwrap()
                .is_none(),
            "unpushed work must be retained"
        );
        git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let original = manager
            .cleanup_snapshot(&workspace.id)
            .await
            .unwrap()
            .unwrap();
        manager
            .reserve_port(
                workspace.id.clone(),
                "web".into(),
                crate::ports::PortOptions {
                    reason: Some("cleanup test".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        fs::write(workspace.path.join("dirty"), "retain me").unwrap();
        assert!(
            manager
                .cleanup_snapshot(&workspace.id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            manager
                .remove_idle(workspace.id.clone(), original)
                .await
                .is_err()
        );
        fs::remove_file(workspace.path.join("dirty")).unwrap();
        let before_command = manager
            .cleanup_snapshot(&workspace.id)
            .await
            .unwrap()
            .unwrap();
        let (plan, _) = manager.begin(workspace.id.clone()).await.unwrap();
        assert!(
            manager
                .cleanup_snapshot(&workspace.id)
                .await
                .unwrap()
                .is_none()
        );
        manager.finish(plan.id, true).await.unwrap();
        let after_command = manager
            .cleanup_snapshot(&workspace.id)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(
            before_command, after_command,
            "short executions must reset the timer"
        );
        fs::create_dir(workspace.path.join("ignored")).unwrap();
        fs::write(workspace.path.join("ignored/build-output"), "build").unwrap();
        assert!(
            manager
                .remove_idle(workspace.id.clone(), after_command)
                .await
                .is_err(),
            "ignored file updates must reset the timer"
        );
        let snapshot = manager
            .cleanup_snapshot(&workspace.id)
            .await
            .unwrap()
            .unwrap();
        let mut timers = Timers::default();
        timers.idle.insert(
            workspace.id.clone(),
            (snapshot, Instant::now() - Duration::from_secs(600)),
        );
        sweep(&manager, &mut timers, Duration::from_secs(600))
            .await
            .unwrap();
        assert!(!workspace.path.exists());
        assert!(manager.list().await.unwrap().is_empty());
        assert!(manager.list_ports(None).await.unwrap().is_empty());
        assert_eq!(
            worktrunk::git(
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
