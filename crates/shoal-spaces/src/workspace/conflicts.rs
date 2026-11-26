//! Merge conflict checks for `shoal conflicts` and workspace watches.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Context, Result};
use shoal_ids::space_ids::SpaceId;
use shoal_refs::CommitId;

use crate::{
    git::{
        self,
        conflicts::{changed_on_both_sides, merge_conflicts},
        default_branch::DefaultBranchLookup,
    },
    model::{ChangedOnBothSides, ConflictCheck, Workspace},
};
use shoal_protocol::space_names::SpaceSelector;

pub mod recorded;

impl Spaces {
    /// `target` defaults to the base workspace's branch, then the default
    /// branch.
    pub async fn check_conflicts(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        target: Option<String>,
    ) -> Result<ConflictCheck> {
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(daemon, &workspace).await?;
        let default_target = target.is_none();
        let check = self.conflicts_with(daemon, &workspace, target).await?;
        if default_target {
            self.record_changed_on_both_sides(&check).await?;
        }
        Ok(check)
    }

    async fn lists_sensitive_files(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
    ) -> Result<bool> {
        let settings = daemon.workspace_settings(workspace).await?;
        Ok(!settings.conflicts.sensitive.is_empty())
    }

    pub async fn recorded_changed_on_both_sides(
        &self,
        workspace_id: &SpaceId,
    ) -> Result<Option<ChangedOnBothSides>> {
        let id = workspace_id.to_owned();
        self.store.run(move |db| recorded::get(db, &id)).await
    }

    /// None unless the last check covered the committed HEAD.
    pub async fn current_changed_on_both_sides(
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

    /// None when the target already has HEAD's changes, as
    /// [`Self::conflicts_settled`] decides, or with `only_sensitive` when
    /// the repository lists no sensitive files. Every read of the worktree,
    /// its configuration included, runs under the file-access guard, so a
    /// stalled worktree fails at once.
    pub async fn watched_conflicts(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        only_sensitive: bool,
    ) -> Result<Option<ConflictCheck>> {
        let workspace = self.workspace(selector).await?;
        let path = workspace.path.clone();
        let check = self.watched_check(daemon, workspace, only_sensitive);
        self.file_access
            .checked(&self.notifications, &path, check)
            .await
    }

    async fn watched_check(
        &self,
        daemon: &dyn Daemon,
        workspace: Workspace,
        only_sensitive: bool,
    ) -> Result<Option<ConflictCheck>> {
        if only_sensitive && !self.lists_sensitive_files(daemon, &workspace).await? {
            return Ok(None);
        }
        self.verify_worktree(daemon, &workspace).await?;
        let mut check = self.conflicts_with(daemon, &workspace, None).await?;
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

    /// Also true when the assignment is done at `head` with its work
    /// integrated elsewhere.
    async fn conflicts_settled(
        &self,
        workspace: &Workspace,
        head: &CommitId,
        target_commit: &CommitId,
    ) -> Result<bool> {
        let integrated = self
            .completion(&workspace.id)
            .await?
            .is_some_and(|completion| completion.integrated && completion.head == *head);
        let (head_rev, target_rev) = (head.as_str(), target_commit.as_str());
        Ok(integrated || git::status::merged_into(&workspace.path, head_rev, target_rev).await?)
    }

    /// Changed since their merge base. Empty when the conflicts are
    /// settled.
    async fn sensitive_changes(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        head: &CommitId,
        target_commit: &CommitId,
    ) -> Result<Vec<String>> {
        let settings = daemon.workspace_settings(workspace).await?;
        let sensitive = &settings.conflicts.sensitive;
        let (head_rev, target_rev) = (head.as_str(), target_commit.as_str());
        let files = changed_on_both_sides(&workspace.path, head_rev, target_rev, sensitive).await?;
        if files.is_empty()
            || self
                .conflicts_settled(workspace, head, target_commit)
                .await?
        {
            return Ok(Vec::new());
        }
        Ok(files)
    }

    async fn conflicts_with(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        target: Option<String>,
    ) -> Result<ConflictCheck> {
        let (target, reference) = match target {
            Some(target) => (target.clone(), target),
            None => {
                let branch = self.conflict_target(daemon, workspace).await?;
                let reference = git::local_ref(&branch.as_str().into());
                (branch, reference)
            }
        };
        let head = git::resolve_commit(&workspace.path, "HEAD", git::run_isolated).await?;
        let target_commit = git::resolve_commit(&workspace.path, &reference, git::run_isolated)
            .await
            .with_context(|| format!("cannot resolve {target}"))?;
        let (head_rev, target_rev) = (head.as_str(), target_commit.as_str());
        let files = merge_conflicts(&workspace.path, head_rev, target_rev).await?;
        let changed_on_both_sides = self
            .sensitive_changes(daemon, workspace, &head, &target_commit)
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

    async fn conflict_target(&self, daemon: &dyn Daemon, workspace: &Workspace) -> Result<String> {
        if let Some(branch) = workspace
            .base_workspace
            .as_ref()
            .and_then(|base| base.branch.clone())
        {
            return Ok(branch.into());
        }
        let repo = daemon
            .repository(crate::repository::Selector::from(workspace.repository()))
            .await?;
        git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached)
            .await
            .map(String::from)
    }
}

#[cfg(test)]
mod tests;
