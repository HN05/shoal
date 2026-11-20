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
use tokio::sync::watch;

use super::{
    activity::follows,
    cursor::{cursor_record, save_cursor_record},
    items::followed_items,
    merge_runs::{MergeTarget, follow_reported_merge},
};
use crate::{
    daemon::{log, manager::Manager},
    forge::{items::reference::ItemRef, updates::UpdateKind},
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

/// A read of a space's items, made for their watches to start from.
pub(crate) struct StartRead {
    pub(super) start: PollStart,
    pub(super) reads: Vec<(String, Snapshot)>,
}

/// Which of a space's watches a read starts.
#[derive(Clone, Copy)]
pub(super) enum Start {
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
        let read = self.read_for_start(items).await;
        if let Err(error) = self.record_watch_start(id, read, Start::Unread).await {
            log!("watches of {id}'s linked items not started: {error:#}");
        }
    }

    /// Start each watch of the space `id` again from a read of its items, as
    /// for an agent that begins a new session there. It reads them holding
    /// the space's PR gate, so no poll records them meanwhile and the read
    /// is the latest when recorded. A `stop` asked for while it waits for the
    /// gate or reads, as by a removal that holds the gate, ends it; once it
    /// has read, it records the read and the read's place among the polls in
    /// full, which later polls depend on.
    pub(crate) async fn restart_watches(
        &self,
        id: &SpaceId,
        mut stop: watch::Receiver<bool>,
    ) -> Result<()> {
        let gated = async {
            let gate = self.gates.lock_prs([id]).await;
            anyhow::Ok((gate, self.read_followed(id).await?))
        };
        let (_gate, read) = tokio::select! {
            gated = gated => gated?,
            _ = stop.wait_for(|stop| *stop) => return Ok(()),
        };
        self.record_watch_start(id, read, Start::Every).await
    }

    /// Read each item the space `id` follows, for its watch to start again
    /// from.
    async fn read_followed(&self, id: &SpaceId) -> Result<StartRead> {
        let owned = id.to_owned();
        let items = (self.store)
            .run(move |db| followed_items(db, &owned, None))
            .await?;
        Ok(self.read_for_start(items).await)
    }

    /// A read of `items` made now, leaving out those the forge does not read
    /// in full.
    async fn read_for_start(&self, items: Vec<ItemRef>) -> StartRead {
        let start = self.poll_start();
        let reads = self.complete_reads(items).await;
        StartRead { start, reads }
    }

    /// Start the watches `start` selects of the space `id` from `read`, and
    /// note the read they started from, for a caller holding the space's PR
    /// gate.
    pub(super) async fn record_watch_start(
        &self,
        id: &SpaceId,
        read: StartRead,
        start: Start,
    ) -> Result<()> {
        let StartRead {
            start: began,
            reads,
        } = read;
        if reads.is_empty() {
            return Ok(());
        }
        // Checked before recording, as a poll's are.
        let starting = Starting {
            read_at: began.at,
            start,
            changing: self.assignees_changing(&reads),
            merges: self.merge_target(id, &reads).await?,
        };
        let owned = id.to_owned();
        let started = self
            .store
            .write(move |tx| start_followed(tx, &owned, reads, &starting))
            .await?;
        if started.iter().any(|(_, queued)| *queued) {
            self.signals.item_updates_queued();
        }
        let urls = started.into_iter().map(|(url, _)| url).collect();
        self.watch_starts.started(id, urls, began.read());
        Ok(())
    }
}

/// What a read that starts a space's watches looked at.
struct Starting {
    /// When the read began.
    read_at: UnixTime,
    /// Which watches it starts.
    start: Start,
    /// The items Shoal was changing on its own as the read was recorded.
    changing: Vec<String>,
    /// Where the merges the read saw are followed, when they are.
    merges: Option<MergeTarget>,
}

/// Start the watches `starting` selects of the space `id` from `reads`, of
/// the items it still follows, following the merges they newly report as a
/// poll's do. Returns the URL of each started watch and whether it queued
/// an update.
fn start_followed(
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    reads: Vec<(String, Snapshot)>,
    starting: &Starting,
) -> Result<Vec<(String, bool)>> {
    // A workspace links its issue while it is set up.
    shoal_links::records::ensure_accepts_links(tx, id)?;
    let mut started = Vec::new();
    for (url, snapshot) in reads {
        if !follows(tx, id, &url)? {
            continue;
        }
        let mut record = cursor_record(tx, id, &url)?.unwrap_or_default();
        if matches!(starting.start, Start::Unread) && record.has_read() {
            continue;
        }
        if starting.changing.contains(&url) {
            record.own_change_running(starting.read_at);
        }
        let replayed = (record.pending.iter()).any(|update| update.kind == UpdateKind::Merged);
        record.start_from(snapshot, &url, starting.read_at);
        if let Some(target) = starting.merges.as_ref().filter(|_| !replayed) {
            follow_reported_merge(tx, id, &url, &mut record, target, UnixTime::now())?;
        }
        save_cursor_record(tx, id, &url, &record)?;
        started.push((url, !record.pending.is_empty()));
    }
    Ok(started)
}

#[cfg(test)]
mod tests;
