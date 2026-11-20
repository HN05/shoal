//! Who moves a space to another joined machine: a scoped caller moves every
//! space its scope reaches, chats included, with no permission key, as it
//! renames or converts them, and a caller on another machine too, since the
//! space stays within the hub; ending a space the user holds takes those
//! holds only as the caller's `user_holds` permission allows.
use std::path::Path;

use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    daemon::{manager::Manager, scope::Caller},
    transfer::MoveDestination,
};

impl Manager {
    /// Let `caller` move the spaces `spaces` to a joined machine: each one
    /// here that its scope reaches, and that the user holds only where it
    /// may take those holds.
    pub(crate) async fn authorize_joined_move(
        &self,
        caller: &Caller,
        spaces: &[SpaceId],
    ) -> Result<()> {
        for id in spaces {
            let selector = SpaceSelector::id(id);
            self.ensure_reaches(caller, &self.space(&selector).await?)
                .await?;
            self.authorize_held_removal(caller, &selector, false)
                .await?;
        }
        Ok(())
    }

    /// Whether `file` is where the package of the space `selector` names
    /// waits among those delivered from here, as a copy for a joined
    /// machine is.
    pub(crate) async fn packs_for_delivery(
        &self,
        selector: &SpaceSelector,
        file: &Path,
    ) -> Result<bool> {
        let space = self.space(selector).await?;
        let staged = (self.paths.move_packages()).join(crate::transfer::package_name(&space.id));
        Ok(file == staged)
    }

    /// Whether the move recorded of the space `selector` names goes to a
    /// joined machine.
    pub(crate) async fn moves_to_joined_machine(&self, selector: &SpaceSelector) -> Result<bool> {
        let space = self.space(selector).await?;
        let record = self.recorded_move(&space.id).await?;
        Ok(matches!(record.destination, MoveDestination::Machine(_)))
    }
}
