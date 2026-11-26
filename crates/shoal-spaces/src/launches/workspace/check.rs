//! Checking a launch's new workspace before a start's pickers, as its start
//! settles it, short of fetching a PR's branches or creating anything.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::Result;
use shoal_protocol::launches::Launch;

use super::{Settled, Settling};
use crate::{
    model::Workspace,
    start_plan::Creation,
    {scope::Caller, workspace::NewWorktreeOptions},
};

impl Spaces {
    /// Settles as [`Self::workspace_placement`] does, short of fetching a
    /// PR's branches, with what a picker has yet to choose left open.
    /// Returns the existing workspace the start goes into instead, the one
    /// with the branch it names or that its issue reopens, and refuses one
    /// with an agent of its own the launch does not replace.
    pub(in crate::launches) async fn check_workspace_placement(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<Workspace>> {
        let workspace = self
            .check_workspace_creation(daemon, launch, caller)
            .await?;
        if let Some(workspace) = &workspace {
            self.check_takes_worker(daemon, launch, workspace).await?;
        }
        Ok(workspace)
    }

    /// As [`Self::check_workspace_placement`] checks a start, without the
    /// agent. Returns the existing workspace it opens instead.
    pub(in crate::launches) async fn check_workspace_creation(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<Workspace>> {
        match self.settle_workspace(daemon, launch, false, caller).await? {
            Settling::New(settled) => {
                let reopened = self.reopened_workspace(daemon, &settled).await?;
                (self.check_settled(daemon, launch, *settled, caller).await)?;
                Ok(reopened)
            }
            Settling::Existing(workspace) => Ok(Some(*workspace)),
        }
    }

    /// Such as the one an issue's earlier work left, found as opening the
    /// branch finds it.
    async fn reopened_workspace(
        &self,
        daemon: &dyn Daemon,
        settled: &Settled,
    ) -> Result<Option<Workspace>> {
        match &settled.creation.existing {
            Some(branch) => self.opening_owner(daemon, &settled.repo, branch).await,
            None => Ok(None),
        }
    }

    /// Refuses what creating it below its parent would refuse. A scoped
    /// `caller`'s copy is confined to its scope as the start confines it.
    async fn check_settled(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        Settled {
            repo, mut creation, ..
        }: Settled,
        caller: Option<&Caller>,
    ) -> Result<()> {
        if let (Some(caller), Some(copy)) = (caller, &mut creation.copy) {
            daemon
                .coordination()
                .copy_within(daemon, caller, &mut copy.source)
                .await?;
        }
        (self.check_launch_parent(daemon, launch, Some(&repo.id), caller)).await?;
        let repository: crate::repository::Selector = (&repo.id).into();
        let (branch, existing) = (creation.branch.as_deref(), creation.existing.as_deref());
        (self.check_creation(
            daemon,
            &repository,
            branch,
            existing,
            worktree_options(&creation),
        ))
        .await
    }
}

fn worktree_options(creation: &Creation) -> NewWorktreeOptions<'_> {
    NewWorktreeOptions {
        git_profile: creation.git_profile.as_deref(),
        path: creation.path.as_deref(),
        base: creation.base.as_deref(),
        disposable: creation.options.disposable,
    }
}
