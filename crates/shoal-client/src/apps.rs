//! Apps built on Shoal: the settings the user gives each in its
//! `[apps.<name>]` tables, and the entries each keeps in the app store.
//! A write names the revision it replaces, and [`stale`] tells its refusal
//! once another change took the entry past it.
use anyhow::Result;
use shoal_protocol::{
    apps::{
        AppName,
        store::{
            AppData, AppScope, Entry, EntryKey, EntryPiece, KeyPrefix, Revision, Stored, Write,
        },
    },
    protocol::{self, AppMethod, ConfigTarget, ErrorCode, RemoteError},
};

use crate::connection::{self, Daemon, pages, request};

/// The table `[apps.<app>]` resolves to for `target` after every layer,
/// none where no layer sets it.
pub async fn config(
    daemon: &impl Daemon,
    app: AppName,
    target: ConfigTarget,
) -> Result<Option<toml::Table>> {
    request(daemon, AppMethod::ReadAppConfig { app, target }).await
}

/// The entry of `app` in `scope` under `key`, with its bytes.
pub async fn read(
    daemon: &impl Daemon,
    app: AppName,
    scope: AppScope,
    key: EntryKey,
) -> Result<Option<Stored>> {
    let pieces = request(daemon, AppMethod::ReadAppEntry { app, scope, key }).await?;
    EntryPiece::join(pieces)
}

/// The entries of `app` in `scope` whose keys start with `prefix`, without
/// their bytes, by key.
pub async fn list(
    daemon: &impl Daemon,
    app: AppName,
    scope: AppScope,
    prefix: KeyPrefix,
) -> Result<Vec<Entry>> {
    pages(daemon, AppMethod::ListAppEntries { app, scope, prefix }).await
}

/// Write `bytes` to the entry of `app` in `scope` that `write` names, from
/// the revision it replaces; the entry at its new revision.
pub async fn write(
    daemon: &impl Daemon,
    app: AppName,
    scope: AppScope,
    write: Write,
    bytes: &[u8],
) -> Result<Entry> {
    let size = bytes.len() as u64;
    let method = AppMethod::WriteAppEntry {
        app,
        scope,
        write,
        size,
    };
    let (mut stream, ready) = connection::open_buffered(daemon, method).await?;
    <()>::try_from(ready)?;
    for piece in AppData::split(bytes) {
        protocol::write(stream.get_mut(), &piece).await?;
    }
    let written = protocol::read_response(&mut stream).await?;
    Entry::try_from(written.body)
}

/// Delete the entry of `app` in `scope` under `key`, which must be at
/// `replaces`.
pub async fn delete(
    daemon: &impl Daemon,
    app: AppName,
    scope: AppScope,
    key: EntryKey,
    replaces: Revision,
) -> Result<()> {
    let method = AppMethod::DeleteAppEntry {
        app,
        scope,
        key,
        replaces,
    };
    request(daemon, method).await
}

/// Whether `error` refused a write or deletion whose revision another
/// change of the entry took it past: read the entry again and write from
/// there.
pub fn stale(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<RemoteError>()
        .is_some_and(|remote| remote.code == ErrorCode::StaleRevision)
}

#[cfg(test)]
mod tests;
