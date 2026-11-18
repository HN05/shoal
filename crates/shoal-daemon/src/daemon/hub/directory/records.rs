//! The hub's records of its machines' spaces: one owner per space ID, and
//! the copies other machines report under it.
use std::collections::HashSet;

use anyhow::{Context as _, Result};
use rusqlite::{OptionalExtension as _, Transaction, params};
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::hub::directory::{Conflict, Listed, Report, SpaceSummary};

use crate::time::UnixTime;

/// Apply `member`'s `report`, and the conflicts it newly raised: the space
/// IDs it reported that another machine owns.
pub(super) fn apply(
    tx: &Transaction<'_>,
    member: &MemberId,
    report: Report,
) -> Result<Vec<Conflict>> {
    let (changed, removed) = match report {
        Report::Whole(spaces) => {
            let reported: HashSet<&SpaceId> = spaces.iter().map(|space| &space.id).collect();
            let gone = (held(tx, member)?.into_iter())
                .filter(|id| !reported.contains(id))
                .collect();
            (spaces, gone)
        }
        Report::Changes { changed, removed } => (changed, removed),
    };
    for space in &removed {
        release(tx, member, space)?;
    }
    let now = UnixTime::now();
    let mut conflicts = Vec::new();
    for space in &changed {
        conflicts.extend(record(tx, member, space, now)?);
    }
    Ok(conflicts)
}

/// Forget every space of `member`, as once the hub revoked it; another
/// machine's copy takes the place of each it owned.
pub(in crate::daemon::hub) fn forget(tx: &Transaction<'_>, member: &MemberId) -> Result<()> {
    for space in held(tx, member)? {
        release(tx, member, &space)?;
    }
    Ok(())
}

/// Every space in the directory, by machine and number, and every copy
/// the directory leaves out.
pub(super) fn read(tx: &Transaction<'_>) -> Result<(Vec<Listed>, Vec<Conflict>)> {
    let mut statement = tx.prepare(
        "SELECT member_id, summary, reported_at FROM hub_spaces ORDER BY member_id, space_id",
    )?;
    let listed = statement.query_map([], |row| {
        Ok((row.get(0)?, row.get::<_, String>(1)?, row.get(2)?))
    })?;
    let mut spaces = Vec::new();
    for row in listed {
        let (machine, summary, reported_at) = row?;
        let space = summary_of(&summary)?;
        spaces.push(Listed {
            machine,
            space,
            reported_at,
        });
    }
    spaces.sort_by_key(|listed| listed.space.number);
    Ok((spaces, conflicts(tx)?))
}

/// The machine that owns the space `id`, if any reported it.
pub(super) fn owner(tx: &Transaction<'_>, id: &SpaceId) -> Result<Option<MemberId>> {
    Ok(tx
        .query_row(
            "SELECT member_id FROM hub_spaces WHERE space_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?)
}

/// The IDs of the spaces `member` reported, owned or copies.
fn held(tx: &Transaction<'_>, member: &MemberId) -> Result<Vec<SpaceId>> {
    let mut statement = tx.prepare(
        "SELECT space_id FROM hub_spaces WHERE member_id=?1
         UNION SELECT space_id FROM hub_space_copies WHERE member_id=?1",
    )?;
    let ids = statement.query_map([member], |row| row.get(0))?;
    Ok(ids.collect::<rusqlite::Result<_>>()?)
}

/// Record `member`'s `space`: as the owner's when no other machine owns
/// its ID, else as a copy, with the conflict a new copy raises.
fn record(
    tx: &Transaction<'_>,
    member: &MemberId,
    space: &SpaceSummary,
    now: UnixTime,
) -> Result<Option<Conflict>> {
    let summary = serde_json::to_string(space).context("write a space summary")?;
    match owner(tx, &space.id)? {
        Some(owner) if &owner != member => {
            let new = !has_copy(tx, &space.id, member)?;
            tx.execute(
                "INSERT INTO hub_space_copies(space_id, member_id, summary, since)
                 VALUES (?1,?2,?3,?4)
                 ON CONFLICT(space_id, member_id) DO UPDATE SET summary=excluded.summary",
                params![space.id, member, summary, now],
            )?;
            Ok(new.then(|| Conflict {
                space: space.id.clone(),
                name: space.name.clone(),
                owner,
                copy: member.clone(),
                since: now,
            }))
        }
        _ => {
            tx.execute(
                "INSERT INTO hub_spaces(space_id, member_id, summary, reported_at)
                 VALUES (?1,?2,?3,?4)
                 ON CONFLICT(space_id) DO UPDATE
                    SET summary=excluded.summary, reported_at=excluded.reported_at",
                params![space.id, member, summary, now],
            )?;
            Ok(None)
        }
    }
}

/// Whether `member` reported a copy of the space `id` before.
fn has_copy(tx: &Transaction<'_>, id: &SpaceId, member: &MemberId) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM hub_space_copies WHERE space_id=?1 AND member_id=?2)",
        params![id, member],
        |row| row.get(0),
    )?)
}

/// Drop `member`'s space `id`, owned or a copy; the earliest copy of one it
/// owned takes its place.
fn release(tx: &Transaction<'_>, member: &MemberId, id: &SpaceId) -> Result<()> {
    tx.execute(
        "DELETE FROM hub_space_copies WHERE space_id=?1 AND member_id=?2",
        params![id, member],
    )?;
    let owned = tx.execute(
        "DELETE FROM hub_spaces WHERE space_id=?1 AND member_id=?2",
        params![id, member],
    )?;
    if owned == 0 {
        return Ok(());
    }
    let earliest: Option<(MemberId, String, UnixTime)> = tx
        .query_row(
            "SELECT member_id, summary, since FROM hub_space_copies WHERE space_id=?1
             ORDER BY since, member_id LIMIT 1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((copy, summary, since)) = earliest {
        tx.execute(
            "DELETE FROM hub_space_copies WHERE space_id=?1 AND member_id=?2",
            params![id, copy],
        )?;
        tx.execute(
            "INSERT INTO hub_spaces(space_id, member_id, summary, reported_at)
             VALUES (?1,?2,?3,?4)",
            params![id, copy, summary, since],
        )?;
    }
    Ok(())
}

/// Every copy the directory leaves out, with the machine that owns its ID.
fn conflicts(tx: &Transaction<'_>) -> Result<Vec<Conflict>> {
    let mut statement = tx.prepare(
        "SELECT c.space_id, c.member_id, c.summary, c.since, s.member_id
         FROM hub_space_copies c JOIN hub_spaces s USING (space_id)
         ORDER BY c.since, c.space_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get::<_, String>(2)?,
            row.get(3)?,
            row.get(4)?,
        ))
    })?;
    let mut conflicts = Vec::new();
    for row in rows {
        let (space, copy, summary, since, owner) = row?;
        conflicts.push(Conflict {
            space,
            name: summary_of(&summary)?.name,
            owner,
            copy,
            since,
        });
    }
    Ok(conflicts)
}

fn summary_of(text: &str) -> Result<SpaceSummary> {
    serde_json::from_str(text).context("read a space summary the hub kept")
}

#[cfg(test)]
mod tests;
