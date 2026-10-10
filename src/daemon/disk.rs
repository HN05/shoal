//! Keep free space on the filesystems holding workspaces and daemon state by
//! removing workspaces idle cleanup would remove, without their idle delay,
//! then stopping tracked executions when that cannot free enough space.
use anyhow::Result;
use std::{
    io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio::time::Instant;

use super::{
    events::EventCause,
    notifications::NotificationKind,
    workspace::{Manager, Protection},
};
use crate::daemon::log;
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
            .check(&manager, Instant::now(), crate::fsutil::available_bytes)
            .await
        {
            manager.publish_disk_reading(None);
            log!("disk space monitor: {error:#}");
        }
    }
}

impl Monitor {
    pub(super) async fn check(
        &mut self,
        manager: &Manager,
        now: Instant,
        available: impl Fn(&Path) -> io::Result<u64>,
    ) -> Result<()> {
        let config = manager.config();
        let settings = &config.overload.disk;
        if !settings.enabled {
            // Re-enabling waits for a fresh reading before recovery.
            manager.publish_disk_reading(None);
            return Ok(());
        }
        let workspaces = manager.list_workspaces().await?;
        let (mut low, complete) = sample(&manager.paths.state, &workspaces, &available);
        // A failed reading prevents agent recovery.
        let recovered = complete
            && low
                .iter()
                .all(|filesystem| filesystem.free >= settings.cleanup_free_bytes());
        manager.publish_disk_reading(recovered.then(|| settings.cleanup_free_bytes()));
        low.retain(|filesystem| filesystem.free < settings.cleanup_free_bytes());
        if self
            .last_cleanup
            .is_none_or(|last| now.duration_since(last) >= CLEANUP_INTERVAL)
        {
            for filesystem in &mut low {
                free_space(manager, settings, &workspaces, filesystem, &available).await?;
            }
            let exhausted = low
                .iter()
                .any(|filesystem| filesystem.free < settings.cleanup_free_bytes());
            self.last_cleanup = exhausted.then_some(now);
        }
        if let Some(critical) = low
            .iter()
            .find(|filesystem| filesystem.free < settings.stop_free_bytes())
        {
            stop_executions(manager, settings, critical).await?;
        }
        Ok(())
    }
}

impl Manager {
    fn publish_disk_reading(&self, recovered_at: Option<u64>) {
        self.disk_recovered_at
            .store(recovered_at.unwrap_or(0), Ordering::Relaxed);
    }

    /// Whether the latest reading found `cleanup_free_bytes` free everywhere.
    /// A reading taken under a lower threshold, before a reload, does not count.
    pub(super) fn disk_space_recovered(&self, cleanup_free_bytes: u64) -> bool {
        let recovered_at = self.disk_recovered_at.load(Ordering::Relaxed);
        recovered_at != 0 && recovered_at >= cleanup_free_bytes
    }
}

/// One reading per filesystem holding daemon state or a workspace, and whether
/// every reading succeeded. A failed reading never authorizes removal, so that
/// filesystem is skipped. A deleted workspace awaits cleanup and is skipped
/// without failing the sample; daemon state must be readable.
fn sample(
    state: &Path,
    workspaces: &[Workspace],
    available: impl Fn(&Path) -> io::Result<u64>,
) -> (Vec<Filesystem>, bool) {
    let mut filesystems: Vec<Filesystem> = Vec::new();
    let mut complete = true;
    let paths = std::iter::once(state).chain(workspaces.iter().map(|w| w.path.as_path()));
    for (index, path) in paths.enumerate() {
        let device = match path.metadata() {
            Ok(metadata) => metadata.dev(),
            Err(error) => {
                if index == 0 || error.kind() != io::ErrorKind::NotFound {
                    log!("disk space reading {}: {error}", path.display());
                    complete = false;
                }
                continue;
            }
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
            Err(error) => {
                log!("disk space reading {}: {error}", path.display());
                complete = false;
            }
        }
    }
    (filesystems, complete)
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
                log!("disk space cleanup removed {}", workspace.name);
                (
                    NotificationKind::WorkspaceRemoved,
                    format!("removed by disk space cleanup; {}", filesystem.describe()),
                )
            }
            Err(error) => {
                manager
                    .record_retained(&workspace.id, EventCause::DiskSpace, &error)
                    .await?;
                log!("disk space cleanup retained {}: {error:#}", workspace.name);
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

/// Stop every running agent through protection, so it restores once free
/// space recovers, then the remaining executions as `shoal stop` does, saving
/// what `shoal resume` restores. Executions started while space stays this
/// low stop on the next check.
async fn stop_executions(
    manager: &Manager,
    settings: &Disk,
    filesystem: &Filesystem,
) -> Result<()> {
    let low = format!(
        "free disk space is below {} GiB at {} ({:.1} GiB free)",
        settings.stop_free_gib,
        filesystem.path.display(),
        filesystem.free as f64 / f64::from(1 << 30)
    );
    let protection = Protection {
        reason: format!("{low}; all running agents stopped"),
        resumes_when: format!("free disk space reaches {} GiB", settings.cleanup_free_gib),
    };
    for agent in manager.stop_agents_for_disk_space(&protection).await {
        log!(
            "disk space protection stopped {} in {}: {low}",
            agent.name,
            agent.workspace
        );
    }
    let executions: Vec<(String, String)> = manager
        .store
        .run(|db| {
            Ok(db
                .prepare("SELECT id, workspace_id FROM executions")?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await?;
    let mut workspaces = Vec::new();
    for (execution, workspace) in executions {
        if !manager.agent_was_overloaded(&execution).await && !workspaces.contains(&workspace) {
            workspaces.push(workspace);
        }
    }
    let (low, reason) = (&low, &protection.reason);
    let stops = workspaces.into_iter().map(|id| async move {
        let workspace = manager.workspace(&id).await?;
        let (kind, message) = match manager.stop_unprotected(&id, reason.clone()).await {
            Ok(()) => {
                log!("disk space protection stopped commands in {}: {low}", workspace.name);
                (
                    NotificationKind::AgentStopped,
                    format!(
                        "Stopped tracked commands: {low}; free disk space, then shoal resume {} lists them",
                        workspace.name
                    ),
                )
            }
            Err(error) => {
                log!("disk space protection could not stop {}: {error:#}", workspace.name);
                (
                    NotificationKind::StopFailed,
                    // Repeated failures collapse, so the message omits free space.
                    format!(
                        "Could not stop tracked executions while disk space is critical at {}: {error:#}",
                        filesystem.path.display()
                    ),
                )
            }
        };
        manager.notify(Some(&workspace.name), kind, message).await;
        anyhow::Ok(())
    });
    for result in futures_util::future::join_all(stops).await {
        if let Err(error) = result {
            log!("disk space protection: {error:#}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{manager, repository};
    use std::sync::atomic::AtomicUsize;

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
        let start = Instant::now();
        let mut monitor = Monitor::default();
        monitor.check(&manager, start, &available).await.unwrap();
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
        monitor.check(&manager, start, above_stop).await.unwrap();
        assert_eq!(names(manager.list_workspaces().await.unwrap()), ["first"]);
        // A pass that could not free enough space waits for the interval.
        manager.release_hold(&held.id, "keep".into()).await.unwrap();
        monitor.check(&manager, start, above_stop).await.unwrap();
        assert_eq!(names(manager.list_workspaces().await.unwrap()), ["first"]);
        monitor
            .check(&manager, start + CLEANUP_INTERVAL, above_stop)
            .await
            .unwrap();
        assert!(manager.list_workspaces().await.unwrap().is_empty());

        // Agents recover only once every filesystem has the cleanup threshold.
        let cleanup = manager.config().overload.disk.cleanup_free_bytes();
        let recovered = |manager: &Manager| manager.disk_space_recovered(cleanup);
        monitor
            .check(&manager, start, |_: &Path| Ok(cleanup - 1))
            .await
            .unwrap();
        assert!(!recovered(&manager));
        monitor
            .check(&manager, start, |_: &Path| Ok(cleanup))
            .await
            .unwrap();
        assert!(recovered(&manager));
        // A reading under a lower threshold does not satisfy a raised one.
        assert!(!manager.disk_space_recovered(cleanup + 1));
        monitor
            .check(&manager, start, |_: &Path| {
                Err(io::Error::other("unreadable"))
            })
            .await
            .unwrap();
        assert!(!recovered(&manager));

        // Unreadable daemon state is never a healthy reading, and only a
        // deleted workspace is skipped without failing the sample.
        let unreadable = |path: &Path| sample(path, &[], |_: &Path| Ok(u64::MAX));
        let (filesystems, complete) = unreadable(&root.path().join("missing"));
        assert!(filesystems.is_empty() && !complete);
        let mut workspace = held.clone();
        workspace.path = root.path().join("deleted");
        let (_, complete) = sample(root.path(), &[workspace.clone()], |_: &Path| Ok(u64::MAX));
        assert!(complete);
        let file = root.path().join("file");
        std::fs::write(&file, "").unwrap();
        workspace.path = file.join("below-a-file");
        let (_, complete) = sample(root.path(), &[workspace], |_: &Path| Ok(u64::MAX));
        assert!(!complete);

        // Disabled protection reads nothing and forgets its last reading.
        manager.publish_disk_reading(Some(cleanup));
        let config = crate::config::Config::path(&manager.paths);
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "[overload.disk]\nenabled = false\n").unwrap();
        manager.reload_config().await.unwrap();
        Monitor::default()
            .check(&manager, start, |_: &Path| -> io::Result<u64> {
                panic!("disabled disk protection sampled free space")
            })
            .await
            .unwrap();
        assert!(!recovered(&manager));
    }
}
