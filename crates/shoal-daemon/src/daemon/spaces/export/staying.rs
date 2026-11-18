//! The spaces an export leaves here when it carries only some of the
//! spaces below its space: they move up one level, below the space above
//! it, and the requests the leaving spaces decided for them pass on.
use anyhow::Result;
use rusqlite::{OptionalExtension, Transaction};
use shoal_ids::repository_ids::RepositoryId;

use crate::{daemon::workspace::Manager, model::Space};
use shoal_ids::space_ids::SpaceId;

impl Manager {
    /// Move the spaces directly below `space` and the spaces `carried` that
    /// do not go with them up to the space above `space`, or to the top
    /// level where that cannot take one, then pass the pending requests the
    /// leaving spaces decide for those that stay on above them.
    pub(super) async fn leave_behind(&self, space: &Space, carried: &[Space]) -> Result<()> {
        let leaving: Vec<SpaceId> = std::iter::once(space)
            .chain(carried)
            .map(|space| space.id.clone())
            .collect();
        let above = space.parent.as_ref().map(|parent| parent.id.clone());
        let moving = leaving.clone();
        (self.store)
            .write(move |tx| move_up(tx, &moving, above.as_ref()))
            .await?;
        self.pass_on_from(&leaving).await
    }
}

/// Move each space directly below one of `leaving` that is not among them
/// below `above`, or to the top level where `above` cannot take it.
fn move_up(tx: &Transaction<'_>, leaving: &[SpaceId], above: Option<&SpaceId>) -> Result<()> {
    let mut staying = Vec::new();
    for id in leaving {
        let children = crate::daemon::workspace::children(tx, id)?;
        staying.extend((children.into_iter()).filter(|child| !leaving.contains(&child.id)));
    }
    for space in staying {
        let repository: Option<RepositoryId> = tx
            .query_row(
                "SELECT repository_id FROM spaces WHERE id=?1",
                [&space.id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let takes = |above: &&SpaceId| {
            crate::daemon::workspace::ensure_parent(tx, repository.as_ref(), &space.id, above)
                .is_ok()
        };
        crate::daemon::workspace::set_parent(tx, &space.id, above.filter(takes))?;
    }
    Ok(())
}
