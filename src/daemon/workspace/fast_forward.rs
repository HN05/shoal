//! Fast-forward a workspace to its branch's pushed head, as a PR review does
//! before reviewing it.
use super::Manager;
use crate::{git, model::Workspace, state::WorkspaceState};
use anyhow::{Context, Result, ensure};

impl Manager {
    /// Fast-forward the workspace recorded on `branch` to the fetched
    /// `origin/<branch>`, returning whether it moved. The lifecycle reservation
    /// and the repository's Git gate exclude removal, renames and restores
    /// meanwhile, and a worktree that is no longer the recorded one or is off
    /// `branch` is refused before Git changes anything.
    pub async fn fast_forward_workspace(&self, selector: &str, branch: &str) -> Result<bool> {
        let id = self.workspace(selector).await?.id;
        let reserved = self
            .reserve_lifecycle(&id, WorkspaceState::Updating)
            .await?;
        // A rename may have finished while this request waited.
        let workspace = self.workspace(&id).await?;
        let result = self.fast_forward(&workspace, reserved, branch).await;
        self.set_state(&id, reserved, workspace.error.clone())
            .await?;
        result
    }

    async fn fast_forward(
        &self,
        workspace: &Workspace,
        state: WorkspaceState,
        branch: &str,
    ) -> Result<bool> {
        ensure!(
            state == WorkspaceState::Ready,
            "workspace {} is {state}; inspect or set it up before updating it",
            workspace.name
        );
        ensure!(
            workspace.branch == branch,
            "workspace {} is recorded on {}, not {branch}",
            workspace.name,
            workspace.branch
        );
        let _git = self.gates.lock_git(&workspace.repository_id).await;
        self.verify_checkout(workspace, branch).await?;
        let (path, target) = (&workspace.path, git::remote_ref("origin", branch));
        if git::is_ancestor(path, &target, "HEAD", git::isolated_command).await? {
            return Ok(false);
        }
        ensure!(
            git::is_ancestor(path, "HEAD", &target, git::isolated_command).await?,
            "workspace {} has diverged from {target}; update it first",
            workspace.name
        );
        git::branch::fast_forward(path, &target)
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
