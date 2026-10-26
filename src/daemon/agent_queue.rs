//! Starts queued agents and restores as slots free and keeps starting
//! entries' slots while their launch runs.
use std::{sync::Arc, time::Duration};

use anyhow::Result;
use tokio::{io::AsyncReadExt as _, net::UnixStream};

use crate::{
    daemon::{log, workspace::Manager, workspace::agent_queue::Start},
    paths::Paths,
    protocol::{self, Body, ErrorCode, Response},
};

/// Bounds how long a freed slot can wait when nothing wakes the dispatcher,
/// such as a stopped agent's discarded record.
const SWEEP_INTERVAL: Duration = Duration::from_secs(15);
/// Lets a burst of state changes settle into one pass.
const SETTLE: Duration = Duration::from_millis(500);

pub(super) async fn run(manager: Arc<Manager>) {
    if let Err(error) = manager.refresh_starting_entries().await {
        log!("agent queue: {error:#}");
    }
    let mut changed = manager.store.watch_changes();
    let mut config = manager.watch_config();
    // Restores wait for sustained healthy readings.
    let mut recovered = manager.recovery_ready.subscribe();
    loop {
        {
            let _operation = manager.background_operations.read().await;
            if let Err(error) = manager.dispatch_queue().await {
                log!("agent queue: {error:#}");
            }
        }
        tokio::select! {
            _ = changed.changed() => {}
            _ = config.changed() => {}
            _ = recovered.changed() => {}
            _ = manager.agent_queue_wake.notified() => {}
            _ = tokio::time::sleep(SWEEP_INTERVAL) => {}
        }
        tokio::time::sleep(SETTLE).await;
        changed.borrow_and_update();
        config.borrow_and_update();
        recovered.borrow_and_update();
    }
}

/// Run the CLI's queue start worker for `start`, detached from the daemon,
/// with the multiplexer variables of the terminal that queued it.
#[cfg(not(test))]
pub(crate) fn spawn_start(paths: &Paths, start: &Start) -> Result<()> {
    use crate::cli::internal::{Worker, internal_command};
    use anyhow::Context as _;
    use std::process::Stdio;

    let argv = internal_command(paths, false, Worker::QueueStart { entry: &start.id })?;
    let mut command = tokio::process::Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .envs(&start.env)
        .current_dir(&paths.home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid detaches the forked child without allocating, before exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().context("start the queued agent")?;
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(())
}

/// Tests record starts instead of running this binary as the CLI.
#[cfg(test)]
pub(crate) fn spawn_start(_paths: &Paths, start: &Start) -> Result<()> {
    tests::STARTED
        .lock()
        .unwrap()
        .push((start.id.clone(), start.env.clone()));
    Ok(())
}

/// Keep `entry`'s slot until the client hangs up or an agent takes it over.
pub(super) async fn hold(
    mut stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
    entry: String,
    startup: tokio::sync::OwnedRwLockReadGuard<()>,
) -> Result<()> {
    let mut changed = manager.store.watch_changes();
    if let Err(error) = manager.hold_queue_entry(&entry).await {
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
            if !manager.queue_entry_starting(&entry).await? {
                return Ok(());
            }
        }
    }
    .await;
    manager.release_queue_hold(&entry).await?;
    held
}

#[cfg(test)]
pub(crate) mod tests;
