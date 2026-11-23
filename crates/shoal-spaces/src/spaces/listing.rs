//! Reading spaces of both kinds as one listing: every space, or those at
//! or below one, each read in one transaction.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::Result;
use shoal_ids::space_ids::SpaceId;

use crate::{
    model::{Space, SpacePart, Worktree},
    {chats, scope::Caller, workspace},
};

impl Spaces {
    /// Every space: the workspaces by name, then the chats in start order.
    pub async fn present_spaces(&self, daemon: &dyn Daemon) -> Result<Vec<Space>> {
        self.spaces_within(daemon, None).await
    }

    /// The space `root` and every space below it, or every space without
    /// `root`: the workspaces by name, then the chats in start order, then
    /// those of this machine below `root` through spaces on other machines.
    pub async fn spaces_within(
        &self,
        daemon: &dyn Daemon,
        root: Option<&SpaceId>,
    ) -> Result<Vec<Space>> {
        let mut spaces = self.local_spaces_within(root).await?;
        if let Some(root) = root {
            for below in daemon.reached_through_elsewhere(root).await {
                spaces.extend(self.local_spaces_within(Some(&below)).await?);
            }
        }
        Ok(spaces)
    }

    /// The space `root` and every space below it through this machine's
    /// spaces, or every space without `root`.
    async fn local_spaces_within(&self, root: Option<&SpaceId>) -> Result<Vec<Space>> {
        let (root, owner) = (root.cloned(), self.root_dir_owner());
        let records = self.records.clone();
        let (workspaces, chats) = self
            .store
            .read(move |tx| {
                let workspaces = workspace::workspaces_within(tx, &*records, root.as_ref())?;
                let chats = chats::recorded_within(tx, &*records, &owner, root.as_ref())?;
                Ok((workspaces, chats))
            })
            .await?;
        let chats = self.with_agents(chats);
        Ok((workspaces.into_iter().map(Space::from))
            .chain(chats.into_iter().map(Space::from))
            .collect())
    }

    /// The spaces `caller` sees, as listing them shows them: with what each
    /// workspace's processes use, the turn state of the coordinator that
    /// does its work and the job the runner runs there.
    pub async fn listed_spaces(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
    ) -> Result<Vec<Space>> {
        let mut spaces = daemon.coordination().visible_spaces(daemon, caller).await?;
        self.load_usage(&mut spaces);
        self.load_coordinator_states(daemon, &mut spaces).await?;
        daemon.attach_jobs(&mut spaces).await?;
        Ok(spaces)
    }

    /// Fill in what the processes of each workspace among `spaces` use,
    /// from the latest sample.
    fn load_usage(&self, spaces: &mut [Space]) {
        for (id, worktree) in worktrees_mut(spaces) {
            worktree.usage = self.workspace_usage(id);
        }
    }
}

/// The ID and worktree of each workspace among `spaces`.
pub fn worktrees_mut(spaces: &mut [Space]) -> impl Iterator<Item = (&SpaceId, &mut Worktree)> {
    spaces
        .iter_mut()
        .filter_map(|Space { id, part, .. }| match part {
            SpacePart::Workspace(worktree) => Some((&*id, worktree)),
            SpacePart::Chat(_) => None,
        })
}
