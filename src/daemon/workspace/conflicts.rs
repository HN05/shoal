//! Merge conflict checks for `shoal conflicts` and workspace watches.
use anyhow::{Context, Result};

use super::Manager;
use crate::{
    git::{
        self,
        conflicts::{changed_on_both_sides, merge_conflicts},
        default_branch::DefaultBranchLookup,
    },
    model::{ChangedOnBothSides, ConflictCheck, Workspace},
};

pub(crate) mod recorded;

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
        let default_target = target.is_none();
        let check = self.conflicts_with(&workspace, target).await?;
        if default_target {
            self.record_changed_on_both_sides(&check).await?;
        }
        Ok(check)
    }

    /// Whether the workspace's repository lists sensitive files.
    async fn lists_sensitive_files(&self, workspace: &Workspace) -> Result<bool> {
        let settings = self.workspace_settings(workspace).await?;
        Ok(!settings.conflicts.sensitive.is_empty())
    }

    /// The paths the last check against the workspace's default target
    /// found changed on both sides.
    pub(crate) async fn recorded_changed_on_both_sides(
        &self,
        workspace_id: &str,
    ) -> Result<Option<ChangedOnBothSides>> {
        let id = workspace_id.to_owned();
        self.store.run(move |db| recorded::get(db, &id)).await
    }

    /// The last check against the workspace's default target, when it
    /// checked the committed HEAD.
    pub(crate) async fn current_changed_on_both_sides(
        &self,
        workspace: &Workspace,
    ) -> Result<Option<ChangedOnBothSides>> {
        let Some(recorded) = self.recorded_changed_on_both_sides(&workspace.id).await? else {
            return Ok(None);
        };
        let head = git::resolve_commit(&workspace.path, "HEAD", git::run_isolated).await?;
        Ok((recorded.head == head).then_some(recorded))
    }

    async fn record_changed_on_both_sides(&self, check: &ConflictCheck) -> Result<()> {
        let check = check.clone();
        self.store.run(move |db| recorded::save(db, &check)).await
    }

    /// The conflicts a workspace watch reports: those of
    /// [`Self::check_conflicts`] with the default target, unless the target
    /// already has HEAD's changes as [`Self::conflicts_settled`] decides; or
    /// none when `only_sensitive` and the repository lists no sensitive
    /// files. Every read of the worktree, its configuration included, runs
    /// under the file-access guard, so a stalled worktree fails at once.
    pub(crate) async fn watched_conflicts(
        &self,
        selector: &str,
        only_sensitive: bool,
    ) -> Result<Option<ConflictCheck>> {
        let workspace = self.workspace(selector).await?;
        let path = workspace.path.clone();
        let check = self.watched_check(workspace, only_sensitive);
        self.check_access(&path, check).await
    }

    async fn watched_check(
        &self,
        workspace: Workspace,
        only_sensitive: bool,
    ) -> Result<Option<ConflictCheck>> {
        if only_sensitive && !self.lists_sensitive_files(&workspace).await? {
            return Ok(None);
        }
        self.verify_worktree(&workspace).await?;
        let mut check = self.conflicts_with(&workspace, None).await?;
        self.record_changed_on_both_sides(&check).await?;
        if check.conflicts
            && self
                .conflicts_settled(&workspace, &check.head, &check.target_commit)
                .await?
        {
            check.conflicts = false;
            check.files.clear();
        }
        Ok(Some(check))
    }

    /// Whether `target_commit` already has `head`'s changes and a merge could
    /// only repeat them, or the assignment is done at `head` with its work
    /// integrated elsewhere.
    async fn conflicts_settled(
        &self,
        workspace: &Workspace,
        head: &str,
        target_commit: &str,
    ) -> Result<bool> {
        let integrated = self
            .completion(&workspace.id)
            .await?
            .is_some_and(|completion| completion.integrated && completion.head == head);
        Ok(integrated || git::status::merged_into(&workspace.path, head, target_commit).await?)
    }

    /// The repository's sensitive paths that `head` and `target_commit` both
    /// changed since their merge base, unless the conflicts are settled.
    async fn sensitive_changes(
        &self,
        workspace: &Workspace,
        head: &str,
        target_commit: &str,
    ) -> Result<Vec<String>> {
        let settings = self.workspace_settings(workspace).await?;
        let sensitive = &settings.conflicts.sensitive;
        let files = changed_on_both_sides(&workspace.path, head, target_commit, sensitive).await?;
        if files.is_empty()
            || self
                .conflicts_settled(workspace, head, target_commit)
                .await?
        {
            return Ok(Vec::new());
        }
        Ok(files)
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
        let changed_on_both_sides = self
            .sensitive_changes(workspace, &head, &target_commit)
            .await?;
        Ok(ConflictCheck {
            workspace_id: workspace.id.clone(),
            target,
            target_commit,
            head,
            conflicts: !files.is_empty(),
            files,
            changed_on_both_sides,
        })
    }

    async fn conflict_target(&self, workspace: &Workspace) -> Result<String> {
        if let Some(branch) = workspace
            .base_workspace
            .as_ref()
            .and_then(|base| base.branch.clone())
        {
            return Ok(branch);
        }
        let repo = self.repository(workspace.repository_id.as_str()).await?;
        git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await
    }
}

#[cfg(test)]
mod tests;
