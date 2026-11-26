//! Watching an app's entries under a key prefix.
use std::collections::BTreeMap;

use anyhow::Result;
use shoal_protocol::{
    apps::{
        AppName,
        store::{Change, Entry, EntryKey, KeyPrefix},
    },
    protocol::Body,
};

use crate::{Apps, Holder};

/// Where a watch's answer goes.
pub trait Watcher: Send {
    /// Send `body`; `false` once the watch ends instead, as when its
    /// caller's scope ended or the daemon restarts.
    fn send(&mut self, body: Body) -> impl Future<Output = Result<bool>> + Send;

    /// Return once whoever watches goes.
    fn gone(&mut self) -> impl Future<Output = ()> + Send;
}

/// The entries of an app one watch follows.
pub struct Watched {
    pub holder: Holder,
    pub app: AppName,
    pub prefix: KeyPrefix,
}

impl Apps {
    /// Send `watched`'s entries to `watcher`, then each change of them,
    /// until it goes.
    pub async fn watch_entries(&self, watched: Watched, watcher: &mut impl Watcher) -> Result<()> {
        // Watch before reading, so a change in between still wakes it.
        let mut changes = self.changes();
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
        let listed = (self.list_entries(holder.clone(), app.clone(), prefix.clone())).await?;
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

#[cfg(test)]
mod tests;
