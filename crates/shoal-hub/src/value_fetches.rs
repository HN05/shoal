//! The hub's records of the settings each machine fetched from another's
//! command, never the values: revoking a machine names them to rotate.
use anyhow::Result;
use rusqlite::{Transaction, params};
use shoal_ids::member_ids::MemberId;
use shoal_protocol::{hub::Fetched, time::UnixTime};

/// A machine's fetch of a setting's value from the machine `machine`.
pub struct Fetch {
    pub requester: MemberId,
    pub machine: MemberId,
    pub setting: String,
    pub at: UnixTime,
}

/// Record `fetch`, keeping when the requester first and last fetched the
/// setting from that machine.
pub fn record(tx: &Transaction<'_>, fetch: &Fetch) -> Result<()> {
    tx.execute(
        "INSERT INTO hub_value_fetches(requester,machine,setting,first_at,last_at)
            VALUES (?1,?2,?3,?4,?4)
            ON CONFLICT(requester,machine,setting) DO UPDATE SET last_at=excluded.last_at",
        params![fetch.requester, fetch.machine, fetch.setting, fetch.at],
    )?;
    Ok(())
}

/// The settings `requester` fetched, each from the machine that ran its
/// command, by setting.
pub fn of(tx: &Transaction<'_>, requester: &MemberId) -> Result<Vec<Fetched>> {
    let mut fetched = tx.prepare(
        "SELECT fetches.setting, machines.name, fetches.last_at
            FROM hub_value_fetches fetches JOIN hub_members machines ON machines.id=fetches.machine
            WHERE fetches.requester=?1 ORDER BY fetches.setting, machines.name",
    )?;
    let rows = fetched.query_map([requester], |row| {
        Ok(Fetched {
            setting: row.get(0)?,
            machine: row.get(1)?,
            last_at: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
