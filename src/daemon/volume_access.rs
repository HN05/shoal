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
const LIMIT: Duration = Duration::from_secs(30);

/// A stalled volume is checked again this long after its last check, so
/// each cleanup sweep rechecks it once while its other checks skip it.
pub(crate) const RECHECK_AFTER: Duration = Duration::from_secs(20);

/// What a process reports when the kernel interrupts its wait on the volume.
const INTERRUPTED: &str = "Interrupted system call";

/// The stalled volumes.
#[derive(Default)]
pub(crate) struct VolumeAccess {
    stalls: Mutex<BTreeMap<PathBuf, Stalled>>,
}

struct Stalled {
    report: VolumeStall,
    checked: Instant,
}

impl VolumeAccess {
    /// Run `check` of `path` within [`LIMIT`], unless its volume stalled
    /// within [`RECHECK_AFTER`]. A check that runs out or is interrupted
    /// fails with the stall; any other outcome shows the volume answers. The
    /// stall comes back too when the volume was not stalled before.
    async fn check<T>(
        &self,
        path: &Path,
        check: impl Future<Output = Result<T>>,
    ) -> (Result<T>, Option<VolumeStall>) {
        let volume = volume(path);
        if let Some(stall) = self.recently_stalled(&volume) {
            return (Err(stalled(&stall)), None);
        }
        let outcome = tokio::time::timeout(LIMIT, check).await;
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

    /// Every stalled volume.
    pub(crate) fn stalls(&self) -> Vec<VolumeStall> {
        lock(&self.stalls)
            .values()
            .map(|stalled| stalled.report.clone())
            .collect()
    }

    fn recently_stalled(&self, volume: &Path) -> Option<VolumeStall> {
        lock(&self.stalls)
            .get(volume)
            .filter(|stalled| stalled.checked.elapsed() < RECHECK_AFTER)
            .map(|stalled| stalled.report.clone())
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
