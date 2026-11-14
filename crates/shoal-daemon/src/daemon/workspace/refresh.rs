//! Refresh: bring a workspace up to date with what it was made from while it
//! holds nothing of its own. A branch moves only by fast-forward, and work
//! of its own is refused, never carried over.
use anyhow::{Context, Result, bail, ensure};

use super::Manager;
use crate::client::execution_ids::ExecutionId;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::scope::Caller,
    git,
    model::{PresentSpace, Refreshed, Repository, Workspace},
    state::WorkspaceState,
};

mod own_work;
mod target;

use own_work::OwnWork;
use target::{Found, Move, Target};

impl Manager {
    /// `shoal refresh`: bring the workspace `selector` names up to date with
    /// what it was made from, refusing one that holds work of its own, or in
    /// which an execution other than `caller`'s own runs.
    pub async fn refresh_workspace(
        &self,
        selector: &SpaceSelector,
        caller: Option<&Caller>,
    ) -> Result<Refreshed> {
        let workspace = match self.space(selector).await? {
            PresentSpace::Workspace(workspace) => workspace,
            PresentSpace::Chat(_) => bail!("a chat has no worktree to refresh"),
        };
        let target = self.refresh_target(&workspace, caller).await?;
        if matches!(target, Target::DefaultBranch) {
            // The sync takes the repository's Git gate itself.
            self.sync_repository(&(&workspace.repository_id).into())
                .await
                .context("could not sync the default branch to refresh to")?;
        }
        let (repo, _git) = self.lock_repository(&workspace.repository_id).await?;
        let reserved = self
            .reserve_lifecycle(&workspace.id, WorkspaceState::Updating)
            .await?;
        // A rename may have finished while this request waited.
        let workspace = self.workspace_by_id(&workspace.id).await?;
        let own = caller.and_then(Caller::execution_id);
        let result = self
            .refresh_reserved(&repo, &workspace, reserved, target, own)
            .await;
        self.set_state(&workspace.id, reserved, workspace.error.clone())
            .await?;
        let refreshed = result?;
        self.touch(&workspace.id).await;
        Ok(Refreshed {
            workspace: self.workspace_by_id(&workspace.id).await?,
            ..refreshed
        })
    }

    /// Refresh the reserved `workspace`, which was in `state`, to `target`,
    /// once nothing but the execution `own` runs in it.
    async fn refresh_reserved(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        state: WorkspaceState,
        target: Target,
        own: Option<&ExecutionId>,
    ) -> Result<Refreshed> {
        self.ensure_refreshable(workspace, state, own).await?;
        let path = &workspace.path;
        let from = git::resolve_commit(path, "HEAD", git::run_isolated).await?;
        let (target, up_to_date) = match self.find_target(repo, target).await? {
            Found::Fixed(what) => (what, true),
            Found::Moving(moving) => {
                let moved = self.move_to_target(workspace, &moving).await?;
                (moving.label, !moved)
            }
        };
        Ok(Refreshed {
            workspace: workspace.clone(),
            target,
            to: git::resolve_commit(path, "HEAD", git::run_isolated).await?,
            from,
            up_to_date,
            setup: false,
        })
    }

    /// Refuse to refresh `workspace`, which was in `state`, unless it is
    /// ready, its worktree verifies with no Git operation under way, and no
    /// execution but `own` runs in it.
    async fn ensure_refreshable(
        &self,
        workspace: &Workspace,
        state: WorkspaceState,
        own: Option<&ExecutionId>,
    ) -> Result<()> {
        ensure!(
            state == WorkspaceState::Ready,
            "workspace {} is {state}; refresh it once it is ready",
            workspace.name
        );
        self.ensure_no_other_execution(workspace, own).await?;
        self.verify_worktree(workspace).await?;
        ensure!(
            !git::status::operation_in_progress(&workspace.path).await?,
            "a Git operation is under way in workspace {}; finish or abort it first",
            workspace.name
        );
        Ok(())
    }

    /// Move `workspace` as `moving` says unless it already has its commit,
    /// returning whether it moved; work of its own refuses the move.
    async fn move_to_target(&self, workspace: &Workspace, moving: &Move) -> Result<bool> {
        let (path, commit) = (&workspace.path, moving.commit.as_str());
        match &moving.how {
            target::How::FastForward => {
                if git::is_ancestor(path, commit, "HEAD", git::isolated_command).await? {
                    return Ok(false);
                }
                OwnWork::branch(commit, &moving.label)
                    .refuse(workspace)
                    .await?;
                self.fast_forward_to(workspace, commit).await?;
            }
            target::How::Restart => {
                let at = workspace
                    .at
                    .as_ref()
                    .context("workspace records no start")?;
                if at.commit == moving.commit {
                    return Ok(false);
                }
                OwnWork::at_commit(at.commit.as_str())
                    .refuse(workspace)
                    .await?;
                self.move_to_start(workspace, moving.commit.clone()).await?;
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
