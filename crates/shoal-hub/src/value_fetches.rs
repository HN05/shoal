//! The hub's records of the settings each machine fetched from another's
//! command, never the values: revoking a machine names them to rotate.
use anyhow::Result;
use rusqlite::{Transaction, params};
use shoal_ids::member_ids::MemberId;
use shoal_protocol::time::UnixTime;

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
