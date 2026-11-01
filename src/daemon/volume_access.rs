//! Volumes whose file access stalls. While macOS waits for an answer to a
//! privacy prompt for a removable volume, every new process that opens a file
//! there, such as each Git the daemon starts, waits until the prompt is
//! answered or the kernel interrupts it, whichever process the prompt is for.
//! Checks of a volume run under a time limit. One that runs out or is
//! interrupted records the volume as stalled, which the daemon reports once,
//! and later checks skip it until a recheck finishes.
use std::{
    collections::BTreeMap,
    io,
    path::{Component, Path, PathBuf},
    sync::{Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use anyhow::{Result, anyhow};
use tokio::time::Instant;

use super::{log, notifications::NotificationKind, workspace::Manager};
use crate::protocol::VolumeStall;

/// A check still running after this long counts as stalled. Git answers a
/// check on a volume it can read within seconds, even under load.
pub(crate) const LIMIT: Duration = Duration::from_secs(30);

/// A stalled volume is checked again this long after its last check, so
/// each cleanup sweep rechecks it once while its other checks skip it.
pub(crate) const RECHECK_AFTER: Duration = Duration::from_secs(20);

/// What a process reports when the kernel interrupts its wait on the volume.
const INTERRUPTED: &str = "Interrupted system call";

/// The stalled volumes, and the checks running now.
#[derive(Default)]
pub(crate) struct VolumeAccess {
    stalls: Mutex<BTreeMap<PathBuf, Stalled>>,
    running: Mutex<Running>,
}

struct Stalled {
    report: VolumeStall,
    checked: Instant,
    /// Whether a recheck runs now, which the volume's other checks leave to.
    rechecking: bool,
}

/// A recheck of a stalled volume, which lets another start when dropped.
struct Recheck<'a> {
    access: &'a VolumeAccess,
    volume: PathBuf,
}

impl Drop for Recheck<'_> {
    fn drop(&mut self) {
        if let Some(stalled) = lock(&self.access.stalls).get_mut(&self.volume) {
            stalled.rechecking = false;
        }
    }
}

#[derive(Default)]
struct Running {
    next: u64,
    checks: BTreeMap<u64, (PathBuf, Instant)>,
}

/// A running check, which leaves [`VolumeAccess::waiting`] when dropped.
struct RunningCheck<'a> {
    access: &'a VolumeAccess,
    id: u64,
}

impl Drop for RunningCheck<'_> {
    fn drop(&mut self) {
        lock(&self.access.running).checks.remove(&self.id);
    }
}

impl VolumeAccess {
    /// Run `check` of `path` within [`LIMIT`], unless its volume stalled
    /// within [`RECHECK_AFTER`] or another check rechecks it. A check that
    /// runs out or is interrupted
    /// fails with the stall; any other outcome shows the volume answers. The
    /// stall comes back too when the volume was not stalled before.
    async fn check<T>(
        &self,
        path: &Path,
        check: impl Future<Output = Result<T>>,
    ) -> (Result<T>, Option<VolumeStall>) {
        let volume = volume(path);
        let _recheck = match self.reserve(&volume) {
            Ok(recheck) => recheck,
            Err(stall) => return (Err(stalled(&stall)), None),
        };
        let running = self.start(path);
        let outcome = tokio::time::timeout(LIMIT, check).await;
        drop(running);
        let (error, reason) = match outcome {
            Ok(Err(error)) if interrupted(&error) => (Some(error), "was interrupted".to_owned()),
            Ok(result) => {
                self.recover(&volume);
                return (result, None);
            }
            Err(_) => (
                None,
                format!("did not finish within {}", log::elapsed(LIMIT)),
            ),
        };
        let (stall, new) = self.record(volume, path, reason);
        let error = match error {
            Some(error) => error.context(stalled(&stall)),
            None => stalled(&stall),
        };
        (Err(error), new.then_some(stall))
    }

    /// The stall of `path`'s volume, however long ago it was checked.
    pub(crate) fn stalled(&self, path: &Path) -> Option<VolumeStall> {
        lock(&self.stalls)
            .get(&volume(path))
            .map(|stalled| stalled.report.clone())
    }

    /// Every stalled volume.
    pub(crate) fn stalls(&self) -> Vec<VolumeStall> {
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

    /// Whether a check of `volume` may run: refused with the stall while the
    /// volume stalled recently or another check rechecks it, and otherwise
    /// the recheck of a stalled volume it reserves.
    fn reserve(&self, volume: &Path) -> Result<Option<Recheck<'_>>, VolumeStall> {
        let mut stalls = lock(&self.stalls);
        let Some(stalled) = stalls.get_mut(volume) else {
            return Ok(None);
        };
        if stalled.rechecking || stalled.checked.elapsed() < RECHECK_AFTER {
            return Err(stalled.report.clone());
        }
        stalled.rechecking = true;
        Ok(Some(Recheck {
            access: self,
            volume: volume.to_owned(),
        }))
    }

    fn start(&self, path: &Path) -> RunningCheck<'_> {
        let mut running = lock(&self.running);
        let id = running.next;
        running.next += 1;
        running.checks.insert(id, (path.to_owned(), Instant::now()));
        RunningCheck { access: self, id }
    }

    /// Record that a check of `path` on `volume` stalled, keeping the first
    /// report of a stall that goes on. Returns the report and whether the
    /// volume was not stalled before.
    fn record(&self, volume: PathBuf, path: &Path, reason: String) -> (VolumeStall, bool) {
        let mut stalls = lock(&self.stalls);
        if let Some(stalled) = stalls.get_mut(&volume) {
            stalled.checked = Instant::now();
            return (stalled.report.clone(), false);
        }
        let report = VolumeStall {
            volume: volume.clone(),
            path: path.to_owned(),
            since: crate::time::unix_seconds(),
            reason,
        };
        log!("{}", report.message());
        stalls.insert(
            volume,
            Stalled {
                report: report.clone(),
                checked: Instant::now(),
                rechecking: false,
            },
        );
        (report, true)
    }

    fn recover(&self, volume: &Path) {
        if lock(&self.stalls).remove(volume).is_some() {
            log!("file access on {} answers again", volume.display());
        }
    }
}

impl Manager {
    /// Run `check` of `path` under [`VolumeAccess`]'s time limit, and tell
    /// the user once when its volume stalls.
    pub(crate) async fn check_volume<T>(
        &self,
        path: &Path,
        check: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        let (result, stall) = self.volume_access.check(path, check).await;
        if let Some(stall) = stall {
            self.notify(None, NotificationKind::VolumeStalled, stall.message())
                .await;
        }
        result
    }
}

/// The mounted volume `path` is on, such as `/Volumes/SSD`, found from the
/// path alone, since reading the filesystem could stall too. Any other path
/// stands for itself.
fn volume(path: &Path) -> PathBuf {
    let mut components = path.components();
    match (components.next(), components.next(), components.next()) {
        (
            Some(Component::RootDir),
            Some(Component::Normal(volumes)),
            Some(Component::Normal(name)),
        ) if volumes == "Volumes" => Path::new("/Volumes").join(name),
        _ => path.to_owned(),
    }
}

/// Whether `error` says the kernel interrupted a wait, as it ends one on a
/// stalled volume.
fn interrupted(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::Interrupted)
            || cause.to_string().contains(INTERRUPTED)
    })
}

fn stalled(stall: &VolumeStall) -> anyhow::Error {
    anyhow!(
        "file access on {} stalled; a macOS privacy prompt may be waiting for an answer",
        stall.volume.display()
    )
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
