//! Watching an app's entries: those under a key prefix as they are, then
//! each change of them, comparing again after each change of the entries
//! this daemon keeps, until whoever watches goes. A watch holds an
//! operation guard only while it writes, so a daemon restart closes it
//! between writes and the client watches again.
use std::{collections::BTreeMap, sync::Arc};

use anyhow::Result;
use shoal_protocol::{
    apps::{
        AppName,
        store::{Change, Entry, EntryKey, KeyPrefix},
    },
    scope_tokens::ScopeToken,
};
use tokio::net::{
    UnixStream,
    unix::{OwnedReadHalf, OwnedWriteHalf},
};

use super::entries::Holder;
use crate::{
    daemon::{answer, manager::Manager},
    protocol::{Body, ErrorCode, Response},
};

/// Where a watch's answer goes.
pub(crate) trait Watcher: Send {
    /// Send `body`; `false` once the watch ends instead, as when its
    /// caller's scope ended or the daemon restarts.
    fn send(&mut self, body: Body) -> impl Future<Output = Result<bool>> + Send;

    /// Return once whoever watches goes.
    fn gone(&mut self) -> impl Future<Output = ()> + Send;
}

/// The entries of an app one watch follows.
pub(crate) struct Watched {
    pub holder: Holder,
    pub app: AppName,
    pub prefix: KeyPrefix,
}

impl Manager {
    /// Send `watched`'s entries to `watcher`, then each change of them,
    /// until it goes.
    pub(crate) async fn watch_kept_entries(
        &self,
        watched: Watched,
        watcher: &mut impl Watcher,
    ) -> Result<()> {
        // Watch before reading, so a change in between still wakes it.
        let mut changes = self.apps.changes();
        let mut sent = self.watched_entries(&watched).await?;
        let snapshot = Body::AppEntries(sent.values().cloned().collect());
        if !watcher.send(snapshot).await? {
            return Ok(());
        }
        loop {
            tokio::select! {
                changed = changes.changed() => changed?,
                () = watcher.gone() => return Ok(()),
            }
            changes.borrow_and_update();
            let current = self.watched_entries(&watched).await?;
            for change in changed(&sent, &current) {
                if !watcher.send(Body::AppChange(change)).await? {
                    return Ok(());
                }
            }
            sent = current;
        }
    }

    /// The entries `watched` covers now, by key.
    async fn watched_entries(&self, watched: &Watched) -> Result<BTreeMap<EntryKey, Entry>> {
        let Watched {
            holder,
            app,
            prefix,
        } = watched;
        let listed = (self.list_kept_entries(holder.clone(), app.clone(), prefix.clone())).await?;
        Ok(listed
            .into_iter()
            .map(|entry| (entry.key.clone(), entry))
            .collect())
    }
}

/// What changed from `before` to `after`: each entry written since, then
/// each one deleted.
fn changed(before: &BTreeMap<EntryKey, Entry>, after: &BTreeMap<EntryKey, Entry>) -> Vec<Change> {
    let written = (after.iter())
        .filter(|(key, entry)| {
            before
                .get(*key)
                .is_none_or(|sent| sent.revision != entry.revision)
        })
        .map(|(_, entry)| Change::Written(entry.clone()));
    let deleted = (before.keys())
        .filter(|key| !after.contains_key(*key))
        .map(|key| Change::Deleted(key.clone()));
    written.chain(deleted).collect()
}

/// A watch on the daemon's socket: the request it answers, and the scope
/// token it came with, which must stay valid while it runs.
pub(crate) struct Socket {
    pub manager: Arc<Manager>,
    pub reader: OwnedReadHalf,
    pub writer: OwnedWriteHalf,
    pub request_id: u64,
    pub scope: Option<ScopeToken>,
}

impl Socket {
    pub(crate) fn new(
        manager: Arc<Manager>,
        stream: UnixStream,
        request_id: u64,
        scope: Option<ScopeToken>,
    ) -> Self {
        let (reader, writer) = stream.into_split();
        Self {
            manager,
            reader,
            writer,
            request_id,
            scope,
        }
    }

    async fn write(&mut self, body: Body) -> Result<()> {
        answer::write_frames(&mut self.writer, Response::new(self.request_id, body)).await
    }
}

impl Watcher for Socket {
    async fn send(&mut self, body: Body) -> Result<bool> {
        // A restarting daemon closes the watch instead, and the client
        // watches again on its replacement.
        let manager = self.manager.clone();
        let Some(_operation) = manager.gates.try_operation() else {
            return Ok(false);
        };
        if !manager.still_scoped(self.scope.as_ref()).await? {
            let refused = Body::error(ErrorCode::ScopeDenied, "expired or unknown workspace scope");
            self.write(refused).await?;
            return Ok(false);
        }
        self.write(body).await?;
        Ok(true)
    }

    async fn gone(&mut self) {
        answer::hangup(&mut self.reader).await
    }
}

#[cfg(test)]
mod tests;
