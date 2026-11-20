//! Who moves a space to another joined machine: a scoped caller moves every
//! space its scope reaches, chats included, with no permission key, as it
//! renames or converts them, and a caller on another machine too, since the
//! space stays within the hub; ending a space the user holds takes those
//! holds only as the caller's `user_holds` permission allows. A copy opens
//! below the space its original is below, so only a copy of a space below
//! the caller's own skips the `admin` permission: one of its own would open
//! beside it, where a scoped caller creates nothing.
use std::path::Path;

use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    daemon::{manager::Manager, scope::Caller},
    transfer::{MoveDestination, MoveOrder},
};

/// Whether `caller` moves the spaces `order` takes with no permission key:
/// to a joined machine, unless it copies its own space there.
pub(crate) fn moves_without_permission(caller: &Caller, order: &MoveOrder) -> bool {
    matches!(order.destination, MoveDestination::Machine(_))
        && !(order.keep && order.spaces.contains(&caller.space_id))
}

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

    /// Whether `file` is where the package of a copy of the space
    /// `selector` names, below `caller`'s own, waits among those delivered
    /// from here, as a copy for a joined machine does that needs no
    /// permission key.
    pub(crate) async fn copies_for_joined_machine(
        &self,
        caller: &Caller,
        selector: &SpaceSelector,
        file: &Path,
    ) -> Result<bool> {
        let space = self.space(selector).await?;
        let staged = (self.paths.move_packages()).join(crate::transfer::package_name(&space.id));
        Ok(file == staged && space.id != caller.space_id)
    }

    /// Whether the space `selector` names is below `caller`'s own, as a
    /// copy that needs no permission key is.
    pub(crate) async fn copies_below_caller(
        &self,
        caller: &Caller,
        selector: &SpaceSelector,
    ) -> Result<bool> {
        Ok(self.space(selector).await?.id != caller.space_id)
    }

    /// Whether a move recorded of the space `selector` names goes to a
    /// joined machine; not for a space that is gone or records none.
    pub(crate) async fn moves_to_joined_machine(&self, selector: &SpaceSelector) -> Result<bool> {
        let Ok(space) = self.space(selector).await else {
            return Ok(false);
        };
        let record = self.move_record(&space.id).await?;
        Ok(record.is_some_and(|record| matches!(record.destination, MoveDestination::Machine(_))))
    }
}

#[cfg(test)]
mod tests;
