//! Secrets, ports, simulators, resource permits and containers, as
//! `docs/design/leases.md` describes them.
use std::sync::Arc;

use anyhow::Result;
use shoal_core::{config, env, fsutil, hooks, repository, tools};
use shoal_daemon_base::{callers, gates, log, notifications};
use shoal_daemon_base::{config_state::ConfigState, notifications::Notifications};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::alerts::AlertKind;
use shoal_protocol::approvals::{ApprovalRequest, Target};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};
use shoal_protocol::{model, protocol, state, time, validate};
use shoal_store as store;
use shoal_subprocess as subprocess;
use tokio::sync::{Notify, futures::Notified};

mod acquired_leases;
pub mod allocation;
mod asleep_leases;
pub mod containers;
pub mod daemon;
pub mod ports;
mod queue;
pub mod resources;
mod secrets;
pub mod sim;
#[cfg(test)]
mod stand_in;

use daemon::{Daemon, Records};
use gates::Gates;
use model::Workspace;
use store::Store;

pub struct Leases {
    store: Store,
    gates: Arc<Gates>,
    config: Arc<ConfigState>,
    notifications: Arc<Notifications>,
    records: Arc<dyn Records>,
    /// Arrival order of queued resource and simulator requests.
    queues: queue::Queues,
    /// Woken when a simulator goes idle, which its expiry checks.
    simulator_idle: Notify,
    /// Woken when a workspace's containers start, whose usage is sampled.
    containers_started: Notify,
}

impl Leases {
    pub fn new(
        store: Store,
        gates: Arc<Gates>,
        config: Arc<ConfigState>,
        notifications: Arc<Notifications>,
        records: Arc<dyn Records>,
    ) -> Self {
        Self {
            store,
            gates,
            config,
            notifications,
            records,
            queues: queue::Queues::default(),
            simulator_idle: Notify::new(),
            containers_started: Notify::new(),
        }
    }

    /// Each workspace still waiting for a lease, with the pool or the
    /// simulators it waits for.
    pub fn waiters(&self) -> Vec<(Target, SpaceId)> {
        self.queues.waiters()
    }

    /// Wait until a simulator goes idle, which may let an idle one expire.
    pub fn simulator_idled(&self) -> Notified<'_> {
        self.simulator_idle.notified()
    }

    /// Wait until a workspace's containers start, whose usage to sample.
    pub fn containers_started(&self) -> Notified<'_> {
        self.containers_started.notified()
    }

    /// The settings of `workspace` over the current global config.
    async fn settings(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
    ) -> Result<config::Effective> {
        daemon.settings(&self.config.current(), workspace).await
    }

    /// Record `request` unless a grant covers it, in a transaction of its
    /// own, returning the request still waiting for or refused a decision.
    async fn request_approval(&self, request: ApprovalRequest) -> Result<Option<ApprovalRequest>> {
        let records = self.records.clone();
        (self.store)
            .write(move |tx| records.check_approval(tx, request))
            .await
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
        kind: AlertKind,
        message: String,
    ) {
        let about = notifications::About::space(name, id);
        let alert = shoal_daemon_base::alerts::NewAlert::shoal(kind, about, message);
        self.notifications.notify(daemon, alert).await;
    }
}

/// Fail unless `workspace`, as read before a lease changes, is ready.
fn ensure_ready(workspace: &Workspace) -> Result<()> {
    match workspace.state {
        state::WorkspaceState::Ready => Ok(()),
        state => Err(store::not_ready(
            workspace.number,
            &workspace.id,
            &workspace.name,
            state,
        )),
    }
}
