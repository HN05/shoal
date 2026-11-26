//! The settings and app store entries of apps built on Shoal. A write names
//! the revision it replaces, and [`stale`] tells its refusal once another
//! change took the entry past it.
use anyhow::Result;
use shoal_protocol::{
    apps::{
        AppName,
        store::{
            AppData, AppScope, Change, Entry, EntryKey, EntryPiece, KeyPrefix, Revision, Stored,
            Write,
        },
    },
    protocol::{self, AppMethod, Body, ConfigTarget, ErrorCode, RemoteError, Responses},
};
use tokio::{io::BufReader, net::UnixStream};

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

/// What a [`Watch`] reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Watched {
    /// Every entry it covers, first and again once it watches anew, as
    /// after a daemon restart or while the hub restarts, which `resynced`
    /// says.
    Entries {
        entries: Vec<Entry>,
        resynced: bool,
    },
    Changed(Change),
}

/// A watch of the entries of an app under a key prefix, on a connection of
/// its own: their snapshot, then each change, and a new snapshot once the
/// watch reconnected after a daemon restart.
pub struct Watch<D> {
    daemon: D,
    watched: (AppName, AppScope, KeyPrefix),
    stream: Option<Responses<BufReader<UnixStream>>>,
    /// The first snapshot, until [`Self::next`] returns it.
    first: Option<Vec<Entry>>,
}

impl<D: Daemon> Watch<D> {
    /// Watch the entries of `app` in `scope` whose keys start with
    /// `prefix`, failing at once when the daemon refuses.
    pub async fn open(daemon: D, app: AppName, scope: AppScope, prefix: KeyPrefix) -> Result<Self> {
        let watched = (app, scope, prefix);
        let (stream, entries) = watch(&daemon, &watched).await?;
        Ok(Self {
            daemon,
            watched,
            stream: Some(stream),
            first: Some(entries),
        })
    }

    /// The next update. Cancelling it, as `tokio::select!` does, loses no
    /// update.
    pub async fn next(&mut self) -> Result<Watched> {
        if let Some(entries) = self.first.take() {
            return Ok(Watched::Entries {
                entries,
                resynced: false,
            });
        }
        loop {
            let Some(stream) = &mut self.stream else {
                let watching = connection::after_restart(|| watch(&self.daemon, &self.watched));
                let (stream, entries) = watching.await?;
                self.stream = Some(stream);
                return Ok(Watched::Entries {
                    entries,
                    resynced: true,
                });
            };
            // The hub's link breaking off a watch of shared entries ends it
            // as a daemon restart does.
            match stream
                .next()
                .await
                .and_then(|response| response.body.into_result())
            {
                Ok(Body::AppChange(change)) => return Ok(Watched::Changed(change)),
                Ok(body) => return Err(body.unexpected("AppChange")),
                Err(error)
                    if connection::stream_closed(&error)
                        || connection::machine_unreachable(&error) =>
                {
                    self.stream = None
                }
                Err(error) => return Err(error),
            }
        }
    }
}

async fn watch(
    daemon: &impl Daemon,
    (app, scope, prefix): &(AppName, AppScope, KeyPrefix),
) -> Result<(Responses<BufReader<UnixStream>>, Vec<Entry>)> {
    let method = AppMethod::WatchAppEntries {
        app: app.clone(),
        scope: *scope,
        prefix: prefix.clone(),
    };
    let (stream, body) = connection::open_buffered(daemon, method).await?;
    Ok((Responses::new(stream), Vec::try_from(body)?))
}

#[cfg(test)]
mod tests;
