//! The leases the daemon grants a workspace: TCP port reservations, and the
//! outcome every allocation shares. [`Leases`] offers their operations;
//! what they need of the rest of the daemon is the interface in [`daemon`].
use std::sync::Arc;

use anyhow::Result;
use shoal_core::{config, env};
use shoal_daemon_base::{callers, notifications};
use shoal_daemon_base::{config_state::ConfigState, notifications::Notifications};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::notifications::NotificationKind;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};
use shoal_protocol::{model, protocol, validate};
use shoal_store as store;

pub mod allocation;
pub mod daemon;
pub mod ports;

use daemon::{Daemon, Grants};
use model::Workspace;
use store::Store;

/// The leases of every workspace, with the daemon's services they use.
pub struct Leases {
    store: Store,
    config: Arc<ConfigState>,
    notifications: Arc<Notifications>,
    grants: Arc<dyn Grants>,
}

impl Leases {
    pub fn new(
        store: Store,
        config: Arc<ConfigState>,
        notifications: Arc<Notifications>,
        grants: Arc<dyn Grants>,
    ) -> Self {
        Self {
            store,
            config,
            notifications,
            grants,
        }
    }

    /// The settings of `workspace` over the current global config.
    async fn settings(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
    ) -> Result<config::Effective> {
        daemon.settings(&self.config.current(), workspace).await
    }

    /// The workspace an optional filter names, or none for every workspace.
    async fn workspace_filter(
        &self,
        daemon: &dyn Daemon,
        selector: Option<&SpaceSelector>,
    ) -> Result<Option<SpaceId>> {
        match selector {
            Some(selector) => Ok(Some(daemon.workspace(selector).await?.id)),
            None => Ok(None),
        }
    }

    /// Tell the user, or the coordinator of the space `id` named `name`,
    /// about an event of a lease. A failure is logged.
    async fn notify(
        &self,
        daemon: &dyn Daemon,
        (name, id): (&SpaceName, &SpaceId),
        kind: NotificationKind,
        message: String,
    ) {
        let about = notifications::About::space(name, id);
        (self.notifications)
            .notify(daemon, about, kind, message, &[])
            .await;
    }
}
