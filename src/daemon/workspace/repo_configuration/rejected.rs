//! Worktree config files the current format rejects. The daemon ignores
//! them, and tells the log and the user once per error, since every pass
//! that reads the workspace's settings meets the same one.
use std::{collections::HashMap, sync::Mutex};

use crate::daemon::{
    log,
    notifications::{About, NotificationKind},
    workspace::Manager,
};
use crate::model::Workspace;

/// The error last reported for each workspace whose config file the
/// current format rejects, by workspace ID.
#[derive(Default)]
pub(crate) struct RejectedConfigs(Mutex<HashMap<String, String>>);

impl RejectedConfigs {
    /// Remember `error` for the workspace `id`; whether it differs from the
    /// one remembered.
    fn changed(&self, id: &str, error: &str) -> bool {
        let mut rejected = self.0.lock().unwrap();
        if rejected.get(id).is_some_and(|reported| reported == error) {
            return false;
        }
        rejected.insert(id.to_owned(), error.to_owned());
        true
    }

    /// Forget the workspace `id`, whose file the format accepts again or
    /// which is gone, so a later rejection is reported anew.
    pub(crate) fn forget(&self, id: &str) {
        self.0.lock().unwrap().remove(id);
    }
}

impl Manager {
    /// Log and tell the user that the daemon ignores `workspace`'s config
    /// file, which the format rejects with `error`, unless they heard of
    /// that error already.
    pub(super) async fn report_rejected_config(
        &self,
        workspace: &Workspace,
        error: &anyhow::Error,
    ) {
        let error = format!("{error:#}");
        if !self.rejected_configs.changed(&workspace.id, &error) {
            return;
        }
        log!("ignored the config file of {}: {error}", workspace.name);
        self.notify_about(
            About::workspace(&workspace.name, &workspace.id),
            NotificationKind::ConfigIgnored,
            format!("Config file ignored until fixed: {error}"),
        )
        .await;
    }
}
