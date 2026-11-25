//! The store: the one connection to the state database, which a blocking
//! worker owns, and the transactions operations run in on it.
use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use rusqlite::{Connection, Transaction, TransactionBehavior};
use tokio::sync::watch;

#[cfg(test)]
mod benchmark;
mod worker;

/// Whether the store's commits sync to disk, which tests skip.
#[derive(Clone, Copy)]
pub(crate) enum Syncs {
    Kept,
    Skipped,
}

#[derive(Clone)]
pub struct Store {
    worker: Arc<worker::Worker>,
}

impl Store {
    /// Open persistence and migrate its schema without changing active operations.
    pub async fn open(path: PathBuf) -> Result<Self> {
        let syncs = match cfg!(test) {
            true => Syncs::Skipped,
            false => Syncs::Kept,
        };
        Self::open_with(path, syncs).await
    }

    pub(crate) async fn open_with(path: PathBuf, syncs: Syncs) -> Result<Self> {
        let store = Self {
            worker: worker::Worker::start(path, syncs)?,
        };
        // Not `write`: tests migrate plain connections, so the migration
        // opens its own transaction.
        store
            .run(crate::migrations::migrate)
            .await
            .context("migrate Shoal state database")?;
        Ok(store)
    }

    /// Run once on the connection's blocking worker, waiting for queue space.
    /// Prefer `write` or `read`; this suits an operation that manages its own
    /// transactions or needs none.
    pub async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.worker.run(operation).await
    }

    /// Run once in an immediate transaction that commits when the operation
    /// succeeds and rolls back when it fails.
    pub async fn write<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Transaction<'_>) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.run(|db| write_transaction(db, operation)).await
    }

    /// Run once in a deferred transaction, so every query reads one snapshot.
    pub async fn read<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Transaction<'_>) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.run(|db| {
            let tx = db.transaction()?;
            let value = operation(&tx)?;
            tx.commit()?;
            Ok(value)
        })
        .await
    }

    pub fn watch_changes(&self) -> watch::Receiver<()> {
        self.worker.changed.subscribe()
    }

    /// How many waits watch the store's changes now, which tells a test
    /// that a wait it started waits.
    #[cfg(any(test, feature = "test-support"))]
    pub fn change_watchers(&self) -> usize {
        self.worker.changed.receiver_count()
    }

    pub async fn shutdown(&self) {
        self.worker.shutdown().await;
    }
}

/// Run `operation` in an immediate transaction on `db`, as [`Store::write`]
/// does, for an operation that runs several transactions on the worker.
pub fn write_transaction<T>(
    db: &mut Connection,
    operation: impl FnOnce(&Transaction<'_>) -> Result<T>,
) -> Result<T> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let value = operation(&tx)?;
    tx.commit()?;
    Ok(value)
}

#[cfg(test)]
mod tests;
