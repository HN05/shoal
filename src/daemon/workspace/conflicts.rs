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
        self.conflicts_with(&workspace, target).await
    }

    /// The conflicts a workspace watch reports: those of
    /// [`Self::check_conflicts`] with the default target, unless that target
    /// already has HEAD's changes and a merge could only repeat them, or the
    /// assignment is done at HEAD with its work integrated elsewhere.
    pub(crate) async fn watched_conflicts(&self, selector: &str) -> Result<ConflictCheck> {
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let mut check = self.conflicts_with(&workspace, None).await?;
        if check.conflicts && self.conflicts_settled(&workspace, &check).await? {
            check.conflicts = false;
            check.files.clear();
        }
        Ok(check)
    }

    async fn conflicts_settled(
        &self,
        workspace: &Workspace,
        check: &ConflictCheck,
    ) -> Result<bool> {
        let integrated = self
            .completion(&workspace.id)
            .await?
            .is_some_and(|completion| completion.integrated && completion.head == check.head);
        Ok(integrated
            || git::status::merged_into(&workspace.path, &check.head, &check.target_commit).await?)
    }

    /// The conflicts of `workspace`'s committed HEAD with `target`.
    async fn conflicts_with(
        &self,
        workspace: &Workspace,
        target: Option<String>,
    ) -> Result<ConflictCheck> {
        let (target, reference) = match target {
            Some(target) => (target.clone(), target),
            None => {
                let branch = self.conflict_target(workspace).await?;
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
            workspace_id: workspace.id.clone(),
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
