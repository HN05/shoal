//! How this machine numbers its alerts in a hub's one sequence of alert
//! numbers, as `docs/design/hub.md` describes. A machine in no hub leaves
//! the numbers to the store.
use anyhow::Result;
use rusqlite::{OptionalExtension as _, Transaction, params};

/// The number a new alert takes from the hub's sequence; none while this
/// machine takes no part in one, where the store numbers it.
pub fn next(tx: &Transaction<'_>) -> Result<Option<i64>> {
    let Some(numbering) = load(tx)? else {
        return Ok(None);
    };
    let number = match free_spare(tx, &numbering)? {
        Some(spare) => spare,
        None => {
            let above = above_known(tx, &numbering)?;
            tx.execute(
                "INSERT OR IGNORE INTO hub_unconfirmed_alerts(number) VALUES (?1)",
                [above],
            )?;
            above
        }
    };
    tx.execute("UPDATE hub_alert_numbering SET taken=?1", [number])?;
    Ok(Some(number))
}

/// Remember that another machine's alert took `number`, which a number
/// this machine takes above its spares then passes.
pub fn saw(tx: &Transaction<'_>, number: i64) -> Result<()> {
    tx.execute(
        "UPDATE hub_alert_numbering SET seen=?1 WHERE seen<?1",
        [number],
    )?;
    Ok(())
}

/// Give the alert `from` the number `to`, unless an alert here holds `to`
/// already; whether it did.
pub fn renumber(tx: &Transaction<'_>, from: i64, to: i64) -> Result<bool> {
    if held(tx, to)? {
        return Ok(false);
    }
    Ok(tx.execute("UPDATE alerts SET id=?2 WHERE id=?1", params![from, to])? > 0)
}

/// What this machine keeps of the hub's sequence.
struct Numbering {
    spares: Vec<i64>,
    taken: i64,
    seen: i64,
}

fn load(tx: &Transaction<'_>) -> Result<Option<Numbering>> {
    let row = tx
        .query_row(
            "SELECT spares, taken, seen FROM hub_alert_numbering",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    row.map(|(spares, taken, seen)| {
        Ok(Numbering {
            spares: serde_json::from_str(&spares)?,
            taken,
            seen,
        })
    })
    .transpose()
}

/// The smallest spare above the highest number taken that no alert here
/// holds, as one recorded before the hub numbered this machine's alerts.
fn free_spare(tx: &Transaction<'_>, numbering: &Numbering) -> Result<Option<i64>> {
    let mut spares = numbering.spares.clone();
    spares.sort_unstable();
    for spare in spares.into_iter().filter(|spare| *spare > numbering.taken) {
        if !held(tx, spare)? {
            return Ok(Some(spare));
        }
    }
    Ok(None)
}

/// The number above every one this machine took, holds, keeps as a spare
/// or saw another machine's alert take.
fn above_known(tx: &Transaction<'_>, numbering: &Numbering) -> Result<i64> {
    let held: i64 = tx.query_row("SELECT COALESCE(MAX(id), 0) FROM alerts", [], |row| {
        row.get(0)
    })?;
    let spare = numbering.spares.iter().copied().max().unwrap_or(0);
    Ok([numbering.taken, numbering.seen, held, spare]
        .into_iter()
        .max()
        .unwrap_or(0)
        + 1)
}

fn held(tx: &Transaction<'_>, number: i64) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM alerts WHERE id=?1)",
        [number],
        |row| row.get(0),
    )?)
}

#[cfg(test)]
mod tests;
