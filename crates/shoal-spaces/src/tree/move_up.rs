//! Moving a space up one level: below the space above its parent, or to the
//! top level where that space cannot take it, as when its parent goes but
//! it stays.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Context as _, Result, ensure};
use rusqlite::{OptionalExtension as _, Transaction};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};

use crate::daemon::Restack;

use crate::{model::SpaceRef, spaces::Removing};

impl Spaces {
    /// Returns the space it is below now. A workspace whose branch builds on
    /// the workspace `restack` names is then stacked past it as `restack`
    /// says.
    pub async fn move_up_one_level(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        restack: Option<&Restack<'_>>,
    ) -> Result<Option<SpaceRef>> {
        let held = [self.space_removals.clone(), self.completed_owners.clone()];
        let moving = id.clone();
        let above = (self.store)
            .write(move |tx| move_up(tx, &moving, &held))
            .await?;
        if let Some(restack) = restack
            && let Some(stacked) =
                (restack.base.stacked_workspaces.iter()).find(|stacked| stacked.id == *id)
        {
            daemon.restack(stacked, restack).await?;
        }
        Ok(above)
    }
}

/// Refuses while a removal `held` reserves the space or either parent: a
/// removal that listed the spaces below one of them would otherwise miss
/// it.
fn move_up(tx: &Transaction<'_>, id: &SpaceId, held: &[Removing; 2]) -> Result<Option<SpaceRef>> {
    let parent = super::parent_id(tx, id)?.context("the space has no parent to move up from")?;
    let above = super::parent_id(tx, &parent)?;
    let involved = [Some(id), Some(&parent), above.as_ref()];
    let reserved = |id: &SpaceId| held.iter().any(|removing| removing.reserved(id));
    ensure!(
        !involved.into_iter().flatten().any(reserved),
        "a removal holds the space or the spaces above it; try again once it ends"
    );
    let repository: Option<RepositoryId> = tx
        .query_row(
            "SELECT repository_id FROM spaces WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let takes = |above: &&SpaceId| super::ensure_parent(tx, repository.as_ref(), id, above).is_ok();
    let above = above.as_ref().filter(takes);
    super::set_parent(tx, id, above)?;
    super::parent(tx, id)
}

#[cfg(test)]
mod tests;
