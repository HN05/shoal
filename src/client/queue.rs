//! The agent queue: admitting launches under `max_agents`, the entries
//! waiting, and the hold that keeps a starting entry's slot.
use std::{collections::BTreeMap, time::Duration};

use anyhow::Result;
use tokio::{io::AsyncReadExt as _, sync::oneshot};

use crate::{
    client::{
        connection::{Daemon, open, request, stream_closed},
        paths::Paths,
    },
    model::{MovedEntry, QueueAdmission, QueueEntry, QueuePlace, QueueRequest, QueuedPlan},
    protocol::QueueMethod,
};

/// Reconnect a hold this soon after the daemon restarts.
const HOLD_RETRY: Duration = Duration::from_secs(1);

/// Admit `request` now or queue it.
pub async fn add(daemon: &impl Daemon, request: QueueRequest) -> Result<QueueAdmission> {
    self::request(daemon, QueueMethod::QueueAdd { request }).await
}

/// Every queued entry.
pub async fn entries(daemon: &impl Daemon) -> Result<Vec<QueueEntry>> {
    request(daemon, QueueMethod::ListQueue).await
}

/// The launch plan queued with `entry`.
pub async fn plan(daemon: &impl Daemon, entry: String) -> Result<QueuedPlan> {
    request(daemon, QueueMethod::QueuedPlan { entry }).await
}

/// Start `entry` now, whatever waits before it.
pub async fn start(daemon: &impl Daemon, entry: String) -> Result<QueueEntry> {
    request(daemon, QueueMethod::QueueStart { entry }).await
}

/// Take `entry` out of the queue.
pub async fn remove(daemon: &impl Daemon, entry: String) -> Result<QueueEntry> {
    request(daemon, QueueMethod::QueueRemove { entry }).await
}

/// Move `entry` to `place` in the queue.
pub async fn move_entry(
    daemon: &impl Daemon,
    entry: String,
    place: QueuePlace,
) -> Result<MovedEntry> {
    request(daemon, QueueMethod::QueueMove { entry, place }).await
}

/// Record `workspace` as the one the starting `entry` opened.
pub async fn attach(daemon: &impl Daemon, entry: String, workspace: String) -> Result<()> {
    request(daemon, QueueMethod::QueueAttach { entry, workspace }).await
}

/// Free the slot of the starting `entry`, whose launch failed with `error`;
/// the daemon tells the user.
pub async fn fail(daemon: &impl Daemon, entry: String, error: String) -> Result<()> {
    request(daemon, QueueMethod::QueueFail { entry, error }).await
}

/// Queue the restore of `execution`'s agent in `workspace`, which starts
/// with `env`.
pub async fn restore(
    daemon: &impl Daemon,
    workspace: String,
    execution: String,
    env: BTreeMap<String, String>,
) -> Result<QueueEntry> {
    let method = QueueMethod::QueueRestore {
        workspace,
        execution,
        env,
    };
    request(daemon, method).await
}

/// Keeps a starting entry's slot while it lives, until its agent registers.
/// It reconnects after a daemon restart.
pub struct Hold(tokio::task::JoinHandle<()>);

impl Hold {
    /// Returns once the daemon holds the slot, ends the entry or cannot be
    /// reached. A daemon at its connection limit closes the hold unanswered,
    /// so preparation waits instead of outliving the reservation.
    pub async fn start(paths: &Paths, entry: &str) -> Self {
        let (settled, held) = oneshot::channel();
        let hold = Self(tokio::spawn(keep(paths.clone(), entry.to_owned(), settled)));
        let _ = held.await;
        hold
    }
}

async fn keep(paths: Paths, entry: String, settled: oneshot::Sender<()>) {
    let mut settled = Some(settled);
    loop {
        let method = QueueMethod::QueueHold {
            entry: entry.clone(),
        };
        let opened = open(&paths, method).await;
        if !matches!(&opened, Err(error) if stream_closed(error))
            && let Some(settled) = settled.take()
        {
            let _ = settled.send(());
        }
        if let Ok((mut stream, body)) = opened {
            // A refusal means the agent took the slot over or the entry ended.
            if body.into_result().is_err() {
                return;
            }
            let mut closed = [0u8; 1];
            let _ = stream.read(&mut closed).await;
        }
        tokio::time::sleep(HOLD_RETRY).await;
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests;
