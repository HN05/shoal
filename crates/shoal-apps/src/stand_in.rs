//! A stand-in for the rest of the daemon, which the tests of apps build the
//! service over: a temporary store, a global config a test publishes, and
//! one repository whose local config a test gives.
use std::sync::{Arc, Mutex};

use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_daemon_base::config_state::ConfigState;
use shoal_protocol::protocol::ConfigTarget;

use crate::{
    Apps,
    config::{
        Config,
        repo::{ConfigLayers, RepoConfig},
    },
    daemon::Repositories,
};

/// The service over a stand-in daemon.
pub(crate) struct Fixture {
    _root: tempfile::TempDir,
    pub(crate) apps: Apps,
    pub(crate) config: Arc<ConfigState>,
    pub(crate) daemon: StandIn,
}

impl Fixture {
    pub(crate) async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let store = shoal_store::test_support::unsynced_store(path)
            .await
            .unwrap();
        let config = Arc::new(ConfigState::new(Config::default()));
        Self {
            _root: root,
            apps: Apps::new(store, config.clone()),
            config,
            daemon: StandIn::default(),
        }
    }

    /// Publish `toml` as the global config.
    pub(crate) fn configure(&self, toml: &str) {
        self.config.publish(Config::from_toml(toml).unwrap());
    }
}

/// The rest of the daemon, with a repository whose local config holds what
/// a test gives.
#[derive(Default)]
pub(crate) struct StandIn {
    local_config: Mutex<RepoConfig>,
}

impl StandIn {
    /// Give the repository the local config `toml`.
    pub(crate) fn set_local_config(&self, toml: &str) {
        *self.local_config.lock().unwrap() = toml::from_str(toml).unwrap();
    }
}

impl Repositories for StandIn {
    fn config_layers(&self, target: ConfigTarget) -> BoxFuture<'_, Result<ConfigLayers>> {
        let layers = match target {
            ConfigTarget::Global => ConfigLayers::default(),
            ConfigTarget::Repository(_) | ConfigTarget::Space(_) => ConfigLayers {
                local_config: self.local_config.lock().unwrap().clone(),
                ..ConfigLayers::default()
            },
        };
        Box::pin(async { Ok(layers) })
    }
}
