//! The hub's one sequence of alert numbers across its machines, which never
//! gives a number twice. It keeps [`SPARES`] spares for each machine, and
//! takes a number a machine's alert took on its own only while no number
//! at or above it was given: any number below the sequence's next went to
//! some machine, so an alert that took one of another machine's gets a new
//! number. A number the hub takes or gives a machine's alert stays the
//! machine's until it reports that it heard.
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use shoal_ids::member_ids::MemberId;
use shoal_protocol::hub::alerts::{AlertNumbering, AlertRenumbering, AlertsReport};

/// How many spare alert numbers the hub keeps for each machine.
const SPARES: usize = 20;

/// Take `member`'s `report` of its alert numbers: take or renumber each it
/// took on its own, drop the spares it used or never heard of, and return
/// the numbers it keeps for the machine, its spares topped up.
pub(crate) fn number(
    tx: &Transaction<'_>,
    member: &MemberId,
    report: &AlertsReport,
) -> Result<AlertNumbering> {
    let mut next = next(tx)?;
    let mut numbering = AlertNumbering::default();
    let mut kept = HashSet::new();
    let mut unconfirmed = report.unconfirmed.clone();
    unconfirmed.sort_unstable();
    unconfirmed.dedup();
    // Confirmed first, so a new number goes above every one confirmed.
    let mut taken = Vec::new();
    for number in unconfirmed {
        if number >= next || keeps(tx, member, number)? {
            keep(tx, member, number, false)?;
            next = next.max(number + 1);
            numbering.confirmed.push(number);
            kept.insert(number);
        } else {
            taken.push(number);
        }
    }
    for from in taken {
        keep(tx, member, next, false)?;
        numbering
            .renumbered
            .push(AlertRenumbering { from, to: next });
        kept.insert(next);
        next += 1;
    }
    let left: HashSet<i64> = report.spares.iter().copied().collect();
    drop_heard(tx, member, &left, &kept)?;
    let mut spares = spares(tx, member)?;
    while spares.len() < SPARES {
        keep(tx, member, next, true)?;
        spares.push(next);
        next += 1;
    }
    tx.execute(
        "INSERT INTO hub_alert_sequence(id, next) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET next=excluded.next",
        [next],
    )?;
    numbering.spares = spares;
    Ok(numbering)
}

/// Drop every number the hub keeps for `member`, as once it revoked it;
/// the sequence goes on past them.
pub(crate) fn forget(tx: &Transaction<'_>, member: &MemberId) -> Result<()> {
    tx.execute("DELETE FROM hub_alert_numbers WHERE member_id=?1", [member])?;
    Ok(())
}

/// The next number of the sequence, which no machine was given.
fn next(db: &Connection) -> Result<i64> {
    let next = db
        .query_row("SELECT next FROM hub_alert_sequence", [], |row| row.get(0))
        .optional()?;
    Ok(next.unwrap_or(1))
}

/// Whether the hub keeps `number` for `member`, as a spare or for an
/// alert of its own.
fn keeps(db: &Connection, member: &MemberId, number: i64) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM hub_alert_numbers WHERE number=?1 AND member_id=?2)",
        params![number, member],
        |row| row.get(0),
    )?)
}

/// Keep `number` for `member`: as a spare, or for one of its alerts.
fn keep(tx: &Transaction<'_>, member: &MemberId, number: i64, spare: bool) -> Result<()> {
    tx.execute(
        "INSERT INTO hub_alert_numbers(number, member_id, spare) VALUES (?1,?2,?3)
         ON CONFLICT(number) DO UPDATE SET spare=excluded.spare",
        params![number, member, spare],
    )?;
    Ok(())
}

/// Drop what the hub keeps for `member` that it no longer needs: the
/// spares not `left` among those the machine reports, which it used or
/// never heard of, and its alerts' numbers it no longer reports, which it
/// heard of, but those it `kept` now.
fn drop_heard(
    tx: &Transaction<'_>,
    member: &MemberId,
    left: &HashSet<i64>,
    kept: &HashSet<i64>,
) -> Result<()> {
    let mut statement =
        tx.prepare("SELECT number, spare FROM hub_alert_numbers WHERE member_id=?1")?;
    let rows = statement.query_map([member], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let held: Vec<(i64, bool)> = rows.collect::<rusqlite::Result<_>>()?;
    for (number, spare) in held {
        let needed = match spare {
            true => left.contains(&number),
            false => kept.contains(&number),
        };
        if !needed {
            tx.execute("DELETE FROM hub_alert_numbers WHERE number=?1", [number])?;
        }
    }
    Ok(())
}

/// The spares the hub keeps for `member`, smallest first.
fn spares(db: &Connection, member: &MemberId) -> Result<Vec<i64>> {
    let mut statement = db.prepare(
        "SELECT number FROM hub_alert_numbers WHERE member_id=?1 AND spare=1 ORDER BY number",
    )?;
    let numbers = statement.query_map([member], |row| row.get(0))?;
    Ok(numbers.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
