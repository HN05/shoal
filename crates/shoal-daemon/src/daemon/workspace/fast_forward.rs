//! Fast-forward a workspace to its branch's pushed head, as a PR review does
//! before reviewing it, or to what a refresh brings it up to date with.
use super::Manager;
use crate::client::space_names::SpaceSelector;
use crate::{git, model::Workspace, state::WorkspaceState};
use anyhow::{Context, Result, ensure};
use shoal_refs::BranchName;

impl Manager {
    /// Fast-forward the workspace recorded on `branch` to the fetched
    /// `origin/<branch>`, returning whether it moved. The lifecycle reservation
    /// and the repository's Git gate exclude removal, renames and restores
    /// meanwhile, and a worktree that is no longer the recorded one or is off
    /// `branch` is refused before Git changes anything.
    pub async fn fast_forward_workspace(
        &self,
        selector: &SpaceSelector,
        branch: &BranchName,
    ) -> Result<bool> {
        let id = self.workspace(selector).await?.id;
        let reserved = self
            .reserve_lifecycle(&id, WorkspaceState::Updating)
            .await?;
        // A rename may have finished while this request waited.
        let workspace = self.workspace_by_id(&id).await?;
        let result = self.fast_forward(&workspace, reserved, branch).await;
        self.set_state(&id, reserved, workspace.error.clone())
            .await?;
        result
    }

    async fn fast_forward(
        &self,
        workspace: &Workspace,
        state: WorkspaceState,
        branch: &BranchName,
    ) -> Result<bool> {
        ensure!(
            state == WorkspaceState::Ready,
            "workspace {} is {state}; inspect or set it up before updating it",
            workspace.name
        );
        let recorded = workspace.require_branch("fast-forwarding")?;
        ensure!(
            recorded == branch,
            "workspace {} is recorded on {recorded}, not {branch}",
            workspace.name,
        );
        let _git = self.gates.lock_git(&workspace.repository_id).await;
        let target = git::remote_ref("origin", branch.as_str());
        self.fast_forward_to(workspace, &target).await
    }

    /// Fast-forward the recorded branch of the reserved `workspace` to the
    /// revision `target`, returning whether it moved; the caller holds the
    /// repository's Git gate. A worktree that is no longer the recorded one,
    /// is off its branch or has diverged from `target` is refused before Git
    /// changes anything.
    pub(super) async fn fast_forward_to(
        &self,
        workspace: &Workspace,
        target: &str,
    ) -> Result<bool> {
        let branch = workspace.require_branch("fast-forwarding")?;
        self.verify_checkout(workspace, Some(branch.as_str()))
            .await?;
        let path = &workspace.path;
        if git::is_ancestor(path, target, "HEAD", git::isolated_command).await? {
            return Ok(false);
        }
        ensure!(
            git::is_ancestor(path, "HEAD", target, git::isolated_command).await?,
            "workspace {} has diverged from {target}; update it first",
            workspace.name
        );
        git::branch::fast_forward(path, target)
            .await
            .with_context(|| {
                format!(
                    "could not fast-forward workspace {} to {target}",
                    workspace.name
                )
            })?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
