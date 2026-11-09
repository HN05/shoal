//! What a watch last saw of an item, and the updates delivered from it that
//! are not acknowledged yet, which the caller stores apart so reading them
//! reads none of the item's history.
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use shoal_time::UnixTime;

use super::{Edited, Snapshot, Update, UpdateKind, held::HeldPasses};
use crate::items::item::ItemState;

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
    #[serde(
        default,
        alias = "own_state_at",
        skip_serializing_if = "Option::is_none"
    )]
    own_change_at: Option<UnixTime>,
    #[serde(default, skip_serializing_if = "HeldPasses::is_empty")]
    held: HeldPasses,
}

impl WatchRecord {
    /// The updates a watch reports for `snapshot` of the item at `url`, seen
    /// by a poll that started at `observed_at`. Updates still pending are
    /// reported again instead, and the cursor stays until they are
    /// acknowledged; new updates become one delivery that stays pending.
    pub fn observe(&mut self, snapshot: Snapshot, url: &str, observed_at: UnixTime) -> Vec<Update> {
        self.observe_reporting(snapshot, url, observed_at, |_| true)
    }

    /// [`Self::observe`] for an item followed only to learn how it ends,
    /// which reports only that it merged or closed.
    pub fn observe_conclusion(
        &mut self,
        snapshot: Snapshot,
        url: &str,
        observed_at: UnixTime,
    ) -> Vec<Update> {
        self.observe_reporting(snapshot, url, observed_at, |update| {
            matches!(update.kind, UpdateKind::Merged | UpdateKind::Closed)
        })
    }

    /// [`Self::observe`], delivering only the new updates `reported` keeps;
    /// the snapshot takes in the rest unreported.
    fn observe_reporting(
        &mut self,
        mut snapshot: Snapshot,
        url: &str,
        observed_at: UnixTime,
        reported: impl Fn(&Update) -> bool,
    ) -> Vec<Update> {
        if !self.pending.is_empty() {
            return self.pending.clone();
        }
        snapshot.normalize(&self.snapshot, UnixTime::now());
        self.discount_own_changes(&mut snapshot, observed_at);
        let changes = self.snapshot.changes(&snapshot, url);
        let mut delivered = self.held.deliver(&snapshot, changes);
        delivered.retain(reported);
        self.pending = one_delivery(delivered);
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
