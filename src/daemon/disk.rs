//! Keep free space on the filesystems holding workspaces and daemon state by
//! removing workspaces idle cleanup would remove, without their idle delay.
use anyhow::Result;
use std::{
    io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::time::Instant;

use super::{events::EventCause, notifications::NotificationKind, workspace::Manager};
use crate::{config::overload::Disk, model::Workspace};

/// Each pass inspects every workspace, so passes run at the idle sweep's pace.
const CLEANUP_INTERVAL: Duration = Duration::from_secs(30);

/// A sampled filesystem, named by the first monitored path on it.
struct Filesystem {
    device: u64,
    path: PathBuf,
    free: u64,
}

impl Filesystem {
    fn describe(&self) -> String {
        format!(
            "{:.1} GiB available at {}",
            self.free as f64 / f64::from(1 << 30),
            self.path.display()
        )
    }
}

#[derive(Default)]
struct Monitor {
    last_cleanup: Option<Instant>,
}

pub(super) async fn run(manager: Arc<Manager>) {
    let mut monitor = Monitor::default();
    loop {
        let poll = manager.config().overload.poll_seconds;
        tokio::time::sleep(Duration::from_secs(poll)).await;
        let _operation = manager.background_operations.read().await;
        if let Err(error) = monitor
            .check(&manager, crate::fsutil::available_bytes)
            .await
        {
            eprintln!("disk space monitor: {error:#}");
        }
    }
}

impl Monitor {
    async fn check(
        &mut self,
        manager: &Manager,
        available: impl Fn(&Path) -> io::Result<u64>,
    ) -> Result<()> {
        let config = manager.config();
        let settings = &config.overload.disk;
        if !settings.enabled {
            return Ok(());
        }
        let workspaces = manager.list_workspaces().await?;
        let low = sample(&manager.paths.state, &workspaces, &available)
            .into_iter()
            .filter(|filesystem| filesystem.free < settings.cleanup_free_bytes());
        let due = self
            .last_cleanup
            .is_none_or(|last| last.elapsed() >= CLEANUP_INTERVAL);
        if !due {
            return Ok(());
        }
        self.last_cleanup = Some(Instant::now());
        for mut filesystem in low {
            free_space(manager, settings, &workspaces, &mut filesystem, &available).await?;
        }
        Ok(())
    }
}

/// One reading per filesystem holding daemon state or a workspace. A failed
/// reading never authorizes removal, so that filesystem is skipped.
fn sample(
    state: &Path,
    workspaces: &[Workspace],
    available: impl Fn(&Path) -> io::Result<u64>,
) -> Vec<Filesystem> {
    let mut filesystems: Vec<Filesystem> = Vec::new();
    let paths = std::iter::once(state).chain(workspaces.iter().map(|w| w.path.as_path()));
    for path in paths {
        let Ok(device) = path.metadata().map(|metadata| metadata.dev()) else {
            continue;
        };
        if filesystems.iter().any(|f| f.device == device) {
            continue;
        }
        match available(path) {
            Ok(free) => filesystems.push(Filesystem {
                device,
                path: path.to_owned(),
                free,
            }),
            Err(error) => eprintln!("disk space reading {}: {error}", path.display()),
        }
    }
    filesystems
}

/// Remove cleanup candidates on the filesystem until it has enough space.
async fn free_space(
    manager: &Manager,
    settings: &Disk,
    workspaces: &[Workspace],
    filesystem: &mut Filesystem,
    available: impl Fn(&Path) -> io::Result<u64>,
) -> Result<()> {
    for workspace in workspaces {
        if filesystem.free >= settings.cleanup_free_bytes() {
            break;
        }
        if workspace
            .path
            .metadata()
            .map(|metadata| metadata.dev())
            .ok()
            != Some(filesystem.device)
        {
            continue;
        }
        let Some((_, snapshot)) = super::cleanup::observe(manager, workspace).await else {
            continue;
        };
        let (kind, message) = match manager.remove_for_disk_space(&workspace.id, snapshot).await {
            Ok(()) => {
                eprintln!("disk space cleanup removed {}", workspace.name);
                (
                    NotificationKind::WorkspaceRemoved,
                    format!("removed by disk space cleanup; {}", filesystem.describe()),
                )
            }
            Err(error) => {
                manager
                    .record_retained(&workspace.id, EventCause::DiskSpace, &error)
                    .await?;
                eprintln!("disk space cleanup retained {}: {error:#}", workspace.name);
                (
                    NotificationKind::CleanupFailed,
                    format!("disk space cleanup retained the workspace: {error:#}"),
                )
            }
        };
        manager.notify(Some(&workspace.name), kind, message).await;
        filesystem.free = available(&filesystem.path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{manager, repository};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn cleanup_removes_candidates_until_enough_space_is_free() {
        let (root, manager) = manager().await;
        let checkout = repository(root.path(), "repo");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        for name in ["first", "second", "third"] {
            manager
                .create_workspace(&repo.id, name.into(), None, None, None)
                .await
                .unwrap();
        }
        let held = manager.workspace("first").await.unwrap();
        manager
            .acquire_hold(&held.id, "keep".into(), None)
            .await
            .unwrap();
        let readings = AtomicUsize::new(0);
        let low = manager.config().overload.disk.cleanup_free_bytes() - 1;
        let available = |_: &Path| {
            // The first removal frees enough space.
            Ok(if readings.fetch_add(1, Ordering::Relaxed) == 0 {
                low
            } else {
                u64::MAX
            })
        };
        let mut monitor = Monitor::default();
        monitor.check(&manager, &available).await.unwrap();
        let names = |workspaces: Vec<Workspace>| {
            workspaces
                .into_iter()
                .map(|workspace| workspace.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(manager.list_workspaces().await.unwrap()),
            ["first", "third"]
        );
        // A pass ran moments ago, so the next check waits for the interval.
        readings.store(0, Ordering::Relaxed);
        monitor.check(&manager, &available).await.unwrap();
        assert_eq!(manager.list_workspaces().await.unwrap().len(), 2);

        // Disabled protection reads nothing.
        let config = crate::config::Config::path(&manager.paths);
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "[overload.disk]\nenabled = false\n").unwrap();
        manager.reload_config().await.unwrap();
        Monitor::default()
            .check(&manager, |_: &Path| -> io::Result<u64> {
                panic!("disabled disk protection sampled free space")
            })
            .await
            .unwrap();
    }
}
