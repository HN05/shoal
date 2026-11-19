//! The entries this daemon keeps: reading them, and changing them as one
//! store transaction each.
use anyhow::Result;
use shoal_protocol::apps::{
    AppName,
    store::{Entry, EntryKey, KeyPrefix, Revision, Stored, Write},
};

use super::entries::{Entries, Holder};
use crate::daemon::manager::Manager;

impl Manager {
    /// The entry of `app` under `key` that `holder` has, with its bytes.
    pub(crate) async fn read_kept_entry(
        &self,
        holder: Holder,
        app: AppName,
        key: EntryKey,
    ) -> Result<Option<Stored>> {
        (self.store)
            .read(move |tx| Entries::of(&holder, &app).read(tx, &key))
            .await
    }

    /// The entries of `app` that `holder` has whose keys start with
    /// `prefix`, without their bytes.
    pub(crate) async fn list_kept_entries(
        &self,
        holder: Holder,
        app: AppName,
        prefix: KeyPrefix,
    ) -> Result<Vec<Entry>> {
        (self.store)
            .read(move |tx| Entries::of(&holder, &app).list(tx, &prefix))
            .await
    }

    /// Refuse a write of `size` bytes as `write` before its bytes come, as
    /// writing them now would be refused.
    pub(crate) async fn check_kept_write(
        &self,
        holder: Holder,
        app: AppName,
        write: Write,
        size: u64,
    ) -> Result<()> {
        let limits = self.config().machine.app_store;
        (self.store)
            .read(move |tx| Entries::of(&holder, &app).check_write(tx, &write, size, &limits))
            .await
    }

    /// Write `bytes` as `write` to the entries of `app` that `holder` has,
    /// and the entry it leaves.
    pub(crate) async fn write_kept_entry(
        &self,
        holder: Holder,
        app: AppName,
        write: Write,
        bytes: Vec<u8>,
    ) -> Result<Entry> {
        let limits = self.config().machine.app_store;
        (self.store)
            .write(move |tx| Entries::of(&holder, &app).write(tx, &write, bytes, &limits))
            .await
    }

    /// Delete the entry of `app` under `key` that `holder` has, which must
    /// be at `replaces`.
    pub(crate) async fn delete_kept_entry(
        &self,
        holder: Holder,
        app: AppName,
        key: EntryKey,
        replaces: Revision,
    ) -> Result<()> {
        (self.store)
            .write(move |tx| Entries::of(&holder, &app).delete(tx, &key, replaces))
            .await
    }
}
