//! Merge conflict checks for `shoal conflicts` and workspace watches.
use anyhow::{Context, Result};

use super::Manager;
use crate::{
    git::{self, conflicts::merge_conflicts, default_branch::DefaultBranchLookup},
    model::{ConflictCheck, Workspace},
};

impl Manager {
    /// Whether the workspace's committed HEAD merges cleanly into `target`,
    /// which defaults to the base workspace's branch, then the default branch.
    pub async fn check_conflicts(
        &self,
        selector: &str,
        target: Option<String>,
    ) -> Result<ConflictCheck> {
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let (target, reference) = match target {
            Some(target) => (target.clone(), target),
            None => {
                let branch = self.conflict_target(&workspace).await?;
                let reference = git::local_ref(&branch);
                (branch, reference)
            }
        };
        let head = git::resolve_commit(&workspace.path, "HEAD", git::run_isolated).await?;
        let target_commit = git::resolve_commit(&workspace.path, &reference, git::run_isolated)
            .await
            .with_context(|| format!("cannot resolve {target}"))?;
        let files = merge_conflicts(&workspace.path, &head, &target_commit).await?;
        Ok(ConflictCheck {
            workspace_id: workspace.id,
            target,
            target_commit,
            head,
            conflicts: !files.is_empty(),
            files,
        })
    }

    async fn conflict_target(&self, workspace: &Workspace) -> Result<String> {
        if let Some(base) = &workspace.base_workspace {
            return Ok(base.branch.clone());
        }
        let repo = self.repository(workspace.repository_id.as_str()).await?;
        git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await
    }
}

#[cfg(test)]
mod tests;
