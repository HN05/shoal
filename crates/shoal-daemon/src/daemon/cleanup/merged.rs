//! Removal of spaces whose work merged into the default branch once nothing
//! has used them for the configured idle period, whether or not their
//! assignment was marked done. Idle time comes from recorded activity, the
//! agent's last stop and a worktree's file change times, so it survives
//! daemon restarts. A space without a worktree has no branch, so it goes
//! once nothing has used it for that period.
use crate::client::space_ids::SpaceId;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Result;
use tokio::time::Instant;

use super::{Rule, Snapshot, Tree, change_time};
use crate::{
    daemon::{
        events::EventCause,
        spaces::removal_notices::Cleanup,
        workspace::{Manager, not_ready},
    },
    model::PresentSpace,
    process::DirectoryUsers,
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
    retained: HashMap<SpaceId, (u64, Instant)>,
    /// The newest file change each workspace's last walk saw, no later than
    /// that walk. Change times only move forward, so until
    /// the idle period passes that change, another walk could only find the
    /// workspace in use; a modification time set into the future does not
    /// defer walks past it.
    changed: HashMap<SpaceId, UnixTime>,
}

impl State {
    /// Whether a failed removal of this unchanged workspace is too recent to retry.
    fn waits(&self, id: &SpaceId, signals: u64) -> bool {
        self.retained
            .get(id)
            .is_some_and(|(retained, at)| *retained == signals && at.elapsed() < RETRY_AFTER)
    }

    /// Whether the last walk saw a file change after `deadline`.
    fn changed_after(&self, id: &SpaceId, deadline: UnixTime) -> bool {
        self.changed
            .get(id)
            .is_some_and(|changed| *changed > deadline)
    }

    /// Remember the newest change a walk at `now` saw, no later than the walk.
    fn remember_change(&mut self, id: &SpaceId, changed: UnixTime, now: UnixTime) {
        self.changed.insert(id.to_owned(), changed.min(now));
    }

    fn forget_removed(&mut self, spaces: &[PresentSpace]) {
        let present = |id: &SpaceId| spaces.iter().any(|space| space.id() == id);
        self.retained.retain(|id, _| present(id));
        self.changed.retain(|id, _| present(id));
    }
}

/// One pass over every space, reading the processes in each worktree from
/// `users`; a failure in one does not skip the others.
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
    let spaces = manager.present_spaces().await?;
    state.forget_removed(&spaces);
    let mut failed = super::SpaceFailures::default();
    for space in spaces {
        if let Some(workspace) = space.worktree()
            && (not_ready(workspace).is_some() || manager.sweep_skips(&workspace.path).await)
        {
            continue;
        }
        if let Err(error) = sweep_space(manager, state, &space, now, users).await {
            failed.push(space.id(), space.name(), &error);
        }
    }
    failed.into_result()
}

async fn sweep_space(
    manager: &Manager,
    state: &mut State,
    space: &PresentSpace,
    now: UnixTime,
    users: &DirectoryUsers,
) -> Result<()> {
    let Some(snapshot) = idle_candidate(manager, state, space, now, users).await? else {
        return Ok(());
    };
    let removal = (manager.remove_unused(space, snapshot, EventCause::Merged)).await;
    match &removal {
        Ok(false) => return Ok(()),
        Ok(true) => {
            state.retained.remove(space.id());
        }
        Err(_) => {
            state
                .retained
                .insert(space.id().clone(), (snapshot.signals, Instant::now()));
        }
    }
    (manager.announce_removal(space, Cleanup::Merged, &removal)).await
}

/// The snapshot of a merged space idle for its configured period, or
/// `None`. Cheap checks come first: recorded activity and the change time the
/// last walk saw, then Git and process state and the failed-removal backoff,
/// and only then one walk of the worktree's files.
async fn idle_candidate(
    manager: &Manager,
    state: &mut State,
    space: &PresentSpace,
    now: UnixTime,
    users: &DirectoryUsers,
) -> Result<Option<Snapshot>> {
    let Some(deadline) = idle_deadline(manager, space, now).await? else {
        return Ok(None);
    };
    if state.changed_after(space.id(), deadline) {
        return Ok(None);
    }
    if let Some(workspace) = space.worktree()
        && !manager.default_branch_fresh(&workspace.repository_id).await
    {
        return Ok(None);
    }
    let signals = manager.candidate_signals(space, Rule::Merged, &[], users);
    let Some(signals) = signals.await? else {
        return Ok(None);
    };
    if state.waits(space.id(), signals) {
        return Ok(None);
    }
    let tree = walk(space).await?;
    state.remember_change(space.id(), tree.changed, now);
    let snapshot = Snapshot {
        rule: Rule::Merged,
        signals,
        tree: tree.fingerprint,
    };
    Ok((tree.changed <= deadline).then_some(snapshot))
}

/// The time after which any use keeps the space as of `now`, or `None`
/// when merged cleanup is disabled for it or its last use already keeps it.
async fn idle_deadline(
    manager: &Manager,
    space: &PresentSpace,
    now: UnixTime,
) -> Result<Option<UnixTime>> {
    let settings = manager.present_space_settings(space).await?;
    let Some(delay) = settings.cleanup.merged.delay() else {
        return Ok(None);
    };
    let deadline = now - delay;
    let used = manager.last_used(space).await?;
    Ok((used.is_some_and(|used| used <= deadline)).then_some(deadline))
}

/// One walk of the files of `space`, whose worktree's newest change also
/// covers its Git state; none for a space without one.
async fn walk(space: &PresentSpace) -> Result<Tree> {
    let Some(workspace) = space.worktree() else {
        return Ok(Tree::EMPTY);
    };
    let (path, git_dir) = (workspace.path.clone(), workspace.git_dir.clone());
    tokio::task::spawn_blocking(move || walk_with_git_state(&path, git_dir.as_deref())).await?
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
