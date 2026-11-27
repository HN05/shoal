//! The spaces an export leaves here when it carries only some of the spaces
//! below its space. They move up one level, and the requests the leaving
//! spaces decided for them pass on. A move to a joined machine instead
//! leaves them below the spaces they were below, which then run on that
//! machine under the same IDs.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, Transaction, params};
use shoal_ids::repository_ids::RepositoryId;

use crate::{
    model::{Space, SpaceRef},
    spaces::Removing,
    state::WorkspaceState,
    workspace,
};
use shoal_ids::space_ids::SpaceId;

impl Spaces {
    /// Moves them to the space above `space`, or to the top level where that
    /// cannot take one, then passes the pending requests the leaving spaces
    /// decide for those that stay to the space above them.
    pub(super) async fn leave_behind(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        carried: &[Space],
    ) -> Result<()> {
        let leaving = leaving(space, carried);
        let above = space.parent.as_ref().map(|parent| parent.id.clone());
        let moving = leaving.clone();
        let held = [self.space_removals.clone(), self.completed_owners.clone()];
        (self.store)
            .write(move |tx| move_up(tx, &moving, above.as_ref(), &held))
            .await?;
        daemon.coordination().pass_on_from(daemon, &leaving).await
    }

    /// They keep the space they are below once it runs on the joined machine
    /// it moves to, so its removal here leaves them. Each records that space
    /// until [`Self::attach_behind`] links it again.
    pub async fn detach_behind(&self, space: &Space, carried: &[Space]) -> Result<()> {
        let leaving = leaving(space, carried);
        let held = [self.space_removals.clone(), self.completed_owners.clone()];
        (self.store)
            .write(move |tx| detach(tx, &leaving, &held))
            .await
    }

    /// Links each below the space it was below, which runs on another joined
    /// machine now, or still here where its removal failed. Runs once the
    /// removal ended.
    pub(super) async fn attach_behind(&self, space: &Space, carried: &[Space]) {
        let leaving = leaving(space, carried);
        let attached = (self.store).write(move |tx| attach(tx, &leaving));
        if let Err(error) = attached.await {
            crate::log!("link the spaces a move left here again: {error:#}");
        }
    }

    /// As when the daemon stopped during the removal.
    pub async fn attach_settled_behind(&self) -> Result<()> {
        (self.store).write(attach_settled).await
    }
}

fn leaving(space: &Space, carried: &[Space]) -> Vec<SpaceId> {
    std::iter::once(space)
        .chain(carried)
        .map(|space| space.id.clone())
        .collect()
}

/// Moves them below `above`, or to the top level where `above` cannot take
/// one. [`staying`] finds them.
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
            crate::tree::ensure_parent(tx, repository.as_ref(), &space.id, above).is_ok()
        };
        crate::tree::set_parent(tx, &space.id, above.filter(takes))?;
    }
    Ok(())
}

/// Records the space each was below. [`staying`] finds them.
fn detach(tx: &Transaction<'_>, leaving: &[SpaceId], held: &[Removing; 2]) -> Result<()> {
    for (space, parent) in staying(tx, leaving, None, held)? {
        crate::tree::set_parent(tx, &space.id, None)?;
        tx.execute(
            "INSERT INTO moved_parents(space_id,parent_id) VALUES (?1,?2)
                ON CONFLICT(space_id) DO UPDATE SET parent_id=excluded.parent_id",
            params![space.id, parent],
        )?;
    }
    Ok(())
}

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

/// Only those whose parent has no recorded move.
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

fn relink(tx: &Transaction<'_>, space: &SpaceId, parent: &SpaceId) -> Result<()> {
    crate::tree::set_parent(tx, space, Some(parent))?;
    tx.execute("DELETE FROM moved_parents WHERE space_id=?1", [space])?;
    Ok(())
}

/// Each with the space it is below. Refuses while a removal `held` reserves
/// a space it leaves or joins, `above` among them, or one of them is being
/// removed: a removal that listed the spaces below one of those would
/// otherwise miss it.
fn staying(
    tx: &Transaction<'_>,
    leaving: &[SpaceId],
    above: Option<&SpaceId>,
    held: &[Removing; 2],
) -> Result<Vec<(SpaceRef, SpaceId)>> {
    let reserved = |id: &SpaceId| held.iter().any(|removing| removing.reserved(id));
    let mut staying = Vec::new();
    for id in leaving {
        let children = crate::tree::children(tx, id)?;
        let stays = (children.into_iter()).filter(|child| !leaving.contains(&child.id));
        staying.extend(stays.map(|child| (child, id.clone())));
    }
    let involved = (leaving.iter())
        .chain(above)
        .chain(staying.iter().map(|(space, _)| &space.id));
    for id in involved {
        ensure!(
            !reserved(id) && !workspace::is_in_state(tx, id, WorkspaceState::Removing)?,
            "a removal holds the spaces it would leave here; export it once that ends"
        );
    }
    Ok(staying)
}
