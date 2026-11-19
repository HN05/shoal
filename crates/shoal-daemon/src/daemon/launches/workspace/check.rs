//! What checking a launch's new workspace before a start's pickers
//! refuses: the workspace settled as its start settles it, short of
//! fetching a PR's branches, with what a picker has yet to choose left
//! open, then what creating it would refuse, without creating anything.
use anyhow::Result;
use shoal_protocol::launches::Launch;
use shoal_protocol::space_names::SpaceSelector;

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
    /// the branch it names, which the start goes into.
    pub(in crate::daemon::launches) async fn check_workspace_placement(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<Workspace>> {
        match self.settle_workspace(launch, false, caller).await? {
            Settling::New(settled) => {
                let parent = launch.parent.clone();
                (self.check_settled(*settled, parent, caller).await).map(|()| None)
            }
            Settling::Existing(workspace) => Ok(Some(*workspace)),
        }
    }

    /// Refuse what creating the workspace `settled` describes would refuse
    /// below `parent`, else, for a scoped `caller`, below the space its
    /// start goes below, confined to its scope as the start confines it.
    async fn check_settled(
        &self,
        Settled {
            repo, mut creation, ..
        }: Settled,
        mut parent: Option<SpaceSelector>,
        caller: Option<&Caller>,
    ) -> Result<()> {
        if let Some(caller) = caller {
            (self.confine_checked(caller, &mut creation, &mut parent)).await?;
        }
        let parent = self.parent_space_id(parent.as_ref()).await?;
        let repository = (&repo.id).into();
        (self.ensure_parent_repository(&repository, parent.as_ref())).await?;
        let (branch, existing) = (creation.branch.as_deref(), creation.existing.as_deref());
        (self.check_creation(&repository, branch, existing, worktree_options(&creation))).await
    }

    /// Confine the checked `creation` and its `parent` to the scoped
    /// `caller`'s tree, as its start confines them: a copy of a workspace
    /// its scope reaches, below its own space unless it names one below it.
    async fn confine_checked(
        &self,
        caller: &Caller,
        creation: &mut Creation,
        parent: &mut Option<SpaceSelector>,
    ) -> Result<()> {
        if let Some(copy) = &mut creation.copy {
            self.copy_within(caller, &mut copy.source).await?;
        }
        self.parent_below(caller, parent).await
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
