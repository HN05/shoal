//! The hub's one sequence of space numbers across its machines.
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
    spares.sort();
    Ok(Numbering {
        spares,
        renumbered: renumbered(tx, member)?,
    })
}

/// Settle the number of `member`'s space `id`, which the directory lists
/// under it now. A spare or new number the space took becomes its listed
/// number, which ends any renumbering. When another machine's space or
/// reservation held the number first, the hub keeps a new one for the
/// space, unless it keeps one already.
pub(super) fn settle(tx: &Transaction<'_>, member: &MemberId, id: &SpaceId) -> Result<()> {
    let number = listed_number(tx, id)?;
    if took_kept(tx, member, id, number)? {
        // As where the machine holds the space's new number without a
        // space and gives it a spare instead.
        return release(tx, member, id);
    }
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
/// ended there or holds another the hub kept for it.
pub(super) fn release(tx: &Transaction<'_>, member: &MemberId, id: &SpaceId) -> Result<()> {
    tx.execute(
        "DELETE FROM hub_numbers WHERE space_id=?1 AND member_id=?2",
        params![id, member],
    )?;
    Ok(())
}

/// Keep `reserved`, every number `member` holds without a space, from
/// any other space.
pub(super) fn reserve_held(
    tx: &Transaction<'_>,
    member: &MemberId,
    reserved: &[SpaceNumber],
) -> Result<()> {
    tx.execute(
        "DELETE FROM hub_reserved_numbers WHERE member_id=?1",
        [member],
    )?;
    for number in reserved {
        tx.execute(
            "INSERT INTO hub_reserved_numbers(member_id, number) VALUES (?1,?2)",
            params![member, number],
        )?;
    }
    Ok(())
}

/// Replace what the hub kept for `member` that `member` now holds without a
/// listed space of its own: a number it reserves for a removed space, or a
/// copy's number. A spare goes, and a space's new number gives way to
/// another.
pub(super) fn replace_held(tx: &Transaction<'_>, member: &MemberId) -> Result<()> {
    let mut statement = tx.prepare(
        "SELECT number, space_id FROM hub_numbers WHERE member_id=?1
            AND number IN (SELECT number FROM hub_held_numbers WHERE member_id=?1)",
    )?;
    let rows = statement.query_map([member], |row| Ok((row.get(0)?, row.get(1)?)));
    let held: Vec<(SpaceNumber, Option<SpaceId>)> = rows?.collect::<rusqlite::Result<_>>()?;
    for (number, space) in held {
        tx.execute("DELETE FROM hub_numbers WHERE number=?1", [number])?;
        if let Some(space) = space {
            let new = smallest_free(tx)?;
            reserve(tx, member, Some(&space), new)?;
        }
    }
    Ok(())
}

/// Drop every number the hub keeps for `member`, as once it revoked it.
pub(super) fn forget(tx: &Transaction<'_>, member: &MemberId) -> Result<()> {
    for forgotten in [
        "DELETE FROM hub_numbers WHERE member_id=?1",
        "DELETE FROM hub_reserved_numbers WHERE member_id=?1",
    ] {
        tx.execute(forgotten, [member])?;
    }
    Ok(())
}

/// Whether `number`, which `member`'s space `id` took, is a spare or the
/// space's new number the hub kept for `member`, which the listing holds
/// from now on.
fn took_kept(
    tx: &Transaction<'_>,
    member: &MemberId,
    id: &SpaceId,
    number: SpaceNumber,
) -> Result<bool> {
    let taken = tx.execute(
        "DELETE FROM hub_numbers WHERE number=?1 AND member_id=?2
            AND (space_id IS NULL OR space_id=?3)",
        params![number, member, id],
    )?;
    Ok(taken > 0)
}

/// The number the directory lists the space `id` with.
fn listed_number(tx: &Transaction<'_>, id: &SpaceId) -> Result<SpaceNumber> {
    of_listed(tx, id)?.ok_or_else(|| anyhow::anyhow!("the hub lists no space {id}"))
}

/// The number the directory lists the space `id` with, if it lists it.
fn of_listed(tx: &Transaction<'_>, id: &SpaceId) -> Result<Option<SpaceNumber>> {
    Ok(tx
        .query_row(
            "SELECT number FROM hub_spaces WHERE space_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Whether something other than `member`'s space `id` holds `number`:
/// another machine's listed space, what the hub keeps for another machine
/// or space, or another machine without a listed space of its own. That
/// machine's copy of this space does not count.
fn held_elsewhere(
    tx: &Transaction<'_>,
    member: &MemberId,
    id: &SpaceId,
    number: SpaceNumber,
) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM hub_spaces
                WHERE number=?1 AND member_id<>?2 AND space_id<>?3
                    AND space_id NOT IN (SELECT space_id FROM hub_numbers
                        WHERE space_id IS NOT NULL))
            OR EXISTS(SELECT 1 FROM hub_numbers
                WHERE number=?1 AND (member_id<>?2 OR space_id<>?3))
            OR EXISTS(SELECT 1 FROM hub_held_numbers WHERE number=?1 AND member_id<>?2
                AND (space_id IS NULL OR space_id<>?3))",
        params![number, member, id],
        |row| row.get(0),
    )?)
}

/// The smallest number no listed space or machine holds and the hub keeps
/// for none.
fn smallest_free(tx: &Transaction<'_>) -> Result<SpaceNumber> {
    Ok(tx.query_row(
        "WITH held(number) AS (
            SELECT number FROM hub_spaces
            UNION ALL SELECT number FROM hub_numbers
            UNION ALL SELECT number FROM hub_held_numbers)
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
