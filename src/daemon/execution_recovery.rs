//! A stopped wrapper waits until its processes are proven stopped and the
//! agent queue admits its restore, or finishes normally when recovery is
//! cancelled. Failed ownership checks never authorize a replacement process.
use anyhow::{Context, Result, ensure};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

use super::workspace::{Manager, Pause};
use crate::daemon::log;
use crate::{
    model::PortReservation,
    protocol::{ExecutionEvent, timing},
};

pub(super) enum Action {
    Resume(Vec<PortReservation>),
    /// Cancel the restore, with the reason the wrapper shows.
    Stop(Option<String>),
    Finished(i32),
}

pub(super) async fn pause(
    manager: &Manager,
    execution_id: &str,
    workspace_id: &str,
    incoming: &mut mpsc::Receiver<Result<ExecutionEvent>>,
    stop: &mut watch::Receiver<bool>,
    recover: bool,
) -> Result<Action> {
    ensure!(recover, "execution is not eligible for overload recovery");
    if !manager.agent_was_overloaded(execution_id).await {
        return Ok(Action::Stop(requested_reason(manager, execution_id).await));
    }
    // Stop survivors before anything can cancel the restore, so every outcome
    // leaves only processes Shoal cannot stop. The wrapper finishes on each,
    // and the daemon's completion scan decides whether they leave it unknown.
    let stopped = match manager
        .pause_agent_execution(execution_id, workspace_id, timing::WORKSPACE_STOP_TIMEOUT)
        .await
    {
        Ok(stopped) => stopped,
        Err(error) => return Ok(restore_failed(error)),
    };
    // Take a place in the agent queue as the agent stops, so restores and
    // launches start in the order they arrived.
    if let Err(error) = manager.queue_agent_restore(execution_id).await {
        return Ok(restore_failed(error));
    }
    let ready = tokio::select! {
        biased;
        event = incoming.recv() => match event.context("execution disconnected")?? {
            ExecutionEvent::Finished { exit_code } => return Ok(Action::Finished(exit_code)),
            _ => return Err(anyhow::anyhow!("unexpected event during overload recovery")),
        },
        changed = stop.changed() => {
            changed?;
            return Ok(Action::Stop(requested_reason(manager, execution_id).await));
        }
        () = recovery_disabled(manager) => {
            return Ok(Action::Stop(Some("automatic recovery was disabled".into())));
        }
        ready = async {
            await_stopped(manager, execution_id, workspace_id, stopped).await?;
            manager.await_restore_admission(execution_id).await
        } => ready,
    };
    let resume = async {
        ready?;
        ensure!(
            manager.config().overload.recovery.enabled,
            "overload recovery was disabled while the agent waited"
        );
        prepare_resume(manager, execution_id, workspace_id).await
    }
    .await;
    Ok(resume.unwrap_or_else(restore_failed))
}

fn restore_failed(error: anyhow::Error) -> Action {
    log!("agent recovery cancelled: {error:#}");
    Action::Stop(Some(format!("automatic restore failed: {error:#}")))
}

/// How often a waiting restore checks again for survivors it could not stop.
const SURVIVOR_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Wait until none of the stopped agent's processes survive, since a restore
/// must not start beside them, and tell the user once what it waits for.
async fn await_stopped(
    manager: &Manager,
    execution_id: &str,
    workspace_id: &str,
    stopped: Pause,
) -> Result<()> {
    let Pause::Unproven(issue) = stopped else {
        return Ok(());
    };
    manager.notify_restore_waits(execution_id, &issue).await;
    loop {
        tokio::time::sleep(SURVIVOR_POLL_INTERVAL).await;
        if let Pause::Paused = manager
            .pause_agent_execution(execution_id, workspace_id, Duration::ZERO)
            .await?
        {
            return Ok(());
        }
    }
}

/// A protection stop without automatic recovery still ends what the agent left
/// running outside its process group, so `shoal resume` can restore it.
pub(super) async fn stop_survivors(manager: &Manager, execution_id: &str, workspace_id: &str) {
    if !manager.agent_was_overloaded(execution_id).await {
        return;
    }
    match manager
        .pause_agent_execution(execution_id, workspace_id, timing::WORKSPACE_STOP_TIMEOUT)
        .await
    {
        Ok(Pause::Paused) => {}
        Ok(Pause::Unproven(issue)) => {
            log!("the stopped agent's processes are not proven stopped: {issue}")
        }
        Err(error) => log!("stopped agent's processes not checked: {error:#}"),
    }
}

/// Why a stop cancelled the restore, such as `shoal stop`.
async fn requested_reason(manager: &Manager, execution_id: &str) -> Option<String> {
    manager
        .stop_request(execution_id)
        .await
        .map(|request| request.reason)
}

/// Resolves once a reload disables overload recovery, so a waiting agent
/// finishes with its restore record instead of waiting for load to recover.
async fn recovery_disabled(manager: &Manager) {
    let mut config = manager.watch_config();
    if config
        .wait_for(|config| !config.overload.recovery.enabled)
        .await
        .is_err()
    {
        std::future::pending::<()>().await;
    }
}

async fn prepare_resume(
    manager: &Manager,
    execution_id: &str,
    workspace_id: &str,
) -> Result<Action> {
    manager.verify_restore_workspace(workspace_id).await?;
    let ports = manager.inspect_workspace(workspace_id).await?.ports;
    if manager.resume_agent_execution(execution_id).await? {
        Ok(Action::Resume(ports))
    } else {
        Ok(Action::Stop(requested_reason(manager, execution_id).await))
    }
}
