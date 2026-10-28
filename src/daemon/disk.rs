//! Keep free space on the filesystems holding workspaces and daemon state by
//! deleting Xcode data that removed workspaces left, then removing
//! workspaces idle cleanup would remove, without their idle delay,
//! then emptying shared caches nothing is using, then stopping tracked
//! executions when that cannot free enough space.
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
    caches::Emptying,
    events::EventCause,
    notifications::NotificationKind,
    warnings::{self, Signal, Warnings},
    workspace::{Manager, Protection},
};
use crate::daemon::log;
use crate::{
    caches::Layout,
    config::overload::Disk,
    model::{Repository, Workspace},
};

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
    /// Whether `path` exists on this filesystem.
    fn holds(&self, path: &Path) -> bool {
        path.metadata().map(|metadata| metadata.dev()).ok() == Some(self.device)
    }

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
    warnings: Warnings,
}

pub(super) async fn run(manager: Arc<Manager>) {
    let mut monitor = Monitor::default();
    loop {
        let poll = manager.config().overload.poll_seconds;
        tokio::time::sleep(Duration::from_secs(poll)).await;
        let _operation = manager.gates.operation().await;
        if let Err(error) = monitor
            .check(&manager, Instant::now(), crate::fsutil::available_bytes)
            .await
        {
            manager.config_state.publish_disk_reading(None);
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
        // Before the config: a reload in between makes this reading stale.
        let generation = manager.config_state.generation();
        let config = manager.config();
        let settings = &config.overload.disk;
        if !settings.enabled {
            // Re-enabling waits for a fresh reading before recovery.
            manager.config_state.publish_disk_reading(None);
            return Ok(());
        }
        let workspaces = manager.list_workspaces().await?;
        let (mut low, complete) = sample(&manager.paths.state, &workspaces, &available);
        // A failed reading prevents agent recovery.
        let recovered = complete
            && low
                .iter()
                .all(|filesystem| filesystem.free >= settings.cleanup_free_bytes());
        manager
            .config_state
            .publish_disk_reading(recovered.then_some(generation));
        if config.overload.warning.enabled
            && let Some(lowest) = low
                .iter()
                .filter(|filesystem| filesystem.free < config.overload.warning.disk_free_bytes())
                .min_by_key(|filesystem| filesystem.free)
        {
            let message = warnings::disk(&config.overload, &lowest.describe());
            self.warnings
                .warn(manager, now, Signal::Disk, &message)
                .await;
        }
        low.retain(|filesystem| filesystem.free < settings.cleanup_free_bytes());
        if self
            .last_cleanup
            .is_none_or(|last| now.duration_since(last) >= CLEANUP_INTERVAL)
        {
            let (readable, complete) =
                clean_up_all(manager, settings, &workspaces, low, &available).await;
            let exhausted = !complete
                || readable
                    .iter()
                    .any(|filesystem| filesystem.free < settings.cleanup_free_bytes());
            self.last_cleanup = exhausted.then_some(now);
            low = readable;
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

/// Clean up each low filesystem, keeping those with a current reading and
/// whether every one still has it. A filesystem whose reading failed stops
/// nothing, as in the sample.
async fn clean_up_all(
    manager: &Manager,
    settings: &Disk,
    workspaces: &[Workspace],
    low: Vec<Filesystem>,
    available: impl Fn(&Path) -> io::Result<u64>,
) -> (Vec<Filesystem>, bool) {
    let mut readable = Vec::with_capacity(low.len());
    let mut complete = true;
    for mut filesystem in low {
        if clean_up(manager, settings, workspaces, &mut filesystem, &available).await {
            readable.push(filesystem);
        } else {
            complete = false;
        }
    }
    (readable, complete)
}

/// Free space on the filesystem, from what loses nothing to idle workspaces
/// and unused caches, and whether its reading is current. A failed step ends
/// the pass with a fresh reading, so a cleanup failure never keeps critical
/// space from stopping executions.
async fn clean_up(
    manager: &Manager,
    settings: &Disk,
    workspaces: &[Workspace],
    filesystem: &mut Filesystem,
    available: impl Fn(&Path) -> io::Result<u64>,
) -> bool {
    let cleaned = async {
        delete_xcode_leftovers(manager, filesystem, &available).await?;
        free_space(manager, settings, workspaces, filesystem, &available).await?;
        empty_caches(manager, settings, filesystem, &available).await
    }
    .await;
    let Err(error) = cleaned else {
        return true;
    };
    log!(
        "disk space cleanup at {}: {error:#}",
        filesystem.path.display()
    );
    match available(&filesystem.path) {
        Ok(free) => {
            filesystem.free = free;
            true
        }
        Err(error) => {
            log!("disk space reading {}: {error}", filesystem.path.display());
            false
        }
    }
}

/// Delete the Xcode data that removed workspaces left on the filesystem,
/// which loses nothing, so it goes before any workspace.
async fn delete_xcode_leftovers(
    manager: &Manager,
    filesystem: &mut Filesystem,
    available: impl Fn(&Path) -> io::Result<u64>,
) -> Result<()> {
    let mut deleted = false;
    for repo in manager.repositories().await? {
        deleted |= manager
            .delete_xcode_leftovers(&repo, |path| filesystem.holds(path))
            .await;
    }
    if deleted {
        filesystem.free = available(&filesystem.path)?;
    }
    Ok(())
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
        let Some(snapshot) = super::cleanup::candidate(manager, workspace).await else {
            continue;
        };
        let (kind, message) = match manager.remove_for_disk_space(&workspace.id, snapshot).await {
            Ok(()) => {
                log!("disk space cleanup removed {}", workspace.name);
                (
                    NotificationKind::WorkspaceRemoved,
                    crate::daemon::workspace::removal_notice(
                        &workspace.name,
                        &format!("removed by disk space cleanup; {}", filesystem.describe()),
                    ),
                )
            }
            Err(error) => {
                // A full disk can fail this write too; the notice still goes out.
                if let Err(recording) = manager
                    .record_retained(&workspace.id, EventCause::DiskSpace, &error)
                    .await
                {
                    log!(
                        "disk space cleanup could not record retaining {}: {recording:#}",
                        workspace.name
                    );
                }
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

/// Empty shared caches on the filesystem that nothing is using until it has
/// enough space.
async fn empty_caches(
    manager: &Manager,
    settings: &Disk,
    filesystem: &mut Filesystem,
    available: impl Fn(&Path) -> io::Result<u64>,
) -> Result<()> {
    for repo in manager.repositories().await? {
        if filesystem.free >= settings.cleanup_free_bytes() {
            break;
        }
        if !caches_on(&repo, filesystem) {
            continue;
        }
        let name = crate::repository::name(&repo);
        let emptying = manager.empty_caches(&repo, false).await;
        if let Some((kind, message)) = emptying_notice(name, emptying, filesystem) {
            manager.notify(None, kind, message).await;
        }
        // Even caches it reports as busy or absent may have had leftovers purged.
        filesystem.free = available(&filesystem.path)?;
    }
    Ok(())
}

/// Whether the repository keeps its shared caches on the filesystem.
fn caches_on(repo: &Repository, filesystem: &Filesystem) -> bool {
    Layout::of(repo).is_some_and(|layout| filesystem.holds(layout.root()))
}

/// The notification for emptying a repository's caches, if it did anything
/// worth telling the user.
fn emptying_notice(
    name: &str,
    emptying: Result<Option<Emptying>>,
    filesystem: &Filesystem,
) -> Option<(NotificationKind, String)> {
    match emptying {
        Ok(Some(Emptying::Emptied { caches })) => {
            log!("disk space cleanup emptied caches of {name}");
            Some((
                NotificationKind::CachesEmptied,
                format!(
                    "Emptied caches of {name} for disk space: {}; {}",
                    caches.join(", "),
                    filesystem.describe()
                ),
            ))
        }
        Ok(_) => None,
        Err(error) => {
            log!("disk space cleanup kept caches of {name}: {error:#}");
            Some((
                NotificationKind::CleanupFailed,
                format!("Disk space cleanup could not empty caches of {name}: {error:#}"),
            ))
        }
    }
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
    let workspaces = workspaces_with_unprotected_executions(manager).await?;
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

/// Workspaces with executions other than agents already stopped by protection.
/// This inventory is advisory; stopping rechecks protection under its locks.
async fn workspaces_with_unprotected_executions(manager: &Manager) -> Result<Vec<String>> {
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
    Ok(workspaces)
}

#[cfg(test)]
mod tests;
