//! Removal of workspaces whose branch merged into the default branch once
//! nothing has used them for the configured idle period, whether or not their
//! assignment was marked done. Idle time comes from recorded activity and file
//! change times, so it survives daemon restarts.
use std::{
    collections::HashMap,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Result;
use tokio::time::Instant;

use super::Snapshot;
use crate::{
    daemon::{events::EventCause, log, notifications::NotificationKind, workspace::Manager},
    model::Workspace,
    state::WorkspaceState,
    time::unix_seconds,
};

/// Retry an unchanged workspace whose removal failed after this long.
const RETRY_AFTER: Duration = Duration::from_secs(60 * 60);

/// What merged cleanup remembers between passes.
#[derive(Default)]
pub struct State {
    /// Workspaces whose removal failed, and when: retried once they change
    /// or after [`RETRY_AFTER`], so a passing failure does not keep them.
    retained: HashMap<String, (Snapshot, Instant)>,
}

impl State {
    /// Whether a failed removal of this unchanged workspace is too recent to retry.
    fn waits(&self, id: &str, snapshot: Snapshot) -> bool {
        self.retained
            .get(id)
            .is_some_and(|(retained, at)| *retained == snapshot && at.elapsed() < RETRY_AFTER)
    }
}

/// One pass over every ready workspace; a failure in one does not skip the others.
pub async fn sweep(manager: &Manager, state: &mut State) -> Result<()> {
    sweep_at(manager, state, unix_seconds()).await
}

/// A pass as of `now`, in Unix seconds.
async fn sweep_at(manager: &Manager, state: &mut State, now: u64) -> Result<()> {
    let workspaces = manager.list_workspaces().await?;
    state
        .retained
        .retain(|id, _| workspaces.iter().any(|workspace| &workspace.id == id));
    let mut failed = super::WorkspaceFailures::default();
    for workspace in workspaces {
        if workspace.state != WorkspaceState::Ready {
            continue;
        }
        if let Err(error) = sweep_workspace(manager, state, &workspace, now).await {
            failed.push(&workspace.id, &workspace.name, &error);
        }
    }
    failed.into_result()
}

async fn sweep_workspace(
    manager: &Manager,
    state: &mut State,
    workspace: &Workspace,
    now: u64,
) -> Result<()> {
    let Some(snapshot) = idle_candidate(manager, workspace, now).await else {
        return Ok(());
    };
    if state.waits(&workspace.id, snapshot) {
        return Ok(());
    }
    let (kind, message, recorded) = match manager.remove_merged_idle(&workspace.id, snapshot).await
    {
        Ok(false) => return Ok(()),
        Ok(true) => {
            log!("merged cleanup removed {}", workspace.name);
            state.retained.remove(&workspace.id);
            (
                NotificationKind::WorkspaceRemoved,
                crate::daemon::workspace::removal_notice(
                    &workspace.name,
                    "removed by merged cleanup",
                ),
                Ok(()),
            )
        }
        Err(error) => {
            log!("merged cleanup retained {}: {error:#}", workspace.name);
            state
                .retained
                .insert(workspace.id.clone(), (snapshot, Instant::now()));
            let recorded = manager
                .record_retained(&workspace.id, EventCause::Merged, &error)
                .await;
            (
                NotificationKind::CleanupFailed,
                format!("merged cleanup retained the workspace: {error:#}"),
                recorded,
            )
        }
    };
    manager.notify(Some(&workspace.name), kind, message).await;
    recorded
}

/// The snapshot of a merged workspace idle for its configured period, or
/// `None`. Cheap checks come first: recorded activity, then Git and process
/// state, and only then a walk of the worktree's files.
async fn idle_candidate(manager: &Manager, workspace: &Workspace, now: u64) -> Option<Snapshot> {
    let observed = async {
        let settings = manager.workspace_settings(workspace).await?;
        let Some(delay) = settings.cleanup.merged.delay() else {
            return Ok(None);
        };
        let deadline = now.saturating_sub(delay.as_secs());
        if manager.active_at(&workspace.id).await? > deadline {
            return Ok(None);
        }
        if !manager.default_branch_fresh(&workspace.repository_id).await {
            return Ok(None);
        }
        let Some(snapshot) = manager.merged_snapshot(&workspace.id).await? else {
            return Ok(None);
        };
        let (path, git_dir) = (workspace.path.clone(), workspace.git_dir.clone());
        let changed =
            tokio::task::spawn_blocking(move || last_change(&path, git_dir.as_deref())).await??;
        Ok((changed <= deadline).then_some(snapshot))
    }
    .await;
    observed.unwrap_or_else(|error: anyhow::Error| {
        log!("merged cleanup skipped {}: {error:#}", workspace.name);
        None
    })
}

/// The newest change time, in Unix seconds, of any file in the worktree,
/// including ignored files, and of the per-worktree Git state that commits,
/// checkouts and staging update. Symlinks are not followed out of the tree.
pub fn last_change(root: &Path, git_dir: Option<&Path>) -> Result<u64> {
    fn changed(path: &Path) -> Result<u64> {
        let metadata = fs::symlink_metadata(path)?;
        Ok(metadata.ctime().max(metadata.mtime()).max(0) as u64)
    }
    let mut newest = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        newest = newest.max(changed(&path)?);
        if fs::symlink_metadata(&path)?.is_dir() {
            for entry in fs::read_dir(&path)? {
                let child = entry?.path();
                if child.file_name().is_none_or(|name| name != ".git") {
                    pending.push(child);
                }
            }
        }
    }
    let state: Vec<PathBuf> = git_dir
        .into_iter()
        .flat_map(|dir| ["HEAD", "index", "logs/HEAD"].map(|file| dir.join(file)))
        .collect();
    for path in state {
        if path.try_exists()? {
            newest = newest.max(changed(&path)?);
        }
    }
    Ok(newest)
}

#[cfg(test)]
mod tests;
