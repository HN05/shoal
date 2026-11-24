//! The CI jobs this machine runs for repositories' forges: what the runner
//! keeps between passes, what stops a job's runner, and the labels a runner
//! registers with.
use std::{
    collections::{HashMap, HashSet},
    sync::{Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use shoal_ids::{queue_entry_ids::QueueEntryId, repository_ids::RepositoryId};
use tokio::{sync::Notify, time::Instant};

use crate::{Runner, log};
use shoal_daemon_base::jobs::JobStop;

mod cancels;
mod claimed;
mod claims;
mod costs;
mod ends;
mod linked;
mod listing;
mod load;
mod memory;
mod overrun;
mod passes;
mod queue;
mod registration;
mod reruns;
mod single_use;
mod superseded;
mod taken;
mod untaken;
mod waiting;

pub use claimed::ClaimedJob;
pub use load::JobLoad;
pub use overrun::GRACE;
pub use passes::run;
pub use registration::Registration;

impl Runner {
    /// What the runner keeps between passes.
    pub fn jobs(&self) -> &Runners {
        &self.jobs
    }

    /// Record in Shoal's log that `cause` stopped the runner of `job`, with
    /// the `outcome` for its run.
    pub fn log_job_stop(&self, job: &ClaimedJob, cause: JobStop, outcome: &str) {
        log!(
            "runner: {} of run {} stopped by {cause}; {outcome}",
            job.job.job.name,
            job.job.run.url
        );
        #[cfg(any(test, feature = "test-support"))]
        (self.jobs.stops.lock().unwrap()).push((job.workspace_id.clone(), cause));
    }
}

/// How often the runners are checked against the repositories' settings,
/// and the forge's waiting jobs read while none could start.
pub const INTERVAL: Duration = Duration::from_secs(60);
/// How long a repository whose runner could not change waits to try again.
const RETRY_AFTER: Duration = Duration::from_secs(5 * 60);

/// How long a repository's jobs wait after its permit pool was full.
const PERMITS_BUSY_FOR: Duration = Duration::from_secs(15);

/// How often the run of a running job is read on its forge.
const RUN_READ_INTERVAL: Duration = Duration::from_secs(60);

/// What the forge said about the run of a running job.
#[derive(Default, Clone, Copy)]
struct RunCheck {
    /// When it was last read.
    read: Option<Instant>,
    /// When it was first found finished.
    finished: Option<Instant>,
}

/// What the runner keeps between passes: when each repository's runner last
/// failed to change or found its permits all held, and the jobs being claimed.
#[derive(Default)]
pub struct Runners {
    failures: Mutex<HashMap<RepositoryId, Instant>>,
    permits_busy: Mutex<HashMap<RepositoryId, Instant>>,
    /// When the forge's waiting jobs were last read.
    waiting_read: Mutex<Option<Instant>>,
    /// The queue entries of the jobs whose claim runs.
    claiming: Mutex<HashSet<QueueEntryId>>,
    /// The runs of the running jobs, by queue entry.
    runs: Mutex<HashMap<QueueEntryId, RunCheck>>,
    /// The running jobs claimed by handle whose runner the forge says took
    /// them, by queue entry.
    taken: Mutex<HashSet<QueueEntryId>>,
    /// When the runs of the jobs waiting to rerun were last read.
    reruns_read: Mutex<Option<Instant>>,
    /// Held while the jobs waiting to rerun are rerun.
    rerunning: tokio::sync::Mutex<()>,
    /// Admission admitted a job, or one ended.
    wake: Notify,
    /// Held while a claim writes a job's files and while the sweep of
    /// unclaimed directories decides on one.
    job_files: tokio::sync::Mutex<()>,
    /// A repository's settings changed since the runners were checked.
    changed: std::sync::atomic::AtomicBool,
    /// The `PATH` tests find the runner's program on instead of the process's.
    #[cfg(any(test, feature = "test-support"))]
    pub path: Mutex<Option<std::ffi::OsString>>,
    /// The workspace of each job whose stop Shoal's log recorded, with what
    /// stopped it, for tests, which do not read the log.
    #[cfg(any(test, feature = "test-support"))]
    pub stops: Mutex<Vec<(shoal_ids::space_ids::SpaceId, JobStop)>>,
}

impl Runners {
    /// The `PATH` the runner's program is found on: the daemon's, which
    /// each job's command inherits.
    pub fn search_path(&self) -> std::ffi::OsString {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(path) = self.path.lock().unwrap().clone() {
            return path;
        }
        std::env::var_os("PATH").unwrap_or_default()
    }

    /// Whether `repository_id`'s runner may be tried again.
    pub fn due(&self, repository_id: &RepositoryId) -> bool {
        lock(&self.failures)
            .get(repository_id)
            .is_none_or(|failed| failed.elapsed() >= RETRY_AFTER)
    }

    pub fn failed(&self, repository_id: &RepositoryId) {
        lock(&self.failures).insert(repository_id.to_owned(), Instant::now());
    }

    /// Whether `repository_id`'s permit pool was full a moment ago.
    pub fn permits_busy(&self, repository_id: &RepositoryId) -> bool {
        lock(&self.permits_busy)
            .get(repository_id)
            .is_some_and(|busy| busy.elapsed() < PERMITS_BUSY_FOR)
    }

    pub fn found_permits_busy(&self, repository_id: &RepositoryId) {
        lock(&self.permits_busy).insert(repository_id.to_owned(), Instant::now());
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn forget_permits_busy(&self) {
        lock(&self.permits_busy).clear();
    }

    /// Whether the forge's waiting jobs are due for a read whether or not
    /// one could start.
    pub fn waiting_jobs_due(&self) -> bool {
        lock(&self.waiting_read).is_none_or(|read| read.elapsed() >= INTERVAL)
    }

    pub fn read_waiting_jobs(&self) {
        *lock(&self.waiting_read) = Some(Instant::now());
    }

    /// Make the last read of the forge's waiting jobs `age` old.
    #[cfg(any(test, feature = "test-support"))]
    pub fn age_waiting_read(&self, age: Duration) {
        *lock(&self.waiting_read) = Instant::now().checked_sub(age);
    }

    /// Take the claim of the job `entry`, unless one runs.
    pub fn begin_claim(&self, entry: &QueueEntryId) -> bool {
        lock(&self.claiming).insert(entry.clone())
    }

    pub fn end_claim(&self, entry: &QueueEntryId) {
        lock(&self.claiming).remove(entry);
    }

    pub fn claiming(&self, entry: &QueueEntryId) -> bool {
        lock(&self.claiming).contains(entry)
    }

    /// Whether the runs of the jobs waiting to rerun are to be read, which
    /// marks them read: as often as a running job's run.
    pub fn take_reruns_read(&self) -> bool {
        let mut read = lock(&self.reruns_read);
        let due = read.is_none_or(|read| read.elapsed() >= RUN_READ_INTERVAL);
        if due {
            *read = Some(Instant::now());
        }
        due
    }

    /// Whether the run of the running job `entry` is to be read, which
    /// marks it read: one found finished stays so, and one read a moment
    /// ago waits.
    pub fn take_run_read(&self, entry: &QueueEntryId) -> bool {
        let mut runs = lock(&self.runs);
        let check = runs.entry(entry.clone()).or_default();
        let due = check.finished.is_none()
            && check
                .read
                .is_none_or(|read| read.elapsed() >= RUN_READ_INTERVAL);
        if due {
            check.read = Some(Instant::now());
        }
        due
    }

    /// The forge reported the run of the running job `entry` finished.
    pub fn run_finished(&self, entry: &QueueEntryId) {
        let mut runs = lock(&self.runs);
        let check = runs.entry(entry.clone()).or_default();
        check.finished.get_or_insert_with(Instant::now);
    }

    /// How long ago the forge reported the run of the running job `entry`
    /// finished, if it did.
    pub fn finished_for(&self, entry: &QueueEntryId) -> Option<Duration> {
        lock(&self.runs)
            .get(entry)
            .and_then(|check| check.finished)
            .map(|finished| finished.elapsed())
    }

    /// Forget what is kept of the jobs other than `claimed`.
    pub fn keep_claimed(&self, claimed: &HashSet<QueueEntryId>) {
        lock(&self.runs).retain(|entry, _| claimed.contains(entry));
        lock(&self.taken).retain(|entry| claimed.contains(entry));
    }

    /// The forge said the runner of the running job `entry` took it.
    pub fn took(&self, entry: &QueueEntryId) {
        lock(&self.taken).insert(entry.clone());
    }

    /// Whether the forge said the runner of the running job `entry` took it.
    pub fn taken(&self, entry: &QueueEntryId) -> bool {
        lock(&self.taken).contains(entry)
    }

    /// Take each run found finished to have finished `by` earlier.
    #[cfg(any(test, feature = "test-support"))]
    pub fn backdate_finished_runs(&self, by: Duration) {
        for check in lock(&self.runs).values_mut() {
            check.finished = check.finished.and_then(|finished| finished.checked_sub(by));
        }
    }

    /// The lock over the runner's job directories.
    pub async fn job_files(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.job_files.lock().await
    }

    /// Run the runner's pass now.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Wait until a pass is asked for.
    pub async fn woken(&self) {
        self.wake.notified().await;
    }

    /// Check the runners against the repositories' settings now, as after
    /// an edit of a repository's local config.
    pub fn settings_changed(&self) {
        self.changed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.wake();
    }

    /// Whether a pass was asked for since the last call.
    #[cfg(any(test, feature = "test-support"))]
    pub fn take_wake(&self) -> bool {
        use futures_util::FutureExt;
        self.wake.notified().now_or_never().is_some()
    }

    pub fn take_changed(&self) -> bool {
        self.changed
            .swap(false, std::sync::atomic::Ordering::Relaxed)
    }

    /// Try every repository again, as after the user changed the config.
    pub fn forget_failures(&self) {
        lock(&self.failures).clear();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The label every runner registers besides the configured ones, naming the
/// machine's system and architecture, such as `shoal-macos-arm64`.
fn platform_label() -> String {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        arch => arch,
    };
    format!("shoal-{}-{arch}", std::env::consts::OS)
}

/// `configured` with the platform label, each once.
pub fn runner_labels(configured: &[String]) -> Vec<String> {
    let mut labels = configured.to_vec();
    let platform = platform_label();
    if !labels.contains(&platform) {
        labels.push(platform);
    }
    labels
}
