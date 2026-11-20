//! `Manager`, the daemon's shared state, with its configuration.
use crate::config_file::ConfigExt;
use crate::paths::StateFiles;
use shoal_ids::execution_ids::ExecutionId;

use crate::{config::Config, daemon::store::Store, model::Repository, paths::Paths};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::OwnedMutexGuard;

pub struct Manager {
    pub store: Store,
    pub(crate) session_hosts: Vec<Arc<dyn crate::sessions::Host>>,
    /// Held while the owner of a recorded place is read and its place
    /// closed, or its ownership moves to another workspace.
    pub(crate) place_ownership: tokio::sync::Mutex<()>,
    pub gates: Arc<super::gates::Gates>,
    pub(crate) waits: crate::daemon::links::wait::ActiveWaits,
    /// The reads of items for watches, and those watches started from.
    pub(crate) watch_starts: crate::daemon::links::wait::WatchStarts,
    pub(super) cleanup_health: super::cleanup::CleanupHealth,
    /// How the loop that puts idle agents to sleep is doing.
    pub(super) idle_sleep_health: super::cleanup::CleanupHealth,
    /// The spaces automatic cleanup removed since Shoal's log recorded it.
    pub(crate) cleanup_removals: super::cleanup::Removed,
    pub(crate) syncs: super::checkout_sync::Syncs,
    /// The triggers.
    pub runner: Arc<shoal_runner::Runner>,
    /// The spaces whose PRs' progress sections wait for an update.
    pub(crate) publications: super::progress::Publications,
    /// The spaces whose issues wait for the agent account to be settled.
    pub(crate) agent_settles: super::links::issue::agent_settles::AgentSettles,
    pub config_state: Arc<super::config_state::ConfigState>,
    pub notifications: Arc<super::notifications::Notifications>,
    /// The leases workspaces hold.
    pub leases: shoal_leases::Leases,
    pub paths: Paths,
    pub executions: super::executions::Executions,
    pub overload_recovery: super::overload_recovery::OverloadRecovery,
    /// The busy CPU the overload monitor reads for job admission.
    pub(crate) busy_cpu: super::busy_cpu::BusyCpu,
    pub agent_queue: super::agent_queue::AgentQueue,
    /// The contributions a running `shoal contribute` keeps from cleanup.
    pub(crate) contribution_holds: super::contributions::holds::ContributionHolds,
    pub(crate) operations: Arc<crate::daemon::operations::Operations>,
    /// The scope tokens approved commands take their permission's actions
    /// with.
    pub(crate) grants: crate::daemon::permissions::Grants,
    /// Held through a stop before a shutdown; true once one finished, until
    /// a system shutdown is cancelled, so later ones leave the work alone.
    pub(crate) shutdown_stopped: tokio::sync::Mutex<bool>,
    pub(crate) signals: super::signals::Signals,
    /// The time runs of agent messages settle by.
    pub(crate) settle_clock: crate::daemon::agent_messages::SettleClock,
    pub(crate) usage: super::workspace::usage::Usage,
    pub(crate) leftovers: crate::daemon::leftovers::Leftovers,
    /// Held while a converted space's agent is handed over to the new place,
    /// so a stop and the relocation never both hand it over.
    pub(crate) conversion_handovers: crate::daemon::conversions::Handovers,
    /// The restarts of agents the daemon took, and the line of sequential
    /// ones.
    pub(crate) restarts: crate::daemon::restarts::Restarts,
    /// Held while an agent is admitted to a workspace, until it is tracked,
    /// and while a workspace's conversion into a chat is recorded, so no
    /// agent starts in a workspace that becomes a chat.
    pub(crate) agent_admissions: tokio::sync::Mutex<()>,
    /// The spaces a removal holds, whose agent no registration runs again.
    pub(crate) space_removals: crate::daemon::spaces::Removing,
    /// The completed spaces whose completion a removal of a space below
    /// them goes by, which keep the workspaces below them meanwhile.
    pub(crate) completed_owners: crate::daemon::spaces::Removing,
    pub(crate) seeds: super::seed::Seeds,
    pub(crate) forge_memories: crate::daemon::links::ForgeMemories,
    /// The local reviews of workspaces.
    pub reviews: Arc<shoal_reviews::Reviews>,
    pub(crate) runners: crate::daemon::runner::Runners,
    /// The retry each proxied execution's proxy waits to make.
    pub(super) proxy_retries:
        std::sync::Mutex<std::collections::HashMap<ExecutionId, super::proxy_retries::ProxyRetry>>,
    /// The limit each proxied execution's proxy found its upstream under,
    /// with the time it stands until.
    pub(super) upstream_limits: std::sync::Mutex<
        std::collections::HashMap<ExecutionId, super::upstream_limits::UpstreamLimit>,
    >,
    /// The config files of worktrees the current format rejects, which the
    /// daemon ignores.
    pub(super) rejected_configs: super::repo_configuration::RejectedConfigs,
    /// When each space's agent state was last reported.
    pub(super) report_times: Arc<super::lost_reports::ReportTimes>,
    /// Directories whose file access stalls, shared with startup's status answer.
    pub(crate) file_access: Arc<crate::daemon::file_access::FileAccess>,
    /// The hub role, or this machine's membership of a hub.
    pub hub: Arc<shoal_hub::Hub>,
    /// The app store's watches.
    pub(crate) apps: crate::daemon::apps::Apps,
    /// The commits other machines carried here for launches on their way.
    pub(crate) carried_holds: crate::daemon::carried_commits::CarriedHolds,
    /// The viewers of simulator demos someone opened.
    pub(crate) viewers: super::demos::viewers::Viewers,
}

impl Manager {
    pub async fn open(paths: Paths) -> Result<Arc<Self>> {
        paths.prepare()?;
        let config = Config::load(&paths)?;
        super::forge_hosts::learn_forge_pins(&Default::default(), &config.machine.forges);
        let store = open_store(&paths).await?;
        let config_state = Arc::new(super::config_state::ConfigState::new(config));
        let notifications = Arc::new(super::notifications::Notifications::new(store.clone()));
        let gates = Arc::new(super::gates::Gates::default());
        let leases = shoal_leases::Leases::new(
            store.clone(),
            gates.clone(),
            config_state.clone(),
            notifications.clone(),
            Arc::new(super::leases::DaemonRecords),
        );
        let hub = Arc::new(shoal_hub::Hub::new(
            store.clone(),
            gates.clone(),
            config_state.clone(),
            notifications.clone(),
            paths.clone(),
            Arc::new(super::hub::HubRecords),
        ));
        let runner = Arc::new(shoal_runner::Runner::new(
            store.clone(),
            config_state.clone(),
            notifications.clone(),
        ));
        let reviews = Arc::new(shoal_reviews::Reviews::new(
            store.clone(),
            gates.clone(),
            Arc::new(super::reviews::ReviewRecords),
            Arc::new(super::gate::GatedRequests),
            paths.clone(),
        ));
        Ok(Arc::new(Self {
            config_state,
            notifications,
            leases,
            waits: Default::default(),
            watch_starts: Default::default(),
            cleanup_health: Default::default(),
            idle_sleep_health: super::cleanup::CleanupHealth::of("Idle sleep"),
            cleanup_removals: Default::default(),
            syncs: Default::default(),
            runner,
            publications: Default::default(),
            agent_settles: Default::default(),
            store,
            // Tests inject hosts and never discover or close the user's
            // sessions.
            session_hosts: if crate::daemon::fake_workers::faked() {
                Vec::new()
            } else {
                crate::sessions::hosts()
            },
            place_ownership: Default::default(),
            gates,
            paths,
            executions: Default::default(),
            overload_recovery: Default::default(),
            busy_cpu: Default::default(),
            agent_queue: Default::default(),
            contribution_holds: Default::default(),
            operations: Default::default(),
            grants: Default::default(),
            shutdown_stopped: Default::default(),
            signals: Default::default(),
            settle_clock: Default::default(),
            usage: Default::default(),
            leftovers: Default::default(),
            conversion_handovers: Default::default(),
            restarts: Default::default(),
            agent_admissions: Default::default(),
            space_removals: Default::default(),
            completed_owners: Default::default(),
            seeds: Default::default(),
            forge_memories: Default::default(),
            reviews,
            runners: Default::default(),
            proxy_retries: Default::default(),
            upstream_limits: Default::default(),
            rejected_configs: Default::default(),
            report_times: Default::default(),
            file_access: Default::default(),
            hub,
            apps: Default::default(),
            carried_holds: Default::default(),
            viewers: Default::default(),
        }))
    }

    /// The global config as last loaded; see [`super::config_state::ConfigState::current`].
    pub fn config(&self) -> Arc<Config> {
        self.config_state.current()
    }

    /// Whether this daemon acts on what the global config sets up for the
    /// machine, such as its triggers, listeners and hub role: a development
    /// build leaves those of the user's own config to the installed Shoal,
    /// whose daemon acts on them.
    pub(crate) fn acts_on_global_config(&self) -> bool {
        let config = crate::config::Config::path(&self.paths);
        crate::config_file::refuse_user_config(&config).is_ok()
    }

    /// Make `config` the one later operations read. Changed overload
    /// settings withdraw recovery readiness before anything can read them, so
    /// a waiting agent needs a fresh healthy interval under the new thresholds.
    pub fn publish_config(&self, config: Config) {
        if config.machine.overload != self.config().machine.overload {
            self.overload_recovery.reset();
        }
        super::forge_hosts::learn_forge_pins(&self.config().machine.forges, &config.machine.forges);
        self.config_state.publish(config);
        self.runner.wake_triggers();
    }

    /// Replace the global config with the file's current contents for later
    /// operations. An invalid file leaves the running config in place.
    pub async fn reload_config(&self) -> Result<()> {
        let _reload = self.config_state.lock_reload().await;
        let paths = self.paths.clone();
        let config = tokio::task::spawn_blocking(move || Config::load_settled(&paths)).await??;
        self.publish_config(config);
        Ok(())
    }

    pub(crate) async fn lock_repository(
        &self,
        selector: impl Into<crate::repository::Selector>,
    ) -> Result<(Repository, OwnedMutexGuard<()>)> {
        let repo = self.repository(selector).await?;
        let guard = self.gates.lock_git(&repo.id).await;
        // Removal may have completed or failed while this request waited.
        let repo = self.repository(&repo.id).await?;
        self.ensure_repository_available(&repo.id).await?;
        Ok((repo, guard))
    }
}

/// The store in `paths`' state directory, whose commits tests do not sync
/// to disk.
async fn open_store(paths: &Paths) -> Result<Store> {
    #[cfg(test)]
    return shoal_store::test_support::unsynced_store(paths.database()).await;
    #[cfg(not(test))]
    Store::open(paths.database()).await
}

#[cfg(test)]
mod tests;
