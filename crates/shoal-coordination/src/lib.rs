//! Coordination between spaces and the user: the alerts one sends another,
//! the threads they hold conversations in and the changes an agent makes
//! for a thread, the approval requests a scoped caller makes and where they
//! go to be decided, the permissions each space holds, and the scope a
//! caller's token reaches. [`Coordination`] holds what they keep in memory
//! and offers their operations; what they need of the rest of the daemon is
//! the interface in [`daemon`].
use shoal_daemon_base::operations::Operations;
use std::sync::Arc;

use anyhow::Result;
use shoal_core::{config, git, refusal, repository};
use shoal_daemon_base::{
    config_state::ConfigState, events, gates::Gates, item_numbers, log, notifications,
    notifications::Notifications, signals::Signals,
};
use shoal_forge_types as forge;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{model, protocol, state, time, validate};
use shoal_store::{self as store, Store, tree};

use crate::model::SpaceRef;

pub mod alerts;
pub mod approvals;
pub mod changes;
pub mod daemon;
pub mod permissions;
pub mod scope;
pub mod threads;

/// The coordination service, which `Manager` holds.
pub struct Coordination {
    store: Store,
    gates: Arc<Gates>,
    config: Arc<ConfigState>,
    notifications: Arc<Notifications>,
    signals: Arc<Signals>,
    operations: Arc<Operations>,
    /// The scope tokens approved commands take their permission's actions
    /// with.
    grants: permissions::Grants,
    records: Arc<dyn daemon::Records>,
}

impl Coordination {
    pub fn new(
        store: Store,
        gates: Arc<Gates>,
        config: Arc<ConfigState>,
        notifications: Arc<Notifications>,
        signals: Arc<Signals>,
        operations: Arc<Operations>,
        records: Arc<dyn daemon::Records>,
    ) -> Self {
        Self {
            store,
            gates,
            config,
            notifications,
            signals,
            operations,
            grants: permissions::Grants::default(),
            records,
        }
    }

    /// `id` and every space below it, `id` first.
    async fn subtree(&self, id: &SpaceId) -> Result<Vec<SpaceId>> {
        let id = id.to_owned();
        self.store.read(move |db| tree::subtree(db, &id)).await
    }

    /// Whether `id` is `ancestor` or a space below it.
    async fn within_subtree(&self, ancestor: &SpaceId, id: &SpaceId) -> Result<bool> {
        Ok(self.subtree(ancestor).await?.contains(id))
    }

    /// The spaces above `id`, nearest first.
    async fn ancestors(&self, id: &SpaceId) -> Result<Vec<SpaceRef>> {
        let id = id.to_owned();
        self.store.read(move |db| tree::ancestors(db, &id)).await
    }
}

#[cfg(test)]
mod stand_in;
