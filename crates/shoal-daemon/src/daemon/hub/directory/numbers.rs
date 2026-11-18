//! The hub's one sequence of space numbers across its machines. A number is
//! held by the listed space that has it, unless the hub renumbers that
//! space, and by the spares and new numbers the hub keeps for a machine. A
//! space whose number another machine's space or reservation held first
//! takes the smallest number none holds, which the hub keeps for it until
//! its machine reports it with that number. A space another machine claims
//! keeps its number there the same way.
use anyhow::Result;
use rusqlite::{OptionalExtension as _, Transaction, params};
use shoal_ids::{
    member_ids::MemberId,
    space_ids::{SpaceId, SpaceNumber},
};
use shoal_protocol::hub::numbering::{Numbering, Renumbering};

/// How many spare numbers the hub keeps for each machine.
const SPARES: usize = 3;

/// The numbers the hub keeps for `member`, its spares topped up to
/// [`SPARES`].
pub(super) fn kept_for(tx: &Transaction<'_>, member: &MemberId) -> Result<Numbering> {
    let mut spares = spares(tx, member)?;
    while spares.len() < SPARES {
        let number = smallest_free(tx)?;
        reserve(tx, member, None, number)?;
        spares.push(number);
    }
    Ok(Numbering {
        spares,
        renumbered: renumbered(tx, member)?,
    })
}

/// Settle the number of `member`'s space `id`, which the directory lists
/// under it now: a spare or new number it took becomes the listing's, and
/// one another machine's space or reservation held first has the hub keep
/// a new one for it, unless it keeps one already.
pub(super) fn settle(tx: &Transaction<'_>, member: &MemberId, id: &SpaceId) -> Result<()> {
    let number = listed_number(tx, id)?;
    tx.execute(
        "DELETE FROM hub_numbers WHERE number=?1 AND member_id=?2
            AND (space_id IS NULL OR space_id=?3)",
        params![number, member, id],
    )?;
    if renumbering(tx, id)?.is_some() || !held_elsewhere(tx, member, id, number)? {
        return Ok(());
    }
    let new = smallest_free(tx)?;
    reserve(tx, member, Some(id), new)
}

/// Keep the number of the space `id` for `member`, which claims it as it
/// imports the space, and return it: its new number, should the hub
/// renumber it, or else the one the directory lists it with, if any.
pub(super) fn keep_for_claim(
    tx: &Transaction<'_>,
    member: &MemberId,
    id: &SpaceId,
) -> Result<Option<SpaceNumber>> {
    let Some(number) = renumbering(tx, id)?.or(of_listed(tx, id)?) else {
        return Ok(None);
    };
    tx.execute(
        "INSERT INTO hub_numbers(number, member_id, space_id) VALUES (?1,?2,?3)
         ON CONFLICT(number) DO UPDATE SET member_id=excluded.member_id,
            space_id=excluded.space_id",
        params![number, member, id],
    )?;
    Ok(Some(number))
}

/// Drop the new number the hub keeps for `member`'s space `id`, which
/// ended there.
pub(super) fn release(tx: &Transaction<'_>, member: &MemberId, id: &SpaceId) -> Result<()> {
    tx.execute(
        "DELETE FROM hub_numbers WHERE space_id=?1 AND member_id=?2",
        params![id, member],
    )?;
    Ok(())
}

/// Drop every number the hub keeps for `member`, as once it revoked it.
pub(super) fn forget(tx: &Transaction<'_>, member: &MemberId) -> Result<()> {
    tx.execute("DELETE FROM hub_numbers WHERE member_id=?1", [member])?;
    Ok(())
}

/// The number the directory lists the space `id` with.
fn listed_number(tx: &Transaction<'_>, id: &SpaceId) -> Result<SpaceNumber> {
    of_listed(tx, id)?.ok_or_else(|| anyhow::anyhow!("the hub lists no space {id}"))
}

/// The number the directory lists the space `id` with, if it lists it.
fn of_listed(tx: &Transaction<'_>, id: &SpaceId) -> Result<Option<SpaceNumber>> {
    Ok(tx
        .query_row(
            "SELECT json_extract(summary,'$.number') FROM hub_spaces WHERE space_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Whether `number`, which `member`'s space `id` has, is held by another
/// machine's listed space that keeps it, or by what the hub keeps for
/// another machine or another space.
fn held_elsewhere(
    tx: &Transaction<'_>,
    member: &MemberId,
    id: &SpaceId,
    number: SpaceNumber,
) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM hub_spaces
                WHERE json_extract(summary,'$.number')=?1 AND member_id<>?2 AND space_id<>?3
                    AND space_id NOT IN (SELECT space_id FROM hub_numbers
                        WHERE space_id IS NOT NULL))
            OR EXISTS(SELECT 1 FROM hub_numbers
                WHERE number=?1 AND (member_id<>?2 OR space_id<>?3))",
        params![number, member, id],
        |row| row.get(0),
    )?)
}

/// The smallest number no listed space holds and the hub keeps for none.
fn smallest_free(tx: &Transaction<'_>) -> Result<SpaceNumber> {
    Ok(tx.query_row(
        "WITH held(number) AS (
            SELECT json_extract(summary,'$.number') FROM hub_spaces
            UNION ALL SELECT number FROM hub_numbers)
        SELECT MIN(n) FROM (SELECT 1 AS n UNION ALL SELECT number+1 FROM held)
            WHERE n NOT IN (SELECT number FROM held)",
        [],
        |row| row.get(0),
    )?)
}

/// Keep `number` for `member`: for its space `space`, or as a spare.
fn reserve(
    tx: &Transaction<'_>,
    member: &MemberId,
    space: Option<&SpaceId>,
    number: SpaceNumber,
) -> Result<()> {
    tx.execute(
        "INSERT INTO hub_numbers(number, member_id, space_id) VALUES (?1,?2,?3)",
        params![number, member, space],
    )?;
    Ok(())
}

/// The new number the hub keeps for the space `id`, if it renumbers it.
fn renumbering(tx: &Transaction<'_>, id: &SpaceId) -> Result<Option<SpaceNumber>> {
    Ok(tx
        .query_row(
            "SELECT number FROM hub_numbers WHERE space_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?)
}

/// The spare numbers the hub keeps for `member`, smallest first.
fn spares(tx: &Transaction<'_>, member: &MemberId) -> Result<Vec<SpaceNumber>> {
    let mut statement = tx.prepare(
        "SELECT number FROM hub_numbers WHERE member_id=?1 AND space_id IS NULL ORDER BY number",
    )?;
    let numbers = statement.query_map([member], |row| row.get(0))?;
    Ok(numbers.collect::<rusqlite::Result<_>>()?)
}

/// The spaces of `member` the hub renumbers, each with its new number.
fn renumbered(tx: &Transaction<'_>, member: &MemberId) -> Result<Vec<Renumbering>> {
    let mut statement = tx.prepare(
        "SELECT space_id, number FROM hub_numbers
            WHERE member_id=?1 AND space_id IS NOT NULL ORDER BY number",
    )?;
    let renumbered = statement.query_map([member], |row| {
        Ok(Renumbering {
            space: row.get(0)?,
            number: row.get(1)?,
        })
    })?;
    Ok(renumbered.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
