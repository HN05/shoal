//! What a space's watch cursors record besides what they saw: changes they
//! need not report and updates they hold.
use anyhow::Result;
use shoal_forge::updates::Edited;
use shoal_forge_types::{items::item::ItemState, updates::Update};
use shoal_ids::space_ids::SpaceId;

use super::cursor::{
    all_pending, cursor_record, cursor_spaces, pending_updates, save_cursor_record, save_pending,
};

/// A change the workspace's agent made to `url` through Shoal, which its
/// watches do not report back: the comments, replies or reviews it posted,
/// the state it set and the details it edited.
#[derive(Clone)]
pub struct OwnChange<'a> {
    pub url: &'a str,
    pub authored: Vec<String>,
    pub state: Option<ItemState>,
    pub edited: Edited,
}

/// Record `change` on the workspace's cursor. The caller holds the workspace's
/// PR gate from before the change, so no poll that observed it can record it
/// first.
pub fn record_own_change(
    db: &rusqlite::Connection,
    workspace_id: &SpaceId,
    change: OwnChange<'_>,
) -> Result<()> {
    let mut record = cursor_record(db, workspace_id, change.url)?.unwrap_or_default();
    record.adopt_own_change(change.authored, change.state, &change.edited);
    save_cursor_record(db, workspace_id, change.url, &record)
}

/// Record `edited`, a change Shoal made to the item at `url` on its own,
/// such as an assignee it added, on every space's cursor of the item, so
/// no watch reports it. The caller holds the gate a poll checks for such a
/// change from before it.
pub fn record_shoal_change(db: &rusqlite::Connection, url: &str, edited: &Edited) -> Result<()> {
    if edited.is_empty() {
        return Ok(());
    }
    for id in cursor_spaces(db, url)? {
        let change = OwnChange {
            url,
            authored: Vec::new(),
            state: None,
            edited: edited.clone(),
        };
        record_own_change(db, &id, change)?;
    }
    Ok(())
}

/// Queue an update for the workspace's next watch of `update.url`, ahead of
/// newly observed activity.
pub fn queue_update(
    db: &rusqlite::Connection,
    workspace_id: &SpaceId,
    update: Update,
) -> Result<()> {
    let mut pending = pending_updates(db, workspace_id, &update.url)?;
    let url = update.url.clone();
    pending.push(update);
    save_pending(db, workspace_id, &url, &pending)
}

/// Remove `deliveries` from the pending updates of the workspaces `ids`;
/// updates queued after them stay pending.
pub fn acknowledge_deliveries(
    db: &rusqlite::Connection,
    ids: &[SpaceId],
    deliveries: &[String],
) -> Result<()> {
    for (id, url, mut pending) in all_pending(db, ids)? {
        let count = pending.len();
        pending.retain(|update| !deliveries.contains(&update.delivery));
        if pending.len() != count {
            save_pending(db, &id, &url, &pending)?;
        }
    }
    Ok(())
}

/// The updates pending on the cursor of the space `id` for the items at
/// `urls`.
pub fn pending_on_cursor(
    db: &rusqlite::Connection,
    id: &SpaceId,
    urls: &[String],
) -> Result<Vec<Update>> {
    Ok(all_pending(db, std::slice::from_ref(id))?
        .into_iter()
        .filter(|(_, url, _)| urls.contains(url))
        .flat_map(|(_, _, pending)| pending)
        .collect())
}
