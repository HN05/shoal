//! Removal of spaces whose work merged into the default branch once nothing
//! has used them for the configured idle period, whether or not their
//! assignment was marked done. Idle time comes from recorded activity and file
//! change times, so it survives daemon restarts. A chat has no branch, so it
//! goes once its agent has been stopped for that period.
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Result;
use tokio::time::Instant;

use super::{Rule, Snapshot, Tree, change_time};
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{events::EventCause, log, notifications::NotificationKind, workspace::Manager},
    model::{Chat, Workspace},
    process::DirectoryUsers,
    state::WorkspaceState,
    time::UnixTime,
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
    /// The newest file change each workspace's last walk saw, no later than
    /// that walk. Change times only move forward, so until
    /// the idle period passes that change, another walk could only find the
    /// workspace in use; a modification time set into the future does not
    /// defer walks past it.
    changed: HashMap<String, UnixTime>,
}

impl State {
    /// Whether a failed removal of this unchanged workspace is too recent to retry.
    fn waits(&self, id: &str, signals: u64) -> bool {
        self.retained
            .get(id)
            .is_some_and(|(retained, at)| *retained == signals && at.elapsed() < RETRY_AFTER)
    }

    /// Whether the last walk saw a file change after `deadline`.
    fn changed_after(&self, id: &str, deadline: UnixTime) -> bool {
        self.changed
            .get(id)
            .is_some_and(|changed| *changed > deadline)
    }

    /// Remember the newest change a walk at `now` saw, no later than the walk.
    fn remember_change(&mut self, id: &str, changed: UnixTime, now: UnixTime) {
        self.changed.insert(id.to_owned(), changed.min(now));
    }

    fn forget_removed(&mut self, workspaces: &[Workspace], chats: &[Chat]) {
        let present = |id: &String| {
            workspaces.iter().any(|workspace| &workspace.id == id)
                || chats.iter().any(|chat| &chat.id == id)
        };
        self.retained.retain(|id, _| present(id));
        self.changed.retain(|id, _| present(id));
    }
}

/// One pass over every ready workspace, reading the processes in each from
/// `users`, and every chat; a failure in one does not skip the others.
pub async fn sweep(manager: &Manager, state: &mut State, users: &DirectoryUsers) -> Result<()> {
    sweep_at(manager, state, UnixTime::now(), users).await
}

/// A pass as of `now`.
async fn sweep_at(
    manager: &Manager,
    state: &mut State,
    now: UnixTime,
    users: &DirectoryUsers,
) -> Result<()> {
    let workspaces = manager.list_workspaces().await?;
    let chats = manager.chats().await?;
    state.forget_removed(&workspaces, &chats);
    let mut failed = super::SpaceFailures::default();
    for workspace in workspaces {
        if workspace.state != WorkspaceState::Ready || manager.sweep_skips(&workspace.path).await {
            continue;
        }
        if let Err(error) = sweep_workspace(manager, state, &workspace, now, users).await {
            failed.push(&workspace.id, &workspace.name, &error);
        }
    }
    for chat in chats {
        if let Err(error) = sweep_chat(manager, state, &chat, now).await {
            failed.push(&chat.id, &chat.name, &error);
        }
    }
    failed.into_result()
}

/// Remove `chat` once its agent has been stopped for the merged idle period
/// and nothing keeps it.
async fn sweep_chat(
    manager: &Manager,
    state: &mut State,
    chat: &Chat,
    now: UnixTime,
) -> Result<()> {
    let settings = manager.chat_settings(chat).await?;
    let Some(delay) = settings.cleanup.merged.delay() else {
        return Ok(());
    };
    let idle = (manager.chat_stopped_at(&chat.id).await?)
        .is_some_and(|stopped_at| stopped_at <= now - delay);
    if !idle {
        return Ok(());
    }
    let Some(snapshot) = manager
        .chat_cleanup_snapshot(chat, super::Rule::Merged)
        .await?
    else {
        return Ok(());
    };
    if state.waits(&chat.id, snapshot.signals) {
        return Ok(());
    }
    let removal = (manager.remove_unused_chat(chat, snapshot, EventCause::Merged)).await;
    match &removal {
        Ok(false) => return Ok(()),
        Ok(true) => {
            state.retained.remove(&chat.id);
        }
        Err(_) => {
            state
                .retained
                .insert(chat.id.clone(), (snapshot.signals, Instant::now()));
        }
    }
    super::notify_chat_removal(manager, chat, "merged cleanup", removal).await;
    Ok(())
}

async fn sweep_workspace(
    manager: &Manager,
    state: &mut State,
    workspace: &Workspace,
    now: UnixTime,
    users: &DirectoryUsers,
) -> Result<()> {
    let Some(snapshot) = idle_candidate(manager, state, workspace, now, users).await else {
        return Ok(());
    };
    let (kind, message, recorded) = match manager
        .remove_merged_idle(&SpaceSelector::id(&workspace.id), snapshot)
        .await
    {
        Ok(false) => return Ok(()),
        Ok(true) => {
            log!("merged cleanup removed {}", workspace.name);
            state.retained.remove(&workspace.id);
            (
                NotificationKind::SpaceRemoved,
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
    now: UnixTime,
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
        let (snapshot, changed) = walked_snapshot(path, workspace.git_dir.clone(), signals).await?;
        state.remember_change(&workspace.id, changed, now);
        Ok((changed <= deadline).then_some(snapshot))
    }
    .await;
    observed.unwrap_or_else(|error: anyhow::Error| {
        log!("merged cleanup skipped {}: {error:#}", workspace.name);
        None
    })
}

/// The time after which any use keeps the workspace as of
/// `now`, or `None` when merged cleanup is disabled for it or recorded
/// activity already keeps it.
async fn idle_deadline(
    manager: &Manager,
    workspace: &Workspace,
    now: UnixTime,
) -> Result<Option<UnixTime>> {
    let settings = manager.workspace_settings(workspace).await?;
    let Some(delay) = settings.cleanup.merged.delay() else {
        return Ok(None);
    };
    let deadline = now - delay;
    Ok((manager.active_at(&workspace.id).await? <= deadline).then_some(deadline))
}

/// The snapshot of a merged candidate at `path` with `signals`, and the
/// newest change its walk saw.
async fn walked_snapshot(
    path: PathBuf,
    git_dir: Option<PathBuf>,
    signals: u64,
) -> Result<(Snapshot, UnixTime)> {
    let tree = tokio::task::spawn_blocking(move || walk_with_git_state(&path, git_dir.as_deref()))
        .await??;
    let snapshot = Snapshot {
        rule: Rule::Merged,
        signals,
        tree: tree.fingerprint,
    };
    Ok((snapshot, tree.changed))
}

/// One walk of the worktree, whose newest change also covers the
/// per-worktree Git state that commits, checkouts and staging update.
fn walk_with_git_state(root: &Path, git_dir: Option<&Path>) -> Result<Tree> {
    let mut tree = super::walk(root)?;
    let state: Vec<PathBuf> = git_dir
        .into_iter()
        .flat_map(|dir| ["HEAD", "index", "logs/HEAD"].map(|file| dir.join(file)))
        .collect();
    for path in state {
        if path.try_exists()? {
            tree.changed = tree.changed.max(change_time(&fs::symlink_metadata(&path)?));
        }
    }
    Ok(tree)
}

#[cfg(test)]
mod tests;
