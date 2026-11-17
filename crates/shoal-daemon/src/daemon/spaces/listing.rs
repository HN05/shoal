//! Reading spaces of both kinds as one listing: every space, or those at
//! or below one, each read in one transaction.
use anyhow::Result;
use shoal_ids::space_ids::SpaceId;

use crate::{
    daemon::{
        chats,
        workspace::{self, Manager},
    },
    model::Space,
};

impl Manager {
    /// Every space: the workspaces by name, then the chats in start order.
    pub(crate) async fn present_spaces(&self) -> Result<Vec<Space>> {
        self.spaces_within(None).await
    }

    /// The space `root` and every space below it, or every space without
    /// `root`: the workspaces by name, then the chats in start order.
    pub(crate) async fn spaces_within(&self, root: Option<&SpaceId>) -> Result<Vec<Space>> {
        let (root, owner) = (root.cloned(), self.root_dir_owner());
        let (workspaces, chats) = self
            .store
            .read(move |tx| {
                let workspaces = workspace::workspaces_within(tx, root.as_ref())?;
                let chats = chats::recorded_within(tx, &owner, root.as_ref())?;
                Ok((workspaces, chats))
            })
            .await?;
        let chats = self.with_agents(chats)?;
        Ok((workspaces.into_iter().map(Space::from))
            .chain(chats.into_iter().map(Space::from))
            .collect())
    }
}
