//! Who moves a space to another joined machine. A scoped caller moves every
//! space its scope reaches with no permission key, as it renames or
//! converts them, and the removed spaces it may restore; ending a space the
//! user holds takes those holds only as its `user_holds` permission allows.
//! A copy opens below the space its original is below, so only a copy of a
//! space below the caller's own skips the `admin` permission: one of its
//! own would open beside it, where a scoped caller creates nothing.
use crate::Spaces;
use crate::daemon::Daemon;
use std::path::Path;

use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    scope::Caller,
    transfer::{MoveDestination, MoveOrder},
};

/// To a joined machine, unless it copies its own space there.
pub fn moves_without_permission(caller: &Caller, order: &MoveOrder) -> bool {
    matches!(order.destination, MoveDestination::Machine(_))
        && !(order.keep && order.spaces.contains(&caller.space_id))
}

impl Spaces {
    /// Each space here must be one its scope reaches, and one the user holds
    /// only where it may take those holds. Each removed one must be one it
    /// may restore.
    pub async fn authorize_joined_move(
        &self,
        daemon: &dyn Daemon,
        caller: &Caller,
        spaces: &[SpaceId],
    ) -> Result<()> {
        for id in spaces {
            let selector = SpaceSelector::id(id);
            let Ok(space) = self.space(&selector).await else {
                self.ensure_restorable_by(daemon, &selector, caller).await?;
                continue;
            };
            daemon
                .coordination()
                .ensure_reaches(daemon, caller, &space)
                .await?;
            daemon
                .coordination()
                .authorize_held_removal(daemon, caller, &selector, false)
                .await?;
        }
        Ok(())
    }

    /// A copy for a joined machine of a space below `caller`'s own waits
    /// there and needs no permission key.
    pub async fn copies_for_joined_machine(
        &self,
        caller: &Caller,
        selector: &SpaceSelector,
        file: &Path,
    ) -> Result<bool> {
        let space = self.space(selector).await?;
        let staged = (self.paths.move_packages()).join(crate::transfer::package_name(&space.id));
        Ok(file == staged && space.id != caller.space_id)
    }

    /// A copy that needs no permission key is of another space. The request's
    /// scope check makes it one below.
    pub async fn copies_other_than_own(
        &self,
        caller: &Caller,
        selector: &SpaceSelector,
    ) -> Result<bool> {
        Ok(self.space(selector).await?.id != caller.space_id)
    }

    /// False for a space that is gone or records none.
    pub async fn moves_to_joined_machine(&self, selector: &SpaceSelector) -> Result<bool> {
        let Ok(space) = self.space(selector).await else {
            return Ok(false);
        };
        let record = self.move_record(&space.id).await?;
        Ok(record.is_some_and(|record| matches!(record.destination, MoveDestination::Machine(_))))
    }
}

#[cfg(test)]
mod tests;
