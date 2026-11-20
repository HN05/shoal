//! The spaces an export leaves here when it carries only some of the
//! spaces below its space: they move up one level, below the space above
//! it, and the requests the leaving spaces decided for them pass on. A
//! move to a joined machine leaves them below the spaces they were below,
//! which run on that machine then, under the same IDs.
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, Transaction, params};
use shoal_ids::repository_ids::RepositoryId;

use crate::{
    daemon::{manager::Manager, spaces::Removing, store},
    model::{Space, SpaceRef},
};
use shoal_ids::space_ids::SpaceId;

/// A space a move to a joined machine leaves here, detached from the
/// space it is below while the removal here of that space runs.
pub(super) struct Detached {
    space: SpaceId,
    parent: SpaceId,
}

impl Manager {
    /// Move the spaces directly below `space` and the spaces `carried` that
    /// do not go with them up to the space above `space`, or to the top
    /// level where that cannot take one, then pass the pending requests the
    /// leaving spaces decide for those that stay on above them.
    pub(super) async fn leave_behind(&self, space: &Space, carried: &[Space]) -> Result<()> {
        let leaving = leaving(space, carried);
        let above = space.parent.as_ref().map(|parent| parent.id.clone());
        let moving = leaving.clone();
        let held = [self.space_removals.clone(), self.completed_owners.clone()];
        (self.store)
            .write(move |tx| move_up(tx, &moving, above.as_ref(), &held))
            .await?;
        self.pass_on_from(&leaving).await
    }

    /// Detach the spaces directly below `space` and the spaces `carried`
    /// that do not go with them, which keep the space they are below once
    /// it runs on the joined machine it moves to, so that its removal here
    /// leaves them; [`Self::attach_behind`] links them to it again.
    pub(super) async fn detach_behind(
        &self,
        space: &Space,
        carried: &[Space],
    ) -> Result<Vec<Detached>> {
        let leaving = leaving(space, carried);
        let held = [self.space_removals.clone(), self.completed_owners.clone()];
        (self.store)
            .write(move |tx| detach(tx, &leaving, &held))
            .await
    }

    /// Link each of the `detached` spaces below the space it was below
    /// again, which runs on another joined machine now, or still here when
    /// its removal failed.
    pub(super) async fn attach_behind(&self, detached: Vec<Detached>) {
        let attached = (self.store).write(move |tx| {
            for Detached { space, parent } in &detached {
                crate::daemon::tree::set_parent(tx, space, Some(parent))?;
            }
            Ok(())
        });
        if let Err(error) = attached.await {
            crate::daemon::log!("link the spaces a move left here again: {error:#}");
        }
    }
}

/// The IDs of `space` and the spaces `carried` that go with it.
fn leaving(space: &Space, carried: &[Space]) -> Vec<SpaceId> {
    std::iter::once(space)
        .chain(carried)
        .map(|space| space.id.clone())
        .collect()
}

/// Move each space directly below one of `leaving` that is not among them
/// below `above`, or to the top level where `above` cannot take it, as
/// [`staying`] finds them.
fn move_up(
    tx: &Transaction<'_>,
    leaving: &[SpaceId],
    above: Option<&SpaceId>,
    held: &[Removing; 2],
) -> Result<()> {
    for (space, _) in staying(tx, leaving, above, held)? {
        let repository: Option<RepositoryId> = tx
            .query_row(
                "SELECT repository_id FROM spaces WHERE id=?1",
                [&space.id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let takes = |above: &&SpaceId| {
            crate::daemon::tree::ensure_parent(tx, repository.as_ref(), &space.id, above).is_ok()
        };
        crate::daemon::tree::set_parent(tx, &space.id, above.filter(takes))?;
    }
    Ok(())
}

/// Detach each space directly below one of `leaving` that is not among
/// them, as [`staying`] finds them, with the space it was below.
fn detach(
    tx: &Transaction<'_>,
    leaving: &[SpaceId],
    held: &[Removing; 2],
) -> Result<Vec<Detached>> {
    let mut detached = Vec::new();
    for (space, parent) in staying(tx, leaving, None, held)? {
        crate::daemon::tree::set_parent(tx, &space.id, None)?;
        detached.push(Detached {
            space: space.id,
            parent,
        });
    }
    Ok(detached)
}

/// Each space directly below one of `leaving` that is not among them, with
/// the one it is below, once no removal `held` reserves a space it leaves
/// or joins, `above` among them, and none of them is being removed: a
/// removal that listed the spaces below one of those would otherwise miss
/// it.
fn staying(
    tx: &Transaction<'_>,
    leaving: &[SpaceId],
    above: Option<&SpaceId>,
    held: &[Removing; 2],
) -> Result<Vec<(SpaceRef, SpaceId)>> {
    let reserved = |id: &SpaceId| held.iter().any(|removing| removing.reserved(id));
    let mut staying = Vec::new();
    for id in leaving {
        let children = crate::daemon::tree::children(tx, id)?;
        let stays = (children.into_iter()).filter(|child| !leaving.contains(&child.id));
        staying.extend(stays.map(|child| (child, id.clone())));
    }
    let involved = (leaving.iter())
        .chain(above)
        .chain(staying.iter().map(|(space, _)| &space.id));
    for id in involved {
        ensure!(
            !reserved(id) && !being_removed(tx, id)?,
            "a removal holds the spaces it would leave here; export it once that ends"
        );
    }
    Ok(staying)
}

/// Whether the space `id` is a workspace being removed.
fn being_removed(tx: &Transaction<'_>, id: &SpaceId) -> Result<bool> {
    store::exists(
        tx,
        "SELECT 1 FROM workspaces WHERE id=?1 AND state=?2",
        params![id, crate::state::WorkspaceState::Removing],
    )
}
