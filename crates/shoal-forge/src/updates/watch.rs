//! What a watch last saw of an item, and the updates delivered from it that
//! are not acknowledged yet, which the caller stores apart so reading them
//! reads none of the item's history.
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use shoal_time::UnixTime;

use super::{Edited, Snapshot, held::HeldPasses};
use shoal_forge_types::items::item::ItemState;
use shoal_forge_types::updates::{Update, UpdateKind};

#[derive(Default, Serialize, Deserialize)]
pub struct WatchRecord {
    #[serde(flatten)]
    pub snapshot: Snapshot,
    #[serde(skip)]
    pub pending: Vec<Update>,
    /// Comments and reviews the workspace changed through Shoal that no
    /// poll has seen change yet.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    authored: BTreeSet<String>,
    /// When the workspace last set the item's state or edited
    /// its details through Shoal; observations that started by then cannot
    /// show it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    own_change_at: Option<UnixTime>,
    #[serde(default, skip_serializing_if = "HeldPasses::is_empty")]
    held: HeldPasses,
}

impl WatchRecord {
    /// Start the watch from `snapshot` of the item at `url`, read by a
    /// lookup that began at `read_at`, as when its space links the item or
    /// an agent begins a new session there. What the item's discussion and
    /// details show then is history, which `shoal view` shows; changes to
    /// its condition since the watch last read it, such as a failed check,
    /// a conflict or its closing, are reported, and of the updates not yet
    /// acknowledged only those of its condition stay.
    pub fn start_from(&mut self, mut snapshot: Snapshot, url: &str, _read_at: UnixTime) {
        snapshot.normalize(&self.snapshot, UnixTime::now());
        // The read shows the space's own comments it has, which no later
        // change may be taken for.
        self.snapshot.adopt_authored(&snapshot, &mut self.authored);
        self.pending.retain(|update| of_condition(update.kind));
        let mut condition = self.snapshot.changes(&snapshot, url);
        condition.retain(|update| of_condition(update.kind));
        (self.pending).extend(one_delivery(self.held.deliver(&snapshot, condition)));
        self.snapshot = snapshot;
    }

    /// Whether the watch has read its item.
    pub fn has_read(&self) -> bool {
        !self.snapshot.before_first_read()
    }

    /// The updates a watch reports for `snapshot` of the item at `url`, seen
    /// by a poll that started at `observed_at`. Updates still pending are
    /// reported again instead, and the cursor stays until they are
    /// acknowledged; new updates become one delivery that stays pending.
    pub fn observe(
        &mut self,
        mut snapshot: Snapshot,
        url: &str,
        observed_at: UnixTime,
    ) -> Vec<Update> {
        if !self.pending.is_empty() {
            return self.pending.clone();
        }
        snapshot.normalize(&self.snapshot, UnixTime::now());
        self.discount_own_changes(&mut snapshot, observed_at);
        let changes = self.snapshot.changes(&snapshot, url);
        self.pending = one_delivery(self.held.deliver(&snapshot, changes));
        self.snapshot = snapshot;
        self.pending.clone()
    }

    /// Take the changes the workspace made through Shoal that `snapshot`
    /// shows as already seen, so its watch does not report them back.
    fn discount_own_changes(&mut self, snapshot: &mut Snapshot, observed_at: UnixTime) {
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

    /// Note that something changes the item for the workspace through
    /// Shoal while a poll that started at `observed_at` reads it, so the
    /// poll reports none of the item's details or state as changed: what
    /// it read may show the change before its record does.
    pub fn own_change_running(&mut self, observed_at: UnixTime) {
        self.own_change_at = self.own_change_at.max(Some(observed_at));
    }

    /// Note the comments and reviews in `authored`, the `state` and the
    /// details `edited` that the workspace changed through Shoal, so no
    /// watch reports them back.
    pub fn adopt_own_change(
        &mut self,
        authored: Vec<String>,
        state: Option<ItemState>,
        edited: &Edited,
    ) {
        self.authored.extend(authored);
        if let Some(state) = state {
            self.snapshot.adopt_state(Some(state.into()));
            self.own_change_at = Some(UnixTime::now());
        }
        if !edited.is_empty() {
            self.snapshot.adopt_edited(edited);
            self.own_change_at = Some(UnixTime::now());
        }
    }
}

/// Whether an update of `kind` reports the item's condition rather than
/// what was said on it or changed in its details.
fn of_condition(kind: UpdateKind) -> bool {
    !matches!(kind, UpdateKind::Comment | UpdateKind::Edited)
}

/// `updates` as one delivery, which an agent acknowledges together.
fn one_delivery(mut updates: Vec<Update>) -> Vec<Update> {
    let delivery = uuid::Uuid::new_v4().to_string();
    for update in &mut updates {
        update.delivery.clone_from(&delivery);
    }
    updates
}

#[cfg(test)]
mod tests;
