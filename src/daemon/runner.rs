//! The runner's background work: each repository's runner follows its
//! `runner.enabled` setting, checked when the global config changes and
//! every [`INTERVAL`] for the repositories' own files.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use tokio::time::Instant;

use crate::daemon::{log, workspace::Manager};

/// How often the runners are checked against the repositories' settings.
const INTERVAL: Duration = Duration::from_secs(60);
/// How long a repository whose runner could not change waits to try again.
const RETRY_AFTER: Duration = Duration::from_secs(5 * 60);

/// When each repository's runner last failed to change.
#[derive(Default)]
pub(crate) struct Runners {
    failures: Mutex<HashMap<String, Instant>>,
    /// The `PATH` tests find the runner's program on instead of the process's.
    #[cfg(test)]
    pub(crate) path: Mutex<Option<std::ffi::OsString>>,
}

impl Runners {
    /// The `PATH` the runner's program is found on: the daemon's, which
    /// each job's command inherits.
    pub(crate) fn search_path(&self) -> std::ffi::OsString {
        #[cfg(test)]
        if let Some(path) = self.path.lock().unwrap().clone() {
            return path;
        }
        std::env::var_os("PATH").unwrap_or_default()
    }

    /// Whether `repository_id`'s runner may be tried again.
    pub(crate) fn due(&self, repository_id: &str) -> bool {
        self.failures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(repository_id)
            .is_none_or(|failed| failed.elapsed() >= RETRY_AFTER)
    }

    pub(crate) fn failed(&self, repository_id: &str) {
        self.failures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(repository_id.to_owned(), Instant::now());
    }

    /// Try every repository again, as after the user changed the config.
    fn forget_failures(&self) {
        self.failures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

pub(super) async fn run(manager: Arc<Manager>) {
    let mut config = manager.config_state.watch();
    loop {
        if let Err(error) = manager.reconcile_runners().await {
            log!("runner: {error:#}");
        }
        tokio::select! {
            _ = config.changed() => manager.runners.forget_failures(),
            _ = tokio::time::sleep(INTERVAL) => {}
        }
    }
}
