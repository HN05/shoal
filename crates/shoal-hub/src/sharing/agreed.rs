//! The copy of each shared item a machine last agreed on with its hub.
//! Copies agreed with another hub count as none.
use anyhow::Result;
use rusqlite::{Transaction, params};
use shoal_protocol::hub::PublicKey;

use super::{Agreed, Copy, Kind, values};

/// The copies of `S` agreed with the hub whose key is `hub`, by key.
pub(super) fn load<S: Kind>(tx: &Transaction<'_>, hub: &PublicKey) -> Result<Agreed<S>> {
    let mut statement =
        tx.prepare("SELECT key, value, revision FROM shared_items WHERE kind=?1 AND hub_key=?2")?;
    let rows = statement.query_map(params![S::NAME, hub], values::row)?;
    let mut agreed = Agreed::<S>::new();
    for row in rows {
        let copy = values::copy::<S>(row?)?;
        agreed.insert(copy.key.clone(), copy);
    }
    Ok(agreed)
}

/// Record `copies` of `S` as agreed with the hub whose key is `hub`.
pub(super) fn record<S: Kind>(
    tx: &Transaction<'_>,
    hub: &PublicKey,
    copies: &[Copy<S>],
) -> Result<()> {
    for copy in copies {
        tx.execute(
            "INSERT OR REPLACE INTO shared_items(kind, key, hub_key, value, revision)
                VALUES (?1,?2,?3,?4,?5)",
            params![
                S::NAME,
                copy.key.to_string(),
                hub,
                values::encode::<S>(copy.value.as_ref())?,
                copy.revision
            ],
        )?;
    }
    Ok(())
}
