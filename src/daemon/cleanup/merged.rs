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

use super::{Rule, Snapshot, Tree};
use crate::{
    daemon::{events::EventCause, log, notifications::NotificationKind, workspace::Manager},
    model::Workspace,
    process::DirectoryUsers,
    state::WorkspaceState,
    time::unix_seconds,
};

/// Retry an unchanged workspace whose removal failed after this long.
const RETRY_AFTER: Duration = Duration::from_secs(60 * 60);

/// What merged cleanup remembers between passes.
#[derive(Default)]
pub struct State {
    /// The signals of workspaces whose removal failed, and when: retried once
    /// HEAD or recorded activity changes or after [`RETRY_AFTER`], so a
    /// passing failure does not keep them. A file change makes the workspace
    /// wait for its idle period again.
    retained: HashMap<String, (u64, Instant)>,
    /// The newest file change each workspace's last walk saw, in Unix seconds.
    /// Change times only move forward, so until the idle period passes that
    /// change, another walk could only find the workspace in use.
    changed: HashMap<String, u64>,
}

impl State {
    /// Whether a failed removal of this unchanged workspace is too recent to retry.
    fn waits(&self, id: &str, signals: u64) -> bool {
        self.retained
            .get(id)
            .is_some_and(|(retained, at)| *retained == signals && at.elapsed() < RETRY_AFTER)
    }

    /// Whether the last walk saw a file change after `deadline`.
    fn changed_after(&self, id: &str, deadline: u64) -> bool {
        self.changed
            .get(id)
            .is_some_and(|changed| *changed > deadline)
    }

    fn forget_removed(&mut self, workspaces: &[Workspace]) {
        let present = |id: &String| workspaces.iter().any(|workspace| &workspace.id == id);
        self.retained.retain(|id, _| present(id));
        self.changed.retain(|id, _| present(id));
    }
}

/// One pass over every ready workspace, reading the processes in each from
/// `users`; a failure in one does not skip the others.
pub async fn sweep(manager: &Manager, state: &mut State, users: &DirectoryUsers) -> Result<()> {
    sweep_at(manager, state, unix_seconds(), users).await
}

/// A pass as of `now`, in Unix seconds.
async fn sweep_at(
    manager: &Manager,
    state: &mut State,
    now: u64,
    users: &DirectoryUsers,
) -> Result<()> {
    let workspaces = manager.list_workspaces().await?;
    state.forget_removed(&workspaces);
    let mut failed = super::WorkspaceFailures::default();
    for workspace in workspaces {
        if workspace.state != WorkspaceState::Ready || manager.sweep_skips(&workspace.path).await {
            continue;
        }
        if let Err(error) = sweep_workspace(manager, state, &workspace, now, users).await {
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
    users: &DirectoryUsers,
) -> Result<()> {
    let Some(snapshot) = idle_candidate(manager, state, workspace, now, users).await else {
        return Ok(());
    };
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
                .insert(workspace.id.clone(), (snapshot.signals, Instant::now()));
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
/// `None`. Cheap checks come first: recorded activity and the change time the
/// last walk saw, then Git and process state and the failed-removal backoff,
/// and only then one walk of the worktree's files.
async fn idle_candidate(
    manager: &Manager,
    state: &mut State,
    workspace: &Workspace,
    now: u64,
    users: &DirectoryUsers,
) -> Option<Snapshot> {
    let observed = async {
        let Some(deadline) = idle_deadline(manager, workspace, now).await? else {
            return Ok(None);
        };
        if state.changed_after(&workspace.id, deadline)
            || !manager.default_branch_fresh(&workspace.repository_id).await
        {
            return Ok(None);
        }
        let Some((path, signals)) = manager.merged_signals(&workspace.id, users).await? else {
            return Ok(None);
        };
        if state.waits(&workspace.id, signals) {
            return Ok(None);
        }
        let git_dir = workspace.git_dir.clone();
        let tree = tokio::task::spawn_blocking(move || walk(&path, git_dir.as_deref())).await??;
        state.changed.insert(workspace.id.clone(), tree.changed);
        Ok((tree.changed <= deadline).then_some(Snapshot {
            rule: Rule::Merged,
            signals,
            tree: tree.fingerprint,
        }))
    }
    .await;
    observed.unwrap_or_else(|error: anyhow::Error| {
        log!("merged cleanup skipped {}: {error:#}", workspace.name);
        None
    })
}

/// The time, in Unix seconds, after which any use keeps the workspace as of
/// `now`, or `None` when merged cleanup is disabled for it or recorded
/// activity already keeps it.
async fn idle_deadline(manager: &Manager, workspace: &Workspace, now: u64) -> Result<Option<u64>> {
    let settings = manager.workspace_settings(workspace).await?;
    let Some(delay) = settings.cleanup.merged.delay() else {
        return Ok(None);
    };
    let deadline = now.saturating_sub(delay.as_secs());
    Ok((manager.active_at(&workspace.id).await? <= deadline).then_some(deadline))
}

/// One walk of the worktree, whose newest change also covers the
/// per-worktree Git state that commits, checkouts and staging update.
fn walk(root: &Path, git_dir: Option<&Path>) -> Result<Tree> {
    let mut tree = super::walk(root)?;
    let state: Vec<PathBuf> = git_dir
        .into_iter()
        .flat_map(|dir| ["HEAD", "index", "logs/HEAD"].map(|file| dir.join(file)))
        .collect();
    for path in state {
        if path.try_exists()? {
            let metadata = fs::symlink_metadata(&path)?;
            let changed = metadata.ctime().max(metadata.mtime()).max(0) as u64;
            tree.changed = tree.changed.max(changed);
        }
    }
    Ok(tree)
}

#[cfg(test)]
mod tests;
