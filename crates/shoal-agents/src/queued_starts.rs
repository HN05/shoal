//! Starts queued agents and restores as slots free, and the waiting
//! preparations of new workspaces as what they wait for allows, and keeps
//! starting entries' slots while their launch runs.
use shoal_daemon_base::operations::Operations;
use std::{sync::Arc, time::Duration};

use anyhow::Result;
use tokio::{io::AsyncReadExt as _, net::UnixStream};

use crate::Agents;
use crate::daemon::Daemon;
use crate::log;
use crate::paths::Paths;
use crate::protocol;
use crate::protocol::Body;
use crate::protocol::ErrorCode;
use crate::protocol::Response;
use crate::queue::Start;

/// Bounds how long a freed slot can wait when nothing wakes the dispatcher,
/// such as a stopped agent's forgotten record.
const SWEEP_INTERVAL: Duration = Duration::from_secs(15);
/// Lets a burst of state changes settle into one pass. A unit test, which
/// waits for each pass it starts, lets one settle only briefly.
const SETTLE: Duration = if cfg!(test) {
    Duration::from_millis(10)
} else {
    Duration::from_millis(500)
};

pub async fn run(agents: Arc<Agents>, daemon: Arc<dyn Daemon>) {
    if let Err(error) = agents.refresh_starting_entries().await {
        log!("agent queue: {error:#}");
    }
    let mut changed = agents.store.watch_changes();
    let mut config = agents.config.watch();
    // Restores wait for sustained healthy readings.
    let mut recovered = agents.overload_recovery.watch();
    loop {
        {
            let _operation = agents.gates.operation().await;
            if let Err(error) = agents.dispatch_queue(&*daemon).await {
                log!("agent queue: {error:#}");
            }
            // Under the guard, so an update cannot quiesce between a
            // preparation's release and its operation's start.
            if let Err(error) = daemon.clone().start_waiting_preparations().await {
                log!("waiting preparations: {error:#}");
            }
        }
        tokio::select! {
            _ = changed.changed() => {}
            _ = config.changed() => {}
            _ = recovered.changed() => {}
            _ = agents.agent_queue.woken() => {}
            _ = tokio::time::sleep(SWEEP_INTERVAL) => {}
        }
        tokio::time::sleep(SETTLE).await;
        changed.borrow_and_update();
        config.borrow_and_update();
        recovered.borrow_and_update();
    }
}

/// Refuse a start the queue's worker would open in a background place
/// under `settings`, for a caller whose place `env` records, when no host
/// can take it there; a test daemon's faked workers open none.
pub fn ensure_background_start(
    daemon: &dyn Daemon,
    settings: &crate::config::Effective,
    env: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    if daemon.fakes_workers() {
        return Ok(());
    }
    shoal_core::terminal::queued::ensure_queued_background(settings, env)
}

/// Run the queue start worker for `start`, detached from the daemon,
/// with the multiplexer variables of the terminal that queued it.
pub fn spawn_start(operations: &Operations, paths: &Paths, start: &Start) -> Result<()> {
    use crate::workers::{Worker, detached_command, internal_command};
    use anyhow::Context as _;
    use std::process::Stdio;

    // Only a test daemon fakes them, which a build without its support
    // never runs.
    if operations.fakes() {
        #[cfg(any(test, feature = "test-support"))]
        return fake::start(start);
    }

    let argv = internal_command(
        paths,
        false,
        Worker::QueueStart {
            entry: start.id.as_str(),
        },
    )?;
    let mut child = detached_command(&argv)
        .envs(&start.env)
        .current_dir(&paths.home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("start the queued agent")?;
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(())
}

/// Keep `entry`'s slot, or a waiting entry out of admission, until the
/// client hangs up or the entry leaves the queue, as when an agent takes it
/// over.
pub async fn hold(
    mut stream: UnixStream,
    request_id: u64,
    agents: Arc<Agents>,
    entry: shoal_ids::queue_entry_ids::QueueEntryId,
    startup: tokio::sync::OwnedRwLockReadGuard<()>,
) -> Result<()> {
    let mut changed = agents.store.watch_changes();
    if let Err(error) = agents.hold_queue_entry(&entry).await {
        let body = Body::error(ErrorCode::OperationFailed, format!("{error:#}"));
        return protocol::write(&mut stream, &Response::new(request_id, body)).await;
    }
    let held = async {
        protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
        drop(startup);
        let (mut reader, _writer) = stream.split();
        let mut closed = [0u8; 1];
        loop {
            tokio::select! {
                result = changed.changed() => result?,
                // The client never writes; a read completes only when it hangs up.
                _ = reader.read(&mut closed) => return Ok(()),
            }
            if !agents.queue_entry_present(&entry).await? {
                return Ok(());
            }
        }
    }
    .await;
    agents.release_queue_hold(&entry).await?;
    held
}

#[cfg(any(test, feature = "test-support"))]
pub mod fake;
