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
        self.coordination.pass_on_from(self, &leaving).await
    }

    /// Detach the spaces directly below `space` and the spaces `carried`
    /// that do not go with them, which keep the space they are below once
    /// it runs on the joined machine it moves to, so that its removal here
    /// leaves them, recording each with that space until
    /// [`Self::attach_behind`] links it again.
    pub(super) async fn detach_behind(&self, space: &Space, carried: &[Space]) -> Result<()> {
        let leaving = leaving(space, carried);
        let held = [self.space_removals.clone(), self.completed_owners.clone()];
        (self.store)
            .write(move |tx| detach(tx, &leaving, &held))
            .await
    }

    /// Link each space a move left here below the space it was below again,
    /// which runs on another joined machine now, or still here where its
    /// removal failed: those below `space` and the spaces `carried`, once
    /// their removal ended.
    pub(super) async fn attach_behind(&self, space: &Space, carried: &[Space]) {
        let leaving = leaving(space, carried);
        let attached = (self.store).write(move |tx| attach(tx, &leaving));
        if let Err(error) = attached.await {
            crate::daemon::log!("link the spaces a move left here again: {error:#}");
        }
    }

    /// Link again each space a move left here whose parent no move takes
    /// any longer, as when the daemon stopped during its removal.
    pub(crate) async fn attach_settled_behind(&self) -> Result<()> {
        (self.store).write(attach_settled).await
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
/// them, as [`staying`] finds them, recording the space it was below.
fn detach(tx: &Transaction<'_>, leaving: &[SpaceId], held: &[Removing; 2]) -> Result<()> {
    for (space, parent) in staying(tx, leaving, None, held)? {
        crate::daemon::tree::set_parent(tx, &space.id, None)?;
        tx.execute(
            "INSERT INTO moved_parents(space_id,parent_id) VALUES (?1,?2)
                ON CONFLICT(space_id) DO UPDATE SET parent_id=excluded.parent_id",
            params![space.id, parent],
        )?;
    }
    Ok(())
}

/// Link each space detached from one of `leaving` below it again.
fn attach(tx: &Transaction<'_>, leaving: &[SpaceId]) -> Result<()> {
    for parent in leaving {
        let detached: Vec<SpaceId> = tx
            .prepare("SELECT space_id FROM moved_parents WHERE parent_id=?1")?
            .query_map([parent], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for space in detached {
            relink(tx, &space, parent)?;
        }
    }
    Ok(())
}

/// Link each detached space whose parent has no recorded move below it
/// again.
fn attach_settled(tx: &Transaction<'_>) -> Result<()> {
    let detached: Vec<(SpaceId, SpaceId)> = tx
        .prepare(
            "SELECT space_id,parent_id FROM moved_parents
                WHERE parent_id NOT IN (SELECT space_id FROM space_moves)",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (space, parent) in detached {
        relink(tx, &space, &parent)?;
    }
    Ok(())
}

/// Link the detached `space` below `parent` again.
fn relink(tx: &Transaction<'_>, space: &SpaceId, parent: &SpaceId) -> Result<()> {
    crate::daemon::tree::set_parent(tx, space, Some(parent))?;
    tx.execute("DELETE FROM moved_parents WHERE space_id=?1", [space])?;
    Ok(())
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
