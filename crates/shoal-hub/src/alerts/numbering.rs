//! This machine's side of the hub's sequence of alert numbers.
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::hub::alerts::{AlertNumbering, AlertRenumbering, AlertsReport};

use crate::daemon::Records;

/// An unread alert a space sent that took a new number.
pub(crate) struct Renumbered {
    pub(crate) from: i64,
    pub(crate) to: i64,
    pub(crate) sender: SpaceId,
    pub(crate) message: String,
}

/// What this machine reports of its alerts' numbers: the spares it has
/// not used, and the numbers the hub has not confirmed, which are every
/// alert's until the hub first numbers this machine's alerts.
pub(crate) fn reported(db: &Transaction<'_>) -> Result<AlertsReport> {
    if !numbered_by_hub(db)? {
        return Ok(AlertsReport {
            spares: Vec::new(),
            unconfirmed: numbers(db, "SELECT id FROM alerts ORDER BY id")?,
            users: None,
        });
    }
    let spares = numbers(
        db,
        "SELECT spare.value FROM hub_alert_numbering, json_each(hub_alert_numbering.spares) AS spare
            WHERE spare.value > hub_alert_numbering.taken
                AND spare.value NOT IN (SELECT id FROM alerts)
            ORDER BY spare.value",
    )?;
    let unconfirmed = numbers(
        db,
        "SELECT number FROM hub_unconfirmed_alerts ORDER BY number",
    )?;
    Ok(AlertsReport {
        spares,
        unconfirmed,
        users: None,
    })
}

/// Take `numbering`, the hub's answer to `reported`: keep its spares,
/// drop the numbers it confirmed from those waiting, and give each alert
/// it renumbers its new number, through `records`, which renumbers what
/// names it. An alert recorded since a first report, which the hub did not
/// number yet, waits for the next. Returns the unread alerts spaces sent
/// that took a new number.
pub(crate) fn take(
    tx: &Transaction<'_>,
    records: &dyn Records,
    reported: &AlertsReport,
    numbering: &AlertNumbering,
) -> Result<Vec<Renumbered>> {
    let first = !numbered_by_hub(tx)?;
    keep_spares(tx, &numbering.spares)?;
    if first {
        wait_unreported(tx, reported)?;
    }
    for number in &numbering.confirmed {
        confirmed(tx, *number)?;
    }
    let mut told = Vec::new();
    for renumbering in &numbering.renumbered {
        told.extend(renumber(tx, records, *renumbering)?);
    }
    Ok(told)
}

/// Stop numbering from a hub's sequence, as once this machine neither
/// takes the hub role nor is a member of a hub.
pub(crate) fn forget(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("DELETE FROM hub_alert_numbering; DELETE FROM hub_unconfirmed_alerts;")?;
    Ok(())
}

fn numbered_by_hub(db: &Connection) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM hub_alert_numbering)",
        [],
        |row| row.get(0),
    )?)
}

/// Keep `spares` as the numbers new alerts take, each above every number
/// an alert here holds once the hub first numbers them.
fn keep_spares(tx: &Transaction<'_>, spares: &[i64]) -> Result<()> {
    tx.execute(
        "INSERT INTO hub_alert_numbering(id, spares, taken, seen)
            VALUES (1, ?1, (SELECT COALESCE(MAX(id), 0) FROM alerts), 0)
         ON CONFLICT(id) DO UPDATE SET spares=excluded.spares",
        [serde_json::to_string(spares)?],
    )?;
    Ok(())
}

/// Have the alerts recorded since the first report, which it did not
/// carry, wait for the hub to confirm their numbers.
fn wait_unreported(tx: &Transaction<'_>, reported: &AlertsReport) -> Result<()> {
    let carried: HashSet<i64> = reported.unconfirmed.iter().copied().collect();
    for number in numbers(tx, "SELECT id FROM alerts")? {
        if !carried.contains(&number) {
            tx.execute(
                "INSERT OR IGNORE INTO hub_unconfirmed_alerts(number) VALUES (?1)",
                [number],
            )?;
        }
    }
    Ok(())
}

fn confirmed(tx: &Transaction<'_>, number: i64) -> Result<()> {
    tx.execute(
        "DELETE FROM hub_unconfirmed_alerts WHERE number=?1",
        [number],
    )?;
    Ok(())
}

/// Give the alert numbered `from` the number `to`, unless it is gone, so
/// its number waits no more, or an alert here holds `to`, so it waits for
/// another, and return it when it is an unread alert a space sent, whose
/// agent hears of it.
fn renumber(
    tx: &Transaction<'_>,
    records: &dyn Records,
    AlertRenumbering { from, to }: AlertRenumbering,
) -> Result<Option<Renumbered>> {
    let unread_sent = tx
        .query_row(
            "SELECT space_id, message FROM alerts
                WHERE id=?1 AND read=0 AND sender='space' AND space_id IS NOT NULL",
            [from],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let renumbered = records.renumber_alert(tx, from, to)?;
    if !held(tx, from)? {
        confirmed(tx, from)?;
    }
    if !renumbered {
        return Ok(None);
    }
    // A machine's numbers only grow, as a follow's place in them assumes,
    // so a new alert never takes a spare below the new number.
    tx.execute(
        "UPDATE hub_alert_numbering SET taken=?1 WHERE taken<?1",
        [to],
    )?;
    Ok(unread_sent.map(|(sender, message)| Renumbered {
        from,
        to,
        sender,
        message,
    }))
}

fn held(db: &Connection, number: i64) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM alerts WHERE id=?1)",
        [number],
        |row| row.get(0),
    )?)
}

fn numbers(db: &Connection, query: &str) -> Result<Vec<i64>> {
    let mut statement = db.prepare(query)?;
    let numbers = statement.query_map(params![], |row| row.get(0))?;
    Ok(numbers.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
