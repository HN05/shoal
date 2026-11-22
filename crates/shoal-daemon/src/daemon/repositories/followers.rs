//! Those who follow a repository's local config.
use shoal_repositories::daemon::Followers;

use crate::daemon::manager::Manager;

impl Followers for Manager {
    fn local_config_changed(&self) {
        // The runner follows `runner.enabled` as soon as it changes.
        self.runner.jobs().settings_changed();
        self.hub.shared_local_configs_changed();
    }
}
