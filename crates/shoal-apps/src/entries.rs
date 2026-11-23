//! The store's record of the entries apps keep on this daemon: reading
//! them, and changing one only from the revision its writer read, within
//! the limits of `[machine.app_store]`.
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension as _, Row, Transaction, params};
use shoal_core::config::machine::AppStore;
use shoal_ids::member_ids::MemberId;
use shoal_protocol::apps::{
    AppName,
    store::{AppScope, Entry, EntryKey, KeyPrefix, Revision, Stored, Write},
};

use crate::{
    refusal::{Reason, Refusal},
    time::UnixTime,
};

/// Whose entries a request reads or changes on this daemon.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Holder {
    /// Every machine and device of the user's.
    Shared,
    /// This machine's own.
    Own,
    /// As the hub, a paired device's own.
    Device(MemberId),
}

impl Holder {
    /// Whose entries in `scope` this daemon's own callers reach.
    pub fn own(scope: AppScope) -> Self {
        match scope {
            AppScope::Shared => Self::Shared,
            AppScope::Device => Self::Own,
        }
    }

    /// The scope and device columns of its entries.
    fn columns(&self) -> (AppScope, &str) {
        match self {
            Self::Shared => (AppScope::Shared, ""),
            Self::Own => (AppScope::Device, ""),
            Self::Device(member) => (AppScope::Device, member.as_str()),
        }
    }
}

/// The entries of `app` that `holder` has, as one change leaves them.
pub(crate) struct Entries<'a> {
    holder: &'a Holder,
    app: &'a AppName,
}

const ENTRY: &str = "key, content_type, revision, length(bytes) AS size, updated_at";

fn entry(row: &Row<'_>) -> rusqlite::Result<Entry> {
    Ok(Entry {
        key: row.get("key")?,
        content_type: row.get("content_type")?,
        revision: Revision(row.get::<_, i64>("revision")? as u64),
        size: row.get::<_, i64>("size")? as u64,
        updated_at: row.get("updated_at")?,
    })
}

impl<'a> Entries<'a> {
    pub(crate) fn of(holder: &'a Holder, app: &'a AppName) -> Self {
        Self { holder, app }
    }

    /// The entry under `key`, with its bytes.
    pub(crate) fn read(&self, tx: &Transaction<'_>, key: &EntryKey) -> Result<Option<Stored>> {
        let (scope, device) = self.holder.columns();
        let sql = format!(
            "SELECT {ENTRY}, bytes FROM app_entries WHERE app=?1 AND scope=?2 AND device=?3 AND key=?4"
        );
        let stored = tx.query_row(&sql, params![self.app, scope, device, key], |row| {
            Ok(Stored {
                entry: entry(row)?,
                bytes: row.get("bytes")?,
            })
        });
        Ok(stored.optional()?)
    }

    /// The entries whose keys start with `prefix`, without their bytes, by
    /// key.
    pub(crate) fn list(&self, tx: &Transaction<'_>, prefix: &KeyPrefix) -> Result<Vec<Entry>> {
        let (scope, device) = self.holder.columns();
        let sql = format!(
            "SELECT {ENTRY} FROM app_entries WHERE app=?1 AND scope=?2 AND device=?3 \
             AND substr(key, 1, length(?4)) = ?4 ORDER BY key"
        );
        let mut statement = tx.prepare(&sql)?;
        let rows = statement.query_map(params![self.app, scope, device, prefix], entry)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Refuse a write of `size` bytes as `write` before its bytes come: one
    /// that names a revision the entry is not at, or that would hold more
    /// than `limits` allow.
    pub(crate) fn check_write(
        &self,
        tx: &Transaction<'_>,
        write: &Write,
        size: u64,
        limits: &AppStore,
    ) -> Result<()> {
        let current = self.current(tx, &write.key)?;
        ensure_replaces(&write.key, current.as_ref(), write.replaces)?;
        ensure!(
            size <= limits.max_entry_bytes(),
            "an entry holds at most {} MiB; set [machine.app_store] max_entry_mib to allow more",
            limits.max_entry_mib
        );
        let replaced = current.map_or(0, |entry| entry.size);
        let total = self.app_bytes(tx)? - replaced + size;
        ensure!(
            total <= limits.max_app_bytes(),
            "the entries of {} would hold more than {} MiB; delete some, or set [machine.app_store] max_app_mib to allow more",
            self.app,
            limits.max_app_mib
        );
        Ok(())
    }

    /// Write `bytes` as `write`, refused as [`Self::check_write`] refuses
    /// it, and the entry it leaves.
    pub(crate) fn write(
        &self,
        tx: &Transaction<'_>,
        write: &Write,
        bytes: Vec<u8>,
        limits: &AppStore,
    ) -> Result<Entry> {
        self.check_write(tx, write, bytes.len() as u64, limits)?;
        let (scope, device) = self.holder.columns();
        let revision = next_revision(tx)?;
        tx.execute(
            "INSERT INTO app_entries(app, scope, device, key, content_type, bytes, revision, updated_at) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8) \
             ON CONFLICT(app, scope, device, key) DO UPDATE SET content_type=excluded.content_type, \
             bytes=excluded.bytes, revision=excluded.revision, updated_at=excluded.updated_at",
            params![
                self.app,
                scope,
                device,
                write.key,
                write.content_type,
                bytes,
                revision.0 as i64,
                UnixTime::now()
            ],
        )?;
        Ok(self
            .current(tx, &write.key)?
            .expect("the entry just written"))
    }

    /// Delete the entry under `key`, which must be at `replaces`.
    pub(crate) fn delete(
        &self,
        tx: &Transaction<'_>,
        key: &EntryKey,
        replaces: Revision,
    ) -> Result<()> {
        let current = self.current(tx, key)?;
        ensure_replaces(key, current.as_ref(), Some(replaces))?;
        let (scope, device) = self.holder.columns();
        tx.execute(
            "DELETE FROM app_entries WHERE app=?1 AND scope=?2 AND device=?3 AND key=?4",
            params![self.app, scope, device, key],
        )?;
        next_revision(tx)?;
        Ok(())
    }

    /// The entry under `key`, without its bytes.
    fn current(&self, tx: &Transaction<'_>, key: &EntryKey) -> Result<Option<Entry>> {
        let (scope, device) = self.holder.columns();
        let sql = format!(
            "SELECT {ENTRY} FROM app_entries WHERE app=?1 AND scope=?2 AND device=?3 AND key=?4"
        );
        let found = tx.query_row(&sql, params![self.app, scope, device, key], entry);
        Ok(found.optional()?)
    }

    /// The bytes every entry of the app holds, whoever holds it.
    fn app_bytes(&self, tx: &Transaction<'_>) -> Result<u64> {
        let total: i64 = tx.query_row(
            "SELECT COALESCE(SUM(length(bytes)), 0) FROM app_entries WHERE app=?1",
            [self.app],
            |row| row.get(0),
        )?;
        Ok(total as u64)
    }
}

/// The revision the next change takes, which no change took before.
fn next_revision(tx: &Transaction<'_>) -> Result<Revision> {
    let revision: i64 = tx.query_row(
        "UPDATE app_revisions SET revision = revision + 1 WHERE id = 1 RETURNING revision",
        [],
        |row| row.get(0),
    )?;
    Ok(Revision(revision as u64))
}

/// Refuse a change of the entry under `key`, now `current`, from a writer
/// that read it at `replaces`, none for no entry, unless it still is.
fn ensure_replaces(
    key: &EntryKey,
    current: Option<&Entry>,
    replaces: Option<Revision>,
) -> Result<()> {
    let now = current.map(|entry| entry.revision);
    if now == replaces {
        return Ok(());
    }
    let message = match (now, replaces) {
        (Some(now), Some(read)) => {
            format!("the entry {key} is at revision {now}, not {read}; read it again")
        }
        (Some(now), None) => format!("the entry {key} exists at revision {now}; read it first"),
        (None, _) => format!("the entry {key} no longer exists; read it again"),
    };
    Err(Refusal::new(Reason::StaleRevision, message).into())
}

#[cfg(test)]
mod tests;
