//! Fast-forwarding a workspace to its branch's pushed head, as a PR review
//! does first, or to what a refresh brings it up to date with.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::{git, model::Workspace, state::WorkspaceState};
use anyhow::{Context, Result, ensure};
use shoal_protocol::space_names::SpaceSelector;
use shoal_refs::BranchName;

impl Spaces {
    /// Returns whether it moved. The lifecycle reservation and the
    /// repository's Git gate exclude removal, renames and restores
    /// meanwhile. A worktree that is no longer the recorded one, or is off
    /// `branch`, is refused before Git changes anything.
    pub async fn fast_forward_workspace(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        branch: &BranchName,
    ) -> Result<bool> {
        let id = self.workspace(selector).await?.id;
        let reserved = self
            .reserve_lifecycle(&id, WorkspaceState::Updating)
            .await?;
        // A rename may have finished while this request waited.
        let workspace = self.workspace_by_id(&id).await?;
        let result = self
            .fast_forward(daemon, &workspace, reserved, branch)
            .await;
        self.set_state(&id, reserved, workspace.error.clone())
            .await?;
        result
    }

    async fn fast_forward(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        state: WorkspaceState,
        branch: &BranchName,
    ) -> Result<bool> {
        crate::target_checks::ready(workspace, state).require()?;
        let recorded = workspace.require_branch("fast-forwarding")?;
        ensure!(
            recorded == branch,
            "workspace {} is recorded on {recorded}, not {branch}",
            workspace.name,
        );
        let _git = self.gates.lock_git(workspace.repository()).await;
        let target = git::remote_ref("origin", branch.as_str());
        self.fast_forward_to(daemon, workspace, &target).await
    }

    /// Returns whether it moved. The caller holds the repository's Git gate.
    /// A worktree that is no longer the recorded one, is off its branch or
    /// has diverged from `target` is refused before Git changes anything.
    pub(super) async fn fast_forward_to(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        target: &str,
    ) -> Result<bool> {
        let branch = workspace.require_branch("fast-forwarding")?;
        self.verify_checkout(daemon, workspace, Some(branch.as_str()))
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
