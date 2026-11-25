//! Starting a space's watches of its items from a read made now, when it
//! links them and when an agent begins a new session there, so they report
//! only what changes after that: what the items held before is their
//! history, which `shoal view` shows.
use anyhow::Result;
use shoal_daemon_base::log;
use shoal_forge::updates::Snapshot;
use shoal_forge_types::{items::reference::ItemRef, updates::UpdateKind};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::time::UnixTime;
use tokio::sync::watch;

use super::{
    cursor::{cursor_record, save_cursor_record},
    followed::{followed_items, follows},
    merge_lookups::{MergeTarget, follow_reported_merge},
    starts::PollStart,
};
use crate::{
    Links,
    daemon::{Daemon, Records},
};

/// A read of a space's items, made for their watches to start from.
pub struct StartRead {
    pub start: PollStart,
    pub reads: Vec<(String, Snapshot)>,
}

/// Which of a space's watches a read starts.
#[derive(Clone, Copy)]
pub enum Start {
    /// Those that never read their item, as a link's read starts.
    Unread,
    /// Every one, as an agent's new session starts them.
    Every,
}

impl Links {
    /// The URLs of the `observations` whose assignees Shoal is changing
    /// on its own.
    pub fn assignees_changing(&self, observations: &[(String, Snapshot)]) -> Vec<String> {
        (observations.iter())
            .map(|(url, _)| url)
            .filter(|url| self.gates.issue_assignees_changing(url))
            .cloned()
            .collect()
    }

    /// Start the watches of `items`, which the space `id` just linked, from
    /// a read of each, unless one already read its item, for a caller
    /// holding the space's PR gate. An item the forge does not read in full
    /// is left to the next poll.
    pub async fn start_linked_watches_gated(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        items: Vec<ItemRef>,
    ) {
        let read = self.read_for_start(items).await;
        if let Err(error) = self
            .record_watch_start(daemon, id, read, Start::Unread)
            .await
        {
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
    pub async fn restart_watches(
        &self,
        daemon: &dyn Daemon,
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
        self.record_watch_start(daemon, id, read, Start::Every)
            .await
    }

    /// Read each item the space `id` follows, for its watch to start again
    /// from.
    async fn read_followed(&self, id: &SpaceId) -> Result<StartRead> {
        let owned = id.to_owned();
        let items = (self.store)
            .read(move |db| followed_items(db, &owned, None))
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
    pub async fn record_watch_start(
        &self,
        daemon: &dyn Daemon,
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
            merges: self.merge_target(daemon, id, &reads).await?,
        };
        let (owned, records) = (id.to_owned(), self.records.clone());
        let started = self
            .store
            .write(move |tx| start_followed(&*records, tx, &owned, reads, &starting))
            .await?;
        if started.iter().any(|(_, queued)| *queued) {
            daemon.item_updates_queued();
        }
        let urls = started.into_iter().map(|(url, _)| url).collect();
        self.watch_starts().started(id, urls, began.read());
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
    records: &dyn Records,
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    reads: Vec<(String, Snapshot)>,
    starting: &Starting,
) -> Result<Vec<(String, bool)>> {
    // A workspace links its issue while it is set up.
    records.ensure_accepts_links(tx, id)?;
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
