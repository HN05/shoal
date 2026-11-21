//! The copy of each shared file a machine last agreed on with its hub, sent
//! or received. Copies agreed with another hub, as before the machine
//! joined this one, count as none.
use std::collections::BTreeMap;

use anyhow::Result;
use rusqlite::{Transaction, params};
use shoal_protocol::hub::{
    PublicKey,
    shared_config::{Copy, FileName},
};

/// The copies agreed with the hub whose key is `hub`, by file.
pub(super) fn load(tx: &Transaction<'_>, hub: &PublicKey) -> Result<BTreeMap<FileName, Copy>> {
    let mut statement =
        tx.prepare("SELECT file, text, revision FROM shared_files WHERE hub_key=?1")?;
    let rows = statement.query_map([hub], |row| {
        Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?))
    })?;
    let mut agreed = BTreeMap::new();
    for row in rows {
        let (file, text, revision) = row?;
        let file: FileName = file.parse()?;
        let copy = Copy {
            file: file.clone(),
            text,
            revision,
        };
        agreed.insert(file, copy);
    }
    Ok(agreed)
}

/// Record `copies` as agreed with the hub whose key is `hub`.
pub(super) fn record(tx: &Transaction<'_>, hub: &PublicKey, copies: &[Copy]) -> Result<()> {
    for Copy {
        file,
        text,
        revision,
    } in copies
    {
        tx.execute(
            "INSERT OR REPLACE INTO shared_files(file, hub_key, text, revision)
                VALUES (?1,?2,?3,?4)",
            params![file.to_string(), hub, text, revision],
        )?;
    }
    Ok(())
}
