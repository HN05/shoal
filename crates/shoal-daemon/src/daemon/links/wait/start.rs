//! Starting a space's watches of its items from a read made now, when it
//! links them and when an agent begins a new session there, so they report
//! only what changes after that: what the items held before is their
//! history, which `shoal view` shows.
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

/// Which of a space's watches a read starts.
#[derive(Clone, Copy)]
enum Start {
    /// Those that never read their item, as a link's read starts.
    Unread,
    /// Every one, as an agent's new session starts them.
    Every,
}

impl Manager {
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
        let read_at = UnixTime::now();
        let reads = self.complete_reads(items).await;
        let id = id.to_owned();
        let started = self
            .store
            .write(move |tx| start_followed(tx, &id, reads, read_at, start))
            .await?;
        if started {
            self.signals.item_updates_queued();
        }
        Ok(())
    }
}

/// Start the watches `start` selects of the space `id` from `reads`, made
/// by lookups that began at `read_at`, of the items it still follows.
/// Returns whether one queued an update.
fn start_followed(
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    reads: Vec<(String, Snapshot)>,
    read_at: UnixTime,
    start: Start,
) -> Result<bool> {
    // A workspace links its issue while it is set up.
    crate::daemon::links::ensure_accepts_links(tx, id)?;
    let mut queued = false;
    for (url, snapshot) in reads {
        if !follows(tx, id, &url)? {
            continue;
        }
        let mut record = cursor_record(tx, id, &url)?.unwrap_or_default();
        if matches!(start, Start::Unread) && record.has_read() {
            continue;
        }
        record.start_from(snapshot, &url, read_at);
        queued |= !record.pending.is_empty();
        save_cursor_record(tx, id, &url, &record)?;
    }
    Ok(queued)
}

#[cfg(test)]
mod tests;
