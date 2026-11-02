//! An item's record on a space's watch cursor: what its watch last saw of the
//! item, and the updates delivered from it that are not acknowledged yet.
use std::collections::BTreeSet;

use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::held::HeldPasses;
use crate::forge::updates::{Snapshot, Update};

#[derive(Default, Serialize, Deserialize)]
pub(super) struct ActivityRecord {
    #[serde(flatten)]
    pub(super) snapshot: Snapshot,
    #[serde(default)]
    pub(super) pending: Vec<Update>,
    /// Comments and reviews the workspace changed through Shoal that no
    /// poll has seen change yet.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    authored: BTreeSet<String>,
    /// Unix seconds when the workspace last set the item's state through
    /// Shoal; observations that started by then cannot show it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    own_state_at: Option<u64>,
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
        // A poll that started by the agent's own state change may have read
        // the state before it.
        if self.own_state_at.is_some_and(|at| observed_at <= at) {
            snapshot.adopt_state(self.snapshot.state());
        } else {
            self.own_state_at = None;
        }
    }

    /// Note the comments and reviews in `authored` and the `state` the
    /// workspace set through Shoal, so no watch reports them back.
    pub(super) fn adopt_own_change(&mut self, authored: Vec<String>, state: Option<&str>) {
        self.authored.extend(authored);
        if let Some(state) = state {
            self.snapshot.adopt_state(state);
            self.own_state_at = Some(crate::time::unix_seconds());
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

/// The item's record on the workspace's watch cursor, if it has one.
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
    Ok(record.map(|text| serde_json::from_str(&text)).transpose()?)
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
    Ok(())
}
