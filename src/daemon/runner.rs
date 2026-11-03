//! The runner's background work: each repository's runner follows its
//! `runner.enabled` setting, checked when the global config changes and
//! every [`INTERVAL`] for the repositories' own files; every
//! [`POLL_INTERVAL`], and whenever admission admits a job, the jobs that
//! ended give up their workspaces, admitted jobs are claimed, and the
//! forge's waiting jobs are read while the machine has room for one.
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use tokio::{sync::Notify, time::Instant};

use crate::daemon::{log, workspace::Manager};

/// How often the runners are checked against the repositories' settings.
const INTERVAL: Duration = Duration::from_secs(60);
/// How often the forge's waiting jobs are read, as often as admission
/// samples the machine's load.
const POLL_INTERVAL: Duration = Duration::from_secs(10);
/// How long a repository whose runner could not change waits to try again.
const RETRY_AFTER: Duration = Duration::from_secs(5 * 60);

/// How long a repository's jobs wait after its permit pool was full.
const PERMITS_BUSY_FOR: Duration = Duration::from_secs(15);

/// What the runner keeps between passes: when each repository's runner last
/// failed to change or found its permits all held, and the jobs being claimed.
#[derive(Default)]
pub(crate) struct Runners {
    failures: Mutex<HashMap<String, Instant>>,
    permits_busy: Mutex<HashMap<String, Instant>>,
    /// The queue entries of the jobs whose claim runs.
    claiming: Mutex<HashSet<String>>,
    /// Admission admitted a job, or one ended.
    wake: Notify,
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
        lock(&self.failures)
            .get(repository_id)
            .is_none_or(|failed| failed.elapsed() >= RETRY_AFTER)
    }

    pub(crate) fn failed(&self, repository_id: &str) {
        lock(&self.failures).insert(repository_id.to_owned(), Instant::now());
    }

    /// Whether `repository_id`'s permit pool was full a moment ago.
    pub(crate) fn permits_busy(&self, repository_id: &str) -> bool {
        lock(&self.permits_busy)
            .get(repository_id)
            .is_some_and(|busy| busy.elapsed() < PERMITS_BUSY_FOR)
    }

    pub(crate) fn found_permits_busy(&self, repository_id: &str) {
        lock(&self.permits_busy).insert(repository_id.to_owned(), Instant::now());
    }

    #[cfg(test)]
    pub(crate) fn forget_permits_busy(&self) {
        lock(&self.permits_busy).clear();
    }

    /// Take the claim of the job `entry`, unless one runs.
    pub(crate) fn begin_claim(&self, entry: &str) -> bool {
        lock(&self.claiming).insert(entry.to_owned())
    }

    pub(crate) fn end_claim(&self, entry: &str) {
        lock(&self.claiming).remove(entry);
    }

    pub(crate) fn claiming(&self, entry: &str) -> bool {
        lock(&self.claiming).contains(entry)
    }

    /// Run the runner's pass now.
    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    /// Try every repository again, as after the user changed the config.
    fn forget_failures(&self) {
        lock(&self.failures).clear();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(super) async fn run(manager: Arc<Manager>) {
    let mut config = manager.config_state.watch();
    let mut reconciled: Option<Instant> = None;
    loop {
        if reconciled.is_none_or(|at| at.elapsed() >= INTERVAL) {
            if let Err(error) = manager.reconcile_runners().await {
                log!("runner: {error:#}");
            }
            reconciled = Some(Instant::now());
        }
        if let Err(error) = pass(&manager).await {
            log!("runner: {error:#}");
        }
        tokio::select! {
            _ = config.changed() => {
                manager.runners.forget_failures();
                reconciled = None;
            }
            _ = manager.runners.wake.notified() => {}
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }
    }
}

/// Remove the workspaces of the jobs that ended, claim the jobs admission
/// admitted, and read the forge's waiting jobs while one could start.
async fn pass(manager: &Arc<Manager>) -> anyhow::Result<()> {
    manager.finish_ended_jobs().await?;
    for entry in manager.admitted_jobs().await? {
        if manager.runners.begin_claim(&entry.id) {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager.claim_job(&entry).await;
                manager.runners.end_claim(&entry.id);
                manager.agent_queue.wake();
            });
        }
    }
    if manager.job_could_start().await? {
        manager.refresh_waiting_jobs().await?;
    }
    Ok(())
}
