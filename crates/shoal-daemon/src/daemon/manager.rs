//! `Manager`, the daemon's shared state, with its configuration.
use crate::config_file::ConfigExt;
use crate::paths::StateFiles;

use crate::{config::Config, daemon::store::Store, paths::Paths};
use anyhow::Result;
use shoal_daemon_base::forge_access::{self, ForgeAccess};
use std::sync::Arc;

pub struct Manager {
    pub store: Store,
    pub gates: Arc<super::gates::Gates>,
    /// The triggers and the CI jobs this machine runs.
    pub runner: Arc<shoal_runner::Runner>,
    pub config_state: Arc<super::config_state::ConfigState>,
    pub notifications: Arc<super::notifications::Notifications>,
    /// The leases workspaces hold.
    pub leases: shoal_leases::Leases,
    pub paths: Paths,
    pub(crate) operations: Arc<crate::daemon::operations::Operations>,
    /// The alerts, threads, approvals, permissions and scope between spaces.
    pub coordination: shoal_coordination::Coordination,
    /// The workspaces and chats: their records and tree, lifecycle, removal
    /// and cleanup.
    pub spaces: shoal_spaces::Spaces,
    pub(crate) signals: Arc<super::signals::Signals>,
    /// The agents.
    pub agents: Arc<shoal_agents::Agents>,
    /// The registered repositories.
    pub repositories: Arc<shoal_repositories::Repositories>,
    /// The issues, PRs and CI runs the spaces link.
    pub links: Arc<shoal_links::Links>,
    /// The local reviews of workspaces.
    pub reviews: Arc<shoal_reviews::Reviews>,
    /// Directories whose file access stalls, shared with startup's status answer.
    pub(crate) file_access: Arc<crate::daemon::file_access::FileAccess>,
    /// The hub role, or this machine's membership of a hub.
    pub hub: Arc<shoal_hub::Hub>,
    /// What apps keep in Shoal.
    pub apps: shoal_apps::Apps,
    /// The demos executions run.
    pub demos: shoal_demos::Demos,
    /// The accounts and forge hosts the daemon reaches the forges with.
    pub forges: Arc<ForgeAccess>,
}

impl Manager {
    pub async fn open(paths: Paths) -> Result<Arc<Self>> {
        paths.prepare()?;
        let config = Config::load(&paths)?;
        forge_access::learn_forge_pins(&Default::default(), &config.machine.forges);
        let store = open_store(&paths).await?;
        let config_state = Arc::new(super::config_state::ConfigState::new(config));
        let notifications = Arc::new(super::notifications::Notifications::new(store.clone()));
        let gates = Arc::new(super::gates::Gates::default());
        let forges = Arc::new(ForgeAccess::new(
            store.clone(),
            gates.clone(),
            config_state.clone(),
        ));
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
            gates.clone(),
            config_state.clone(),
            notifications.clone(),
            forges.clone(),
            Arc::new(super::runner::RunnerRecords),
            paths.clone(),
        ));
        let file_access = Arc::new(super::file_access::FileAccess::default());
        let space_removals = crate::daemon::spaces::Removing::default();
        let space_records: Arc<dyn shoal_spaces::daemon::Records> =
            Arc::new(super::space_service::SpaceRecords {
                coordination: super::coordination::CoordinationRecords(space_removals.clone()),
            });
        let links = Arc::new(shoal_links::Links::new(
            store.clone(),
            gates.clone(),
            config_state.clone(),
            notifications.clone(),
            file_access.clone(),
            forges.clone(),
            Arc::new(super::links::records::LinkRecords),
        ));
        let repositories = Arc::new(shoal_repositories::Repositories::new(
            store.clone(),
            gates.clone(),
            config_state.clone(),
            notifications.clone(),
            file_access.clone(),
            forges.clone(),
            paths.clone(),
            Arc::new(super::repositories::RepositoryRecords(
                space_records.clone(),
            )),
        ));
        let signals = Arc::new(super::signals::Signals::default());
        let agents = Arc::new(shoal_agents::Agents::new(
            store.clone(),
            gates.clone(),
            config_state.clone(),
            signals.clone(),
            notifications.clone(),
            file_access.clone(),
            paths.clone(),
            Arc::new(super::agent_service::AgentRecords),
            Arc::new(super::agent_service::SpaceRemovals(space_removals.clone())),
            // Tests inject hosts and never discover or close the user's
            // sessions.
            if crate::daemon::fake_workers::faked() {
                Vec::new()
            } else {
                crate::sessions::hosts()
            },
        ));
        let reviews = Arc::new(shoal_reviews::Reviews::new(
            store.clone(),
            gates.clone(),
            forges.clone(),
            Arc::new(super::reviews::ReviewRecords),
            Arc::new(super::gate::GatedRequests),
            paths.clone(),
        ));
        let apps = shoal_apps::Apps::new(store.clone(), config_state.clone());
        let demos = shoal_demos::Demos::new(store.clone(), paths.clone());
        let coordination = shoal_coordination::Coordination::new(
            store.clone(),
            gates.clone(),
            config_state.clone(),
            notifications.clone(),
            signals.clone(),
            Arc::new(super::coordination::CoordinationRecords(
                space_removals.clone(),
            )),
        );
        let spaces = shoal_spaces::Spaces::new(
            store.clone(),
            gates.clone(),
            config_state.clone(),
            notifications.clone(),
            signals.clone(),
            paths.clone(),
            file_access.clone(),
            forges.clone(),
            space_removals,
            space_records,
        );
        Ok(Arc::new(Self {
            config_state,
            notifications,
            leases,
            runner,
            store,
            gates,
            paths,
            operations: Default::default(),
            coordination,
            spaces,
            signals,
            agents,
            repositories,
            links,
            reviews,
            file_access,
            hub,
            apps,
            demos,
            forges,
        }))
    }

    /// This daemon as the spaces' interface, which their operations that go
    /// on in the background keep.
    pub fn daemon(self: &Arc<Self>) -> Arc<dyn shoal_spaces::daemon::Daemon> {
        self.clone()
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
    /// settings or memory limit withdraw recovery readiness before anything
    /// can read them, so a waiting agent needs a fresh healthy interval under
    /// the new thresholds.
    pub fn publish_config(&self, config: Config) {
        if super::overload::protection_changed(&self.config(), &config) {
            self.agents.overload_recovery().reset();
        }
        forge_access::learn_forge_pins(&self.config().machine.forges, &config.machine.forges);
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
