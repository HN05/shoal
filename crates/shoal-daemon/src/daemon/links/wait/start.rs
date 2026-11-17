//! Starting a space's watches of its items from a read made now, when it
//! links them and when an agent begins a new session there, so they report
//! only what changes after that: what the items held before is their
//! history, which `shoal view` shows.
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::Result;
use shoal_ids::space_ids::SpaceId;

use super::{
    activity::follows,
    cursor::{cursor_record, save_cursor_record},
    items::followed_items,
};
use crate::{
    daemon::{log, workspace::Manager},
    forge::items::reference::ItemRef,
    time::UnixTime,
};
use shoal_forge::updates::Snapshot;

/// When a poll of item activity began: its time, which the space's own
/// changes are compared with, and its place among the daemon's reads,
/// which tells it from a watch's start read made in the same second.
#[derive(Clone, Copy)]
pub(crate) struct PollStart {
    pub(super) at: UnixTime,
    read: u64,
}

impl PollStart {
    /// Its place among the daemon's reads.
    pub(super) fn read(self) -> u64 {
        self.read
    }
}

/// The daemon's reads of items for watches, numbered in the order they
/// began, and the read each watch last started from. A read in flight
/// ends with the daemon, so none of this outlives it.
#[derive(Default)]
pub(crate) struct WatchStarts {
    reads: AtomicU64,
    started: Mutex<HashMap<(SpaceId, String), u64>>,
}

impl WatchStarts {
    /// The number of a read beginning now.
    fn begin(&self) -> u64 {
        self.reads.fetch_add(1, Ordering::SeqCst)
    }

    /// Note that the watches of the space `id` of `urls` started from `read`.
    fn started(&self, id: &SpaceId, urls: Vec<String>, read: u64) {
        let mut started = self
            .started
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for url in urls {
            started.insert((id.to_owned(), url), read);
        }
    }

    /// Which of `urls` the space `id` started watching from a read that
    /// began after `read`, so a poll that began at `read` may show them
    /// before it.
    pub(super) fn stale<'a>(
        &self,
        id: &SpaceId,
        urls: impl Iterator<Item = &'a String>,
        read: u64,
    ) -> Vec<String> {
        let started = self
            .started
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        urls.filter(|url| {
            started
                .get(&(id.to_owned(), (*url).clone()))
                .is_some_and(|start| *start > read)
        })
        .cloned()
        .collect()
    }
}

/// Which of a space's watches a read starts.
#[derive(Clone, Copy)]
enum Start {
    /// Those that never read their item, as a link's read starts.
    Unread,
    /// Every one, as an agent's new session starts them.
    Every,
}

impl Manager {
    /// The start of a poll of item activity beginning now.
    pub(super) fn poll_start(&self) -> PollStart {
        self.poll_start_at(UnixTime::now())
    }

    /// The start of a poll beginning now as if at `at`.
    pub(super) fn poll_start_at(&self, at: UnixTime) -> PollStart {
        PollStart {
            at,
            read: self.watch_starts.begin(),
        }
    }

    /// Start the watches of `items`, which the space `id` just linked, from
    /// a read of each, unless one already read its item, for a caller
    /// holding the space's PR gate. An item the forge does not read in full
    /// is left to the next poll.
    pub(crate) async fn start_linked_watches_gated(&self, id: &SpaceId, items: Vec<ItemRef>) {
        if let Err(error) = self.start_watches(id, items, Start::Unread).await {
            log!("watches of {id}'s linked items not started: {error:#}");
        }
    }

    /// Start the watch of each item the space `id` follows again from a
    /// read of it made now. An item the forge does not read in full keeps
    /// its watch as it was.
    pub(crate) async fn restart_watches(&self, id: &SpaceId) -> Result<()> {
        let owned = id.to_owned();
        let items = (self.store)
            .run(move |db| followed_items(db, &owned, None))
            .await?;
        let _guard = self.gates.lock_prs([id]).await;
        self.start_watches(id, items, Start::Every).await
    }

    /// Start the watches of `items` on the cursor of the space `id` that
    /// `start` selects from a read of each made now.
    async fn start_watches(&self, id: &SpaceId, items: Vec<ItemRef>, start: Start) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let read = self.poll_start();
        let reads = self.complete_reads(items).await;
        // Checked before recording, as a poll's are.
        let changing = self.assignees_changing(&reads);
        let owned = id.to_owned();
        let started = self
            .store
            .write(move |tx| start_followed(tx, &owned, reads, read.at, start, &changing))
            .await?;
        if started.iter().any(|(_, queued)| *queued) {
            self.signals.item_updates_queued();
        }
        let urls = started.into_iter().map(|(url, _)| url).collect();
        self.watch_starts.started(id, urls, read.read());
        Ok(())
    }
}

/// Start the watches `start` selects of the space `id` from `reads`, made
/// by lookups that began at `read_at`, of the items it still follows,
/// those of `changing` while Shoal changes them on its own. Returns the
/// URL of each started watch and whether it queued an update.
fn start_followed(
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    reads: Vec<(String, Snapshot)>,
    read_at: UnixTime,
    start: Start,
    changing: &[String],
) -> Result<Vec<(String, bool)>> {
    // A workspace links its issue while it is set up.
    crate::daemon::links::ensure_accepts_links(tx, id)?;
    let mut started = Vec::new();
    for (url, snapshot) in reads {
        if !follows(tx, id, &url)? {
            continue;
        }
        let mut record = cursor_record(tx, id, &url)?.unwrap_or_default();
        if matches!(start, Start::Unread) && record.has_read() {
            continue;
        }
        if changing.contains(&url) {
            record.own_change_running(read_at);
        }
        record.start_from(snapshot, &url, read_at);
        save_cursor_record(tx, id, &url, &record)?;
        started.push((url, !record.pending.is_empty()));
    }
    Ok(started)
}

#[cfg(test)]
mod tests;
