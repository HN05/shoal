//! Starting a space's watches of its items from a read made when it links
//! them, so they report only what changes after that: what the items held
//! before is their history, which `shoal view` shows.
use anyhow::Result;
use shoal_ids::space_ids::SpaceId;

use super::{
    activity::follows,
    cursor::{cursor_record, save_cursor_record},
};
use crate::{
    daemon::{links::space::require_ready_space, log, workspace::Manager},
    forge::items::reference::ItemRef,
    time::UnixTime,
};
use shoal_forge::updates::Snapshot;

impl Manager {
    /// Start the watches of `items`, which the space `id` just linked, from
    /// a read of each, unless one already read its item, for a caller
    /// holding the space's PR gate. An item the forge does not read in full
    /// is left to the next poll.
    pub(crate) async fn start_linked_watches_gated(&self, id: &SpaceId, items: Vec<ItemRef>) {
        if let Err(error) = self.start_watches(id, items).await {
            log!("watches of {id}'s linked items not started: {error:#}");
        }
    }

    /// Start the watches of `items` on the cursor of the space `id` that
    /// never read their item from a read of each made now.
    async fn start_watches(&self, id: &SpaceId, items: Vec<ItemRef>) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let read_at = UnixTime::now();
        let reads = self.complete_reads(items).await;
        let id = id.to_owned();
        let started = self
            .store
            .write(move |tx| start_followed(tx, &id, reads, read_at))
            .await?;
        if started {
            self.signals.item_updates_queued();
        }
        Ok(())
    }
}

/// Start the watches of the space `id` that never read their item from
/// `reads`, made by lookups that began at `read_at`, of the items it still
/// follows. Returns whether one queued an update.
fn start_followed(
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    reads: Vec<(String, Snapshot)>,
    read_at: UnixTime,
) -> Result<bool> {
    require_ready_space(tx, id)?;
    let mut queued = false;
    for (url, snapshot) in reads {
        if !follows(tx, id, &url)? {
            continue;
        }
        let mut record = cursor_record(tx, id, &url)?.unwrap_or_default();
        if record.has_read() {
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
