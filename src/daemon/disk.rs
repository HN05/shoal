//! Keep free space on the filesystems holding workspaces and daemon state by
//! removing workspaces idle cleanup would remove, without their idle delay,
//! then stopping tracked executions when that cannot free enough space.
use anyhow::Result;
use std::{
    io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::time::Instant;

use super::{
    events::EventCause,
    notifications::NotificationKind,
    workspace::{Manager, StopRecords},
};
use crate::{config::overload::Disk, model::Workspace};

/// Each pass inspects every workspace, so a pass that could not free enough
/// space repeats at the idle sweep's pace.
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
pub(super) struct Monitor {
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
    pub(super) async fn check(
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
        let mut low = sample(&manager.paths.state, &workspaces, &available);
        low.retain(|filesystem| filesystem.free < settings.cleanup_free_bytes());
        if self
            .last_cleanup
            .is_none_or(|last| last.elapsed() >= CLEANUP_INTERVAL)
        {
            for filesystem in &mut low {
                free_space(manager, settings, &workspaces, filesystem, &available).await?;
            }
            let exhausted = low
                .iter()
                .any(|filesystem| filesystem.free < settings.cleanup_free_bytes());
            self.last_cleanup = exhausted.then(Instant::now);
        }
        if let Some(critical) = low
            .iter()
            .find(|filesystem| filesystem.free < settings.stop_free_bytes())
        {
            stop_executions(manager, critical).await?;
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

/// Stop every workspace's tracked executions as `shoal stop` does, saving
/// what `shoal resume` restores. Executions started while space stays this
/// low stop on the next check.
async fn stop_executions(manager: &Manager, filesystem: &Filesystem) -> Result<()> {
    let workspaces: Vec<String> = manager
        .store
        .run(|db| {
            Ok(db
                .prepare("SELECT DISTINCT workspace_id FROM executions")?
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await?;
    let stops = workspaces.into_iter().map(|id| async move {
        let workspace = manager.workspace(&id).await?;
        let (kind, message) = match manager.stop_workspace(&id, StopRecords::Save).await {
            Ok(()) => {
                eprintln!("disk space protection stopped {}", workspace.name);
                (
                    NotificationKind::AgentStopped,
                    format!(
                        "Stopped tracked executions: {}; free disk space, then restore with shoal resume {}",
                        filesystem.describe(),
                        workspace.name
                    ),
                )
            }
            Err(error) => {
                eprintln!("disk space protection could not stop {}: {error:#}", workspace.name);
                (
                    NotificationKind::StopFailed,
                    format!(
                        "Could not stop tracked executions: {}: {error:#}",
                        filesystem.describe()
                    ),
                )
            }
        };
        manager.notify(Some(&workspace.name), kind, message).await;
        anyhow::Ok(())
    });
    for result in futures_util::future::join_all(stops).await {
        if let Err(error) = result {
            eprintln!("disk space protection: {error:#}");
        }
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
        // That pass freed enough space, so the next low reading starts another.
        // Space above the stop threshold stops nothing.
        let above_stop = |_: &Path| Ok(manager.config().overload.disk.stop_free_bytes());
        monitor.check(&manager, above_stop).await.unwrap();
        assert_eq!(names(manager.list_workspaces().await.unwrap()), ["first"]);
        // A pass that could not free enough space waits for the interval.
        manager.release_hold(&held.id, "keep".into()).await.unwrap();
        monitor.check(&manager, above_stop).await.unwrap();
        assert_eq!(names(manager.list_workspaces().await.unwrap()), ["first"]);
        Monitor::default()
            .check(&manager, above_stop)
            .await
            .unwrap();
        assert!(manager.list_workspaces().await.unwrap().is_empty());

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
