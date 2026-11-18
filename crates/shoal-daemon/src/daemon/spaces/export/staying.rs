//! The spaces an export leaves here when it carries only some of the
//! spaces below its space: they move up one level, below the space above
//! it, and the requests the leaving spaces decided for them pass on.
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, Transaction, params};
use shoal_ids::repository_ids::RepositoryId;

use crate::{
    daemon::{spaces::Removing, store, workspace::Manager},
    model::Space,
};
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
        let held = [self.space_removals.clone(), self.completed_owners.clone()];
        (self.store)
            .write(move |tx| move_up(tx, &moving, above.as_ref(), &held))
            .await?;
        self.pass_on_from(&leaving).await
    }
}

/// Move each space directly below one of `leaving` that is not among them
/// below `above`, or to the top level where `above` cannot take it, once
/// no removal `held` reserves a space it leaves or joins and none of them
/// is being removed: a removal that listed the spaces below one of those
/// would otherwise miss it.
fn move_up(
    tx: &Transaction<'_>,
    leaving: &[SpaceId],
    above: Option<&SpaceId>,
    held: &[Removing; 2],
) -> Result<()> {
    let reserved = |id: &SpaceId| held.iter().any(|removing| removing.reserved(id));
    let mut staying = Vec::new();
    for id in leaving {
        let children = crate::daemon::workspace::children(tx, id)?;
        staying.extend((children.into_iter()).filter(|child| !leaving.contains(&child.id)));
    }
    let involved = (leaving.iter())
        .chain(above)
        .chain(staying.iter().map(|space| &space.id));
    for id in involved {
        ensure!(
            !reserved(id) && !being_removed(tx, id)?,
            "a removal holds the spaces it would leave here; export it once that ends"
        );
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

/// Whether the space `id` is a workspace being removed.
fn being_removed(tx: &Transaction<'_>, id: &SpaceId) -> Result<bool> {
    store::exists(
        tx,
        "SELECT 1 FROM workspaces WHERE id=?1 AND state=?2",
        params![id, crate::state::WorkspaceState::Removing],
    )
}
