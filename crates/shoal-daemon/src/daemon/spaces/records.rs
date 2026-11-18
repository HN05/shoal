//! The row each space has in `spaces`, with what every space has, which its
//! kind's row extends: recording a space, changing its kind and removing it.
//! A space's kind row goes in first, in the same transaction, so that what
//! journals the space's creation reads both.
use anyhow::Result;
use rusqlite::{Transaction, params};
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::space_names::SpaceName;

use crate::{
    config::{modes::ModeName, permissions::PermissionsConfig},
    daemon::{hub::numbering, store},
    model::SpaceKind,
};

/// What a space records in `spaces`.
pub(crate) struct NewSpace<'a> {
    pub id: &'a SpaceId,
    pub kind: SpaceKind,
    pub repository_id: Option<&'a RepositoryId>,
    pub name: &'a SpaceName,
    /// The number it keeps, as a restored space does; 0 takes the next one
    /// of the hub's sequence on a machine of a hub, and else the smallest
    /// one free here.
    pub number: SpaceNumber,
    pub mode: Option<&'a ModeName>,
    pub narrowing: &'a PermissionsConfig,
    /// When it was recorded, which a conversion keeps.
    pub created_at: crate::time::UnixTime,
}

/// Record `space`, whose kind's row is already recorded, and return its
/// number.
pub(crate) fn record(tx: &Transaction<'_>, space: &NewSpace<'_>) -> Result<SpaceNumber> {
    // The trigger that numbers a recorded space gives one recorded with 0
    // the smallest number free.
    let number = match space.number.get() {
        0 => numbering::next(tx, space.id)?.unwrap_or_default(),
        _ => space.number,
    };
    tx.execute(
        "INSERT INTO spaces(id,kind,repository_id,name,number,mode,narrowing,created_at)
         VALUES (?1,?2,?3,?4,NULLIF(?5,0),?6,?7,?8)",
        params![
            space.id,
            space.kind,
            space.repository_id,
            space.name,
            number,
            space.mode,
            store::narrowing_column(space.narrowing)?,
            space.created_at,
        ],
    )?;
    Ok(
        tx.query_row("SELECT number FROM spaces WHERE id=?1", [space.id], |row| {
            row.get(0)
        })?,
    )
}

/// Make the space `id` one of `kind`, whose agent runs in `mode` with the
/// permissions `narrowing` gives it, once its row of that kind is recorded
/// and before its other kind's row goes.
pub(crate) fn convert(
    tx: &Transaction<'_>,
    id: &SpaceId,
    kind: SpaceKind,
    mode: &ModeName,
    narrowing: &PermissionsConfig,
) -> Result<()> {
    tx.execute(
        "UPDATE spaces SET kind=?2,mode=?3,narrowing=?4 WHERE id=?1",
        params![id, kind, mode, store::narrowing_column(narrowing)?],
    )?;
    Ok(())
}

/// Remove the space `id`, with its kind's row and what the tables of spaces
/// hold for it. Returns whether it was recorded.
pub(crate) fn remove(tx: &Transaction<'_>, id: &SpaceId) -> Result<bool> {
    Ok(tx.execute("DELETE FROM spaces WHERE id=?1", [id])? == 1)
}
