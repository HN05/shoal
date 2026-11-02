//! An item's record on a space's watch cursor: what its watch last saw of the
//! item, and the updates delivered from it that are not acknowledged yet,
//! stored apart so reading them reads none of the item's history.
use std::collections::BTreeSet;

use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::held::HeldPasses;
use crate::forge::updates::{Edited, Snapshot, Update};

#[derive(Default, Serialize, Deserialize)]
pub(super) struct ActivityRecord {
    #[serde(flatten)]
    pub(super) snapshot: Snapshot,
    #[serde(skip)]
    pub(super) pending: Vec<Update>,
    /// Comments and reviews the workspace changed through Shoal that no
    /// poll has seen change yet.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    authored: BTreeSet<String>,
    /// Unix seconds when the workspace last set the item's state or edited
    /// its details through Shoal; observations that started by then cannot
    /// show it.
    #[serde(
        default,
        alias = "own_state_at",
        skip_serializing_if = "Option::is_none"
    )]
    own_change_at: Option<u64>,
    #[serde(default, skip_serializing_if = "HeldPasses::is_empty")]
    held: HeldPasses,
}

impl ActivityRecord {
    /// The updates a watch reports for `snapshot` of the item at `url`, seen
    /// by a poll that started at `observed_at`. Updates still pending are
    /// reported again instead, and the cursor stays until they are
    /// acknowledged; new updates become one delivery that stays pending.
    pub(super) fn observe(
        &mut self,
        mut snapshot: Snapshot,
        url: &str,
        observed_at: u64,
    ) -> Vec<Update> {
        if !self.pending.is_empty() {
            return self.pending.clone();
        }
        snapshot.normalize(&self.snapshot, crate::time::unix_seconds());
        self.discount_own_changes(&mut snapshot, observed_at);
        let changes = self.snapshot.changes(&snapshot, url);
        self.pending = one_delivery(self.held.deliver(&snapshot, changes));
        self.snapshot = snapshot;
        self.pending.clone()
    }

    /// Take the changes the workspace made through Shoal that `snapshot`
    /// shows as already seen, so its watch does not report them back.
    fn discount_own_changes(&mut self, snapshot: &mut Snapshot, observed_at: u64) {
        self.snapshot.adopt_authored(snapshot, &mut self.authored);
        // A poll that started by the agent's own change may have read the
        // item before it.
        if self.own_change_at.is_some_and(|at| observed_at <= at) {
            snapshot.adopt_state(self.snapshot.state());
            snapshot.adopt_details(&self.snapshot);
        } else {
            self.own_change_at = None;
        }
    }

    /// Note the comments and reviews in `authored`, the `state` and the
    /// details `edited` that the workspace changed through Shoal, so no
    /// watch reports them back.
    pub(super) fn adopt_own_change(
        &mut self,
        authored: Vec<String>,
        state: Option<&str>,
        edited: &Edited,
    ) {
        self.authored.extend(authored);
        if let Some(state) = state {
            self.snapshot.adopt_state(state);
            self.own_change_at = Some(crate::time::unix_seconds());
        }
        if !edited.is_empty() {
            self.snapshot.adopt_edited(edited);
            self.own_change_at = Some(crate::time::unix_seconds());
        }
    }
}

/// `updates` as one delivery, which an agent acknowledges together.
fn one_delivery(mut updates: Vec<Update>) -> Vec<Update> {
    let delivery = uuid::Uuid::new_v4().to_string();
    for update in &mut updates {
        update.delivery.clone_from(&delivery);
    }
    updates
}

/// The item's record on the workspace's watch cursor, if it has one: what
/// its watch saw of the item, or updates queued from it before any watch did.
pub(super) fn cursor_record(
    db: &rusqlite::Connection,
    id: &str,
    url: &str,
) -> Result<Option<ActivityRecord>> {
    let record: Option<String> = db
        .query_row(
            "SELECT record FROM pr_activity WHERE space_id=?1 AND url=?2",
            params![id, url],
            |row| row.get(0),
        )
        .optional()?;
    let pending = pending_updates(db, id, url)?;
    if record.is_none() && pending.is_empty() {
        return Ok(None);
    }
    let mut record: ActivityRecord = record
        .map(|text| serde_json::from_str(&text))
        .transpose()?
        .unwrap_or_default();
    record.pending = pending;
    Ok(Some(record))
}

/// Store `record` as the item's record on the workspace's watch cursor.
pub(super) fn save_cursor_record(
    db: &rusqlite::Connection,
    id: &str,
    url: &str,
    record: &ActivityRecord,
) -> Result<()> {
    db.execute(
        "INSERT INTO pr_activity(space_id,url,record) VALUES (?1,?2,?3)
         ON CONFLICT(space_id,url) DO UPDATE SET record=excluded.record",
        params![id, url, serde_json::to_string(record)?],
    )?;
    save_pending(db, id, url, &record.pending)
}

/// The updates delivered from the item on the workspace's watch cursor that
/// are not acknowledged yet.
pub(super) fn pending_updates(
    db: &rusqlite::Connection,
    id: &str,
    url: &str,
) -> Result<Vec<Update>> {
    let updates: Option<String> = db
        .query_row(
            "SELECT updates FROM pending_updates WHERE space_id=?1 AND url=?2",
            params![id, url],
            |row| row.get(0),
        )
        .optional()?;
    Ok(updates
        .map(|text| serde_json::from_str(&text))
        .transpose()?
        .unwrap_or_default())
}

/// Store `updates` as the pending updates of the item on the workspace's
/// watch cursor.
pub(super) fn save_pending(
    db: &rusqlite::Connection,
    id: &str,
    url: &str,
    updates: &[Update],
) -> Result<()> {
    if updates.is_empty() {
        db.execute(
            "DELETE FROM pending_updates WHERE space_id=?1 AND url=?2",
            params![id, url],
        )?;
    } else {
        db.execute(
            "INSERT INTO pending_updates(space_id,url,updates) VALUES (?1,?2,?3)
             ON CONFLICT(space_id,url) DO UPDATE SET updates=excluded.updates",
            params![id, url, serde_json::to_string(updates)?],
        )?;
    }
    Ok(())
}

/// The pending updates of every item on the cursors of the spaces `ids`,
/// with their space and URL.
pub(super) fn all_pending(
    db: &rusqlite::Connection,
    ids: &[String],
) -> Result<Vec<(String, String, Vec<Update>)>> {
    let mut statement = db.prepare("SELECT url, updates FROM pending_updates WHERE space_id=?1")?;
    let mut pending = Vec::new();
    for id in ids {
        let rows = statement
            .query_map([id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (url, updates) in rows {
            pending.push((id.clone(), url, serde_json::from_str(&updates)?));
        }
    }
    Ok(pending)
}
