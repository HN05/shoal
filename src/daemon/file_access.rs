//! Directories whose file access stalls. A check of a worktree, such as a Git
//! the daemon starts there, can wait on file access without end: while macOS
//! waits for an answer to a privacy prompt, every new process that opens a
//! file in the protected location waits until the prompt is answered or the
//! kernel interrupts it, whichever process the prompt is for. Checks run under
//! a time limit. One that runs out or is interrupted records the directory
//! holding the checked path as stalled, which the daemon reports once, and
//! later checks of paths there skip it until a recheck finishes.
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use anyhow::{Result, anyhow};
use tokio::time::Instant;

use super::{log, notifications::NotificationKind, workspace::Manager};
use crate::protocol::AccessStall;

/// A check still running after this long counts as stalled. Git answers a
/// check of a directory it can read within seconds, even under load.
pub(crate) const LIMIT: Duration = Duration::from_secs(30);

/// A stalled directory is checked again this long after its last check, so
/// each cleanup sweep rechecks it once while its other checks skip it.
pub(crate) const RECHECK_AFTER: Duration = Duration::from_secs(20);

/// What a process reports when the kernel interrupts its wait on file access.
const INTERRUPTED: &str = "Interrupted system call";

/// The stalled directories, and the checks running now.
#[derive(Default)]
pub(crate) struct FileAccess {
    stalls: Mutex<BTreeMap<PathBuf, Stalled>>,
    running: Mutex<Running>,
}

struct Stalled {
    report: AccessStall,
    checked: Instant,
    /// Whether a recheck runs now, which the directory's other checks leave to.
    rechecking: bool,
}

/// A recheck of a stalled directory, which lets another start when dropped.
struct Recheck<'a> {
    access: &'a FileAccess,
    directory: PathBuf,
}

impl Drop for Recheck<'_> {
    fn drop(&mut self) {
        if let Some(stalled) = lock(&self.access.stalls).get_mut(&self.directory) {
            stalled.rechecking = false;
        }
    }
}

#[derive(Default)]
struct Running {
    next: u64,
    checks: BTreeMap<u64, (PathBuf, Instant)>,
}

/// A running check, which leaves [`FileAccess::waiting`] when dropped.
struct RunningCheck<'a> {
    access: &'a FileAccess,
    id: u64,
}

impl Drop for RunningCheck<'_> {
    fn drop(&mut self) {
        lock(&self.access.running).checks.remove(&self.id);
    }
}

impl FileAccess {
    /// Run `check` of `path` within [`LIMIT`], unless its directory stalled
    /// within [`RECHECK_AFTER`] or another check rechecks it. A check that
    /// runs out or is interrupted fails with the stall; any other outcome
    /// shows the directory answers. The stall comes back too when the
    /// directory was not stalled before.
    async fn check<T>(
        &self,
        path: &Path,
        check: impl Future<Output = Result<T>>,
    ) -> (Result<T>, Option<AccessStall>) {
        let directory = directory(path);
        let _recheck = match self.reserve(&directory) {
            Ok(recheck) => recheck,
            Err(stall) => return (Err(stalled(&stall)), None),
        };
        let running = self.start(path);
        let outcome = tokio::time::timeout(LIMIT, check).await;
        drop(running);
        let (error, reason) = match outcome {
            Ok(Err(error)) if interrupted(&error) => (Some(error), "was interrupted".to_owned()),
            Ok(result) => {
                self.recover(&directory);
                return (result, None);
            }
            Err(_) => (
                None,
                format!("did not finish within {}", log::elapsed(LIMIT)),
            ),
        };
        let (stall, new) = self.record(directory, path, reason);
        let error = match error {
            Some(error) => error.context(stalled(&stall)),
            None => stalled(&stall),
        };
        (Err(error), new.then_some(stall))
    }

    /// The stall of `path`'s directory, however long ago it was checked.
    pub(crate) fn stalled(&self, path: &Path) -> Option<AccessStall> {
        lock(&self.stalls)
            .get(&directory(path))
            .map(|stalled| stalled.report.clone())
    }

    /// Every stalled directory.
    pub(crate) fn stalls(&self) -> Vec<AccessStall> {
        lock(&self.stalls)
            .values()
            .map(|stalled| stalled.report.clone())
            .collect()
    }

    /// The path of the check that has run longest, and for how long.
    pub(crate) fn waiting(&self) -> Option<(PathBuf, Duration)> {
        lock(&self.running)
            .checks
            .values()
            .min_by_key(|(_, started)| *started)
            .map(|(path, started)| (path.clone(), started.elapsed()))
    }

    /// Whether a check in `directory` may run: refused with the stall while
    /// the directory stalled recently or another check rechecks it, and
    /// otherwise the recheck of a stalled directory it reserves.
    fn reserve(&self, directory: &Path) -> Result<Option<Recheck<'_>>, AccessStall> {
        let mut stalls = lock(&self.stalls);
        let Some(stalled) = stalls.get_mut(directory) else {
            return Ok(None);
        };
        if stalled.rechecking || stalled.checked.elapsed() < RECHECK_AFTER {
            return Err(stalled.report.clone());
        }
        stalled.rechecking = true;
        Ok(Some(Recheck {
            access: self,
            directory: directory.to_owned(),
        }))
    }

    fn start(&self, path: &Path) -> RunningCheck<'_> {
        let mut running = lock(&self.running);
        let id = running.next;
        running.next += 1;
        running.checks.insert(id, (path.to_owned(), Instant::now()));
        RunningCheck { access: self, id }
    }

    /// Record that a check of `path` in `directory` stalled, keeping the
    /// first report of a stall that goes on. Returns the report and whether
    /// the directory was not stalled before.
    fn record(&self, directory: PathBuf, path: &Path, reason: String) -> (AccessStall, bool) {
        let mut stalls = lock(&self.stalls);
        if let Some(stalled) = stalls.get_mut(&directory) {
            stalled.checked = Instant::now();
            return (stalled.report.clone(), false);
        }
        let report = AccessStall {
            directory: directory.clone(),
            path: path.to_owned(),
            since: crate::time::unix_seconds(),
            reason,
        };
        log!("{}", report.message());
        stalls.insert(
            directory,
            Stalled {
                report: report.clone(),
                checked: Instant::now(),
                rechecking: false,
            },
        );
        (report, true)
    }

    fn recover(&self, directory: &Path) {
        if lock(&self.stalls).remove(directory).is_some() {
            log!("file access in {} answers again", directory.display());
        }
    }
}

impl Manager {
    /// Run `check` of `path` under [`FileAccess`]'s time limit, and tell
    /// the user once when its directory stalls.
    pub(crate) async fn check_access<T>(
        &self,
        path: &Path,
        check: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        let (result, stall) = self.file_access.check(path, check).await;
        if let Some(stall) = stall {
            self.notify(None, NotificationKind::AccessStalled, stall.message())
                .await;
        }
        result
    }
}

/// The directory holding `path`, such as a repository's directory of
/// workspaces, whose other paths a stall there likely holds too. It is found
/// from the path alone, since reading the filesystem could stall too. A root
/// stands for itself.
fn directory(path: &Path) -> PathBuf {
    path.parent().unwrap_or(path).to_owned()
}

/// Whether `error` says the kernel interrupted a wait, as it ends one on a
/// stalled file access.
fn interrupted(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::Interrupted)
            || cause.to_string().contains(INTERRUPTED)
    })
}

fn stalled(stall: &AccessStall) -> anyhow::Error {
    anyhow!(
        "file access in {} stalled; a macOS privacy prompt may be waiting for an answer",
        stall.directory.display()
    )
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
