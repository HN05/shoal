//! Worktree config files the current format rejects. The daemon reports
//! each error once, since every pass that reads the workspace's settings
//! meets the same one.
use std::{collections::HashMap, sync::Mutex};

use shoal_ids::space_ids::SpaceId;

use shoal_daemon_base::{
    alerts::NewAlert,
    log,
    notifications::{About, AlertKind},
};
use shoal_protocol::model::Workspace;

use crate::{Repositories, daemon::Daemon};

/// The error last reported for each workspace whose config file the
/// current format rejects, by workspace ID.
#[derive(Default)]
pub(crate) struct RejectedConfigs(Mutex<HashMap<SpaceId, String>>);

impl RejectedConfigs {
    /// Remember `error` for the workspace `id`; whether it differs from the
    /// one remembered.
    fn changed(&self, id: &SpaceId, error: &str) -> bool {
        let mut rejected = self.0.lock().unwrap();
        if rejected.get(id).is_some_and(|reported| reported == error) {
            return false;
        }
        rejected.insert(id.clone(), error.to_owned());
        true
    }

    /// Forget the workspace `id`, whose file the format accepts again or
    /// which is gone, so a later rejection is reported anew.
    pub(crate) fn forget(&self, id: &SpaceId) {
        self.0.lock().unwrap().remove(id);
    }
}

impl Repositories {
    /// Forget the rejected config file of the workspace `id`, which is
    /// gone.
    pub fn forget_rejected_config(&self, id: &SpaceId) {
        self.rejected_configs.forget(id);
    }

    /// Log and tell the user that the daemon ignores `workspace`'s config
    /// file, which the format rejects with `error`, unless they heard of
    /// that error already.
    pub(super) async fn report_rejected_config(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        error: &anyhow::Error,
    ) {
        let error = format!("{error:#}");
        if !self.rejected_configs.changed(&workspace.id, &error) {
            return;
        }
        log!("ignored the config file of {}: {error}", workspace.name);
        let alert = NewAlert::shoal(
            AlertKind::ConfigIgnored,
            About::space(&workspace.name, &workspace.id),
            format!("Config file ignored until fixed: {error}"),
        );
        self.notifications.notify(daemon, alert).await;
    }
}
