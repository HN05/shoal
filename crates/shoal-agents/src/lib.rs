//! The daemon's agents: the executions their wrappers register and relay,
//! the agent queue that admits, starts and restores them, their stops,
//! restarts and interrupts, the protection that stops them for overload,
//! disk space and shutdowns, recovery after a daemon restart, their
//! sessions, places and stopped work, the turn state their hooks report,
//! the subagents they run, retries of the turns API errors end, the alerts
//! queued for them, the user's engagement with them, idle wakes, sleep and
//! wake timers, the trust their tools give repositories' paths, and keeping
//! the machine awake while they have work. [`Agents`] holds what they keep
//! in memory and offers their operations; what they need of the rest of the
//! daemon is the interface in [`daemon`].
use std::{collections::HashMap, sync::Arc};

use shoal_ids::execution_ids::ExecutionId;

use shoal_core::{
    agents, ai, caches, config, env, execution, fsutil, git, hooks, installed, paths, process,
    refusal, repository, shoal_dirs, start_plan, terminal, tmp_dirs, workers,
};
use shoal_daemon_base::{
    config_state::ConfigState, file_access::FileAccess, gates::Gates, log,
    notifications::Notifications, signals::Signals,
};
use shoal_forge_types as forge;
use shoal_protocol::{agent, model, paths::Paths, protocol, state, time, validate};
use shoal_store::Store;

pub mod busy_cpu;
pub mod capacity;
pub mod control;
pub mod daemon;
pub mod engagement;
pub mod execution_connection;
pub mod execution_recovery;
pub mod executions;
pub mod idle_wakes;
pub mod keep_awake;
pub mod lost_reports;
pub mod messages;
#[cfg(target_os = "linux")]
pub mod oom_scores;
pub mod overload;
pub mod overload_recovery;
pub mod places;
pub mod program_status;
pub mod proxy_retries;
pub mod queue;
pub mod queued_starts;
pub mod restart_recovery;
pub mod restarts;
pub mod sessions;
pub mod shutdown_stop;
#[cfg(test)]
mod stand_in;
pub mod stopped_work;
pub mod subagents;
pub mod trust;
pub mod turn_retry;
pub mod turn_state;
pub mod turns;
pub mod upstream_limits;
pub mod wake_timers;
pub mod warnings;

/// The agents' service, which `Manager` holds.
pub struct Agents {
    store: Store,
    gates: Arc<Gates>,
    config: Arc<ConfigState>,
    signals: Arc<Signals>,
    notifications: Arc<Notifications>,
    file_access: Arc<FileAccess>,
    paths: Paths,
    records: Arc<dyn daemon::Records>,
    /// The removals under way, which the records of new work read.
    removals: Arc<dyn daemon::Removals>,
    /// When each space's agent state was last reported.
    report_times: lost_reports::ReportTimes,
    /// The time runs of agent messages settle by.
    settle_clock: messages::SettleClock,
    /// The executions the daemon tracks and the agents they run.
    executions: executions::Executions,
    /// The restarts of agents the daemon took, and the line of sequential
    /// ones.
    restarts: restarts::Restarts,
    /// The hosts whose sessions the places of agents run in.
    session_hosts: Vec<Arc<dyn shoal_core::sessions::Host>>,
    /// Held while the owner of a recorded place is read and its place
    /// closed, or its ownership moves to another workspace.
    place_ownership: tokio::sync::Mutex<()>,
    /// Held through a stop before a shutdown; true once one finished, until
    /// a system shutdown is cancelled, so later ones leave the work alone.
    shutdown_stopped: tokio::sync::Mutex<bool>,
    /// The busy CPU the overload monitor reads for job admission.
    busy_cpu: busy_cpu::BusyCpu,
    /// What the agent queue keeps in memory.
    agent_queue: queue::AgentQueue,
    /// When agents protection stopped may start again.
    overload_recovery: overload_recovery::OverloadRecovery,
    /// Held while an agent is admitted to a workspace, until it is tracked,
    /// and while a workspace's conversion into a chat is recorded, so no
    /// agent starts in a workspace that becomes a chat.
    agent_admissions: tokio::sync::Mutex<()>,
    /// The retry each proxied execution's proxy waits to make.
    proxy_retries: std::sync::Mutex<HashMap<ExecutionId, proxy_retries::ProxyRetry>>,
    /// The limit each proxied execution's proxy found its upstream under,
    /// with the time it stands until.
    upstream_limits: std::sync::Mutex<HashMap<ExecutionId, upstream_limits::UpstreamLimit>>,
}

impl Agents {
    #[allow(
        clippy::too_many_arguments,
        reason = "what the agents share with the daemon's other subsystems"
    )]
    pub fn new(
        store: Store,
        gates: Arc<Gates>,
        config: Arc<ConfigState>,
        signals: Arc<Signals>,
        notifications: Arc<Notifications>,
        file_access: Arc<FileAccess>,
        paths: Paths,
        records: Arc<dyn daemon::Records>,
        removals: Arc<dyn daemon::Removals>,
        session_hosts: Vec<Arc<dyn shoal_core::sessions::Host>>,
    ) -> Self {
        Self {
            store,
            gates,
            config,
            signals,
            notifications,
            file_access,
            paths,
            records,
            removals,
            report_times: Default::default(),
            settle_clock: Default::default(),
            executions: Default::default(),
            restarts: Default::default(),
            session_hosts,
            place_ownership: Default::default(),
            shutdown_stopped: Default::default(),
            busy_cpu: Default::default(),
            agent_queue: Default::default(),
            overload_recovery: Default::default(),
            agent_admissions: Default::default(),
            proxy_retries: Default::default(),
            upstream_limits: Default::default(),
        }
    }

    /// Forget the order and times of the state reports of the space `id`,
    /// which is gone.
    pub fn forget_reports(&self, id: &shoal_ids::space_ids::SpaceId) {
        self.report_times.forget(id);
    }

    /// The executions recorded in the space `id`.
    pub async fn space_executions(
        &self,
        id: &shoal_ids::space_ids::SpaceId,
    ) -> anyhow::Result<Vec<model::Execution>> {
        let id = id.to_owned();
        self.store
            .run(move |db| shoal_store::executions(db, &id))
            .await
    }

    /// The executions the daemon tracks and the agents they run.
    pub fn executions(&self) -> &executions::Executions {
        &self.executions
    }

    /// The proxy retries each execution's agent reported.
    #[cfg(any(test, feature = "test-support"))]
    pub fn proxy_retries(
        &self,
    ) -> &std::sync::Mutex<HashMap<ExecutionId, proxy_retries::ProxyRetry>> {
        &self.proxy_retries
    }

    /// The upstream limits each execution's agent reported.
    #[cfg(any(test, feature = "test-support"))]
    pub fn upstream_limits(
        &self,
    ) -> &std::sync::Mutex<HashMap<ExecutionId, upstream_limits::UpstreamLimit>> {
        &self.upstream_limits
    }

    /// The restarts under way.
    #[cfg(any(test, feature = "test-support"))]
    pub fn restarts(&self) -> &restarts::Restarts {
        &self.restarts
    }

    /// Replace the hosts whose sessions agents run in, as a test injects.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_session_hosts(&mut self, hosts: Vec<Arc<dyn shoal_core::sessions::Host>>) {
        self.session_hosts = hosts;
    }

    /// The queue of agents waiting for admission.
    pub fn agent_queue(&self) -> &queue::AgentQueue {
        &self.agent_queue
    }

    /// Held while the daemon stops every agent for a shutdown or its exit.
    pub fn shutdown_stopped(&self) -> &tokio::sync::Mutex<bool> {
        &self.shutdown_stopped
    }

    /// The busy CPU the overload monitor reads for job admission.
    pub fn busy_cpu(&self) -> &busy_cpu::BusyCpu {
        &self.busy_cpu
    }

    /// When agents protection stopped may start again.
    pub fn overload_recovery(&self) -> &overload_recovery::OverloadRecovery {
        &self.overload_recovery
    }

    /// Admit an agent to the workspace `selector` names, unless it
    /// converts. The guard, held until the agent is tracked, keeps the
    /// workspace from becoming a chat meanwhile.
    pub async fn admit_agent(
        &self,
        daemon: &dyn daemon::Daemon,
        selector: &shoal_protocol::space_names::SpaceSelector,
    ) -> anyhow::Result<tokio::sync::MutexGuard<'_, ()>> {
        let admitted = self.agent_admissions.lock().await;
        daemon.ensure_takes_agents(selector).await?;
        Ok(admitted)
    }

    /// Held while an agent is admitted to a workspace, until it is tracked,
    /// and while a workspace's conversion into a chat is recorded.
    pub fn agent_admissions(&self) -> &tokio::sync::Mutex<()> {
        &self.agent_admissions
    }

    /// The clock runs of agent messages settle by.
    pub fn settle_clock(&self) -> &messages::SettleClock {
        &self.settle_clock
    }
}

#[cfg(test)]
shoal_core::prepare_test_process!();
