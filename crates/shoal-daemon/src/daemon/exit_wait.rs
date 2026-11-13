//! Exiting once nothing the exit would cut off runs, as an update's restart
//! and an exit that waits do: work the daemon runs itself, in background
//! operations and in connected commands that cannot reattach, ends with it,
//! while agents and CI jobs reattach to the next daemon.
mod work;

use std::time::Duration;

use anyhow::Result;
use tokio::{net::UnixStream, sync::OwnedRwLockWriteGuard};

use super::{
    answer,
    workspace::{ExitQuiescence, HeldBack, Manager},
};
use crate::protocol::{self, Body, Exit, ExitWhen, Response, Work};

/// How often an exit that waits looks again at the work in progress.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Background operations held off, when none is in progress and no
/// connected execution would stop with the daemon. Admission is paused in
/// the server while checking. Background readers cover requests, hooks and
/// mutations; execution connections hold one only while their execution
/// changes or their wrapper cannot reattach.
pub(super) fn quiesce(manager: &Manager) -> Option<OwnedRwLockWriteGuard<()>> {
    // Checked first too, so a connected execution does not hold operations
    // off for a moment on every attempt while it runs.
    if manager.executions.has_unattachable() {
        return None;
    }
    let guard = manager.gates.try_quiesce()?;
    (!manager.executions.has_unattachable()).then_some(guard)
}

/// A daemon ready to exit: nothing its exit would cut off runs, and nothing
/// new starts until it has exited.
pub(super) struct Quiet {
    quiescence: Option<ExitQuiescence>,
    _held_back: HeldBack,
}

impl Quiet {
    /// Let operations run again, still holding back new work, once the
    /// daemon accepts no more requests.
    pub(super) fn release_operations(&mut self) {
        self.quiescence = None;
    }
}

/// Hold back new work and wait until nothing an exit would cut off runs,
/// sending the client of request `request_id` on `stream` the work that
/// runs each time it changes. `None` once the client hung up, or once work
/// other than requests and background passes runs and `when` refuses to
/// wait for it.
pub(super) async fn wait(
    manager: &Manager,
    stream: &mut UnixStream,
    request_id: u64,
    exit: Exit,
    when: ExitWhen,
) -> Result<Option<Quiet>> {
    let held_back = manager.gates.hold_back(exit);
    let mut reported = None;
    loop {
        if let Some(quiescence) = quiesce(manager) {
            return Ok(Some(Quiet {
                quiescence: Some(manager.gates.keep_for_exit(quiescence)),
                _held_back: held_back,
            }));
        }
        let running = manager.work_an_exit_cuts_off().await?;
        if reported.as_ref() != Some(&running) {
            let response = Response::new(request_id, Body::ExitWaits(running.clone()));
            protocol::write(stream, &response).await?;
            if when == ExitWhen::IfIdle && Work::refuses_idle_exit(&running) {
                return Ok(None);
            }
            reported = Some(running);
        }
        tokio::select! {
            () = tokio::time::sleep(POLL_INTERVAL) => {}
            () = answer::hangup(stream) => return Ok(None),
        }
    }
}

#[cfg(test)]
mod tests;
