//! What checking a launch's new workspace before a start's pickers
//! refuses: the workspace settled as its start settles it, short of
//! fetching a PR's branches, with what a picker has yet to choose left
//! open, then what creating it would refuse, without creating anything.
use anyhow::Result;
use shoal_protocol::launches::Launch;

use super::{Settled, Settling};
use crate::{
    daemon::{manager::Manager, scope::Caller, workspace::NewWorktreeOptions},
    model::Workspace,
    start_plan::Creation,
};

impl Manager {
    /// Refuse what starting the new workspace `launch` names for `caller`
    /// would refuse, settled as [`Self::workspace_placement`] settles it
    /// short of fetching a PR's branches, and with what a picker has yet to
    /// choose left open; returns instead the existing workspace that has
    /// the branch it names or that its issue reopens, which the start goes
    /// into, refusing one with an agent of its own the launch does not
    /// replace.
    pub(in crate::daemon::launches) async fn check_workspace_placement(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<Workspace>> {
        let workspace = self.check_workspace_creation(launch, caller).await?;
        if let Some(workspace) = &workspace {
            self.check_takes_worker(launch, workspace).await?;
        }
        Ok(workspace)
    }

    /// Refuse what creating the workspace `launch` names for `caller` would
    /// refuse, as [`Self::check_workspace_placement`] checks a start, short
    /// of the agent it starts; returns instead the existing workspace it
    /// opens.
    pub(in crate::daemon::launches) async fn check_workspace_creation(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<Workspace>> {
        match self.settle_workspace(launch, false, caller).await? {
            Settling::New(settled) => {
                let reopened = self.reopened_workspace(&settled, caller).await?;
                (self.check_settled(launch, *settled, caller).await)?;
                Ok(reopened)
            }
            Settling::Existing(workspace) => Ok(Some(*workspace)),
        }
    }

    /// The workspace `caller` sees on the existing branch `settled` opens,
    /// such as the one an issue's earlier work left, which the start
    /// reopens.
    async fn reopened_workspace(
        &self,
        settled: &Settled,
        caller: Option<&Caller>,
    ) -> Result<Option<Workspace>> {
        let Some(branch) = &settled.creation.existing else {
            return Ok(None);
        };
        let workspaces = self.launch_workspaces(caller).await?;
        let workspaces =
            crate::launches::workspace::repository_workspaces(&workspaces, &settled.repo);
        Ok(crate::launches::on_branch(&workspaces, branch, None).cloned())
    }

    /// Refuse what creating the workspace `settled` describes for `launch`
    /// would refuse below the parent it goes below, a scoped `caller`'s
    /// copy confined to its scope as the start confines it.
    async fn check_settled(
        &self,
        launch: &Launch,
        Settled {
            repo, mut creation, ..
        }: Settled,
        caller: Option<&Caller>,
    ) -> Result<()> {
        if let (Some(caller), Some(copy)) = (caller, &mut creation.copy) {
            self.copy_within(caller, &mut copy.source).await?;
        }
        (self.check_launch_parent(launch, Some(&repo.id), caller)).await?;
        let repository: crate::repository::Selector = (&repo.id).into();
        let (branch, existing) = (creation.branch.as_deref(), creation.existing.as_deref());
        (self.check_creation(&repository, branch, existing, worktree_options(&creation))).await
    }
}

/// The options of the new worktree `creation` describes.
fn worktree_options(creation: &Creation) -> NewWorktreeOptions<'_> {
    NewWorktreeOptions {
        git_profile: creation.git_profile.as_deref(),
        path: creation.path.as_deref(),
        base: creation.base.as_deref(),
        disposable: creation.options.disposable,
    }
}

#[cfg(test)]
mod tests;
