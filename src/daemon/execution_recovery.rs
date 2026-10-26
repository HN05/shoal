//! A stopped wrapper waits until its processes are proven stopped and the
//! agent queue admits its restore, or only for the proof when `shoal restart`
//! stopped it, and finishes normally when recovery is cancelled. Failed
//! ownership checks never authorize a replacement process.
use anyhow::{Context, Result, ensure};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

use super::workspace::{Manager, Pause};
use crate::daemon::log;
use crate::{
    execution::recovery,
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
    if manager.restart_requested(execution_id).await {
        return restart(manager, execution_id, workspace_id, incoming, stop).await;
    }
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
    match assignment_done(manager, workspace_id).await {
        Ok(false) => {}
        Ok(true) => return Ok(done_cancels_restore()),
        Err(error) => return Ok(restore_failed(error)),
    }
    // Take a place in the agent queue as the agent stops, so restores and
    // launches start in the order they arrived.
    if let Err(error) = manager.queue_agent_restore(execution_id).await {
        return Ok(restore_failed(error));
    }
    let ready = tokio::select! {
        biased;
        action = interrupted(manager, execution_id, incoming, stop) => return action,
        () = recovery_disabled(manager) => {
            return Ok(Action::Stop(Some("automatic recovery was disabled".into())));
        }
        ready = async {
            await_stopped(manager, execution_id, workspace_id, stopped).await?;
            // Every process the agent left has stopped writing.
            manager.checkpoint_after_agent(workspace_id).await;
            manager.admit_agent_restore(execution_id, workspace_id).await
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

/// Restore an agent `shoal restart` stopped once its processes are proven
/// stopped, answering the restart when it does not resume.
async fn restart(
    manager: &Manager,
    execution_id: &str,
    workspace_id: &str,
    incoming: &mut mpsc::Receiver<Result<ExecutionEvent>>,
    stop: &mut watch::Receiver<bool>,
) -> Result<Action> {
    let action = restart_once_stopped(manager, execution_id, workspace_id, incoming, stop).await;
    if let Ok(Action::Stop(reason)) = &action {
        let reason = reason
            .clone()
            .unwrap_or_else(|| "the restart was cancelled".into());
        manager.fail_restart(execution_id, reason).await;
    }
    action
}

async fn restart_once_stopped(
    manager: &Manager,
    execution_id: &str,
    workspace_id: &str,
    incoming: &mut mpsc::Receiver<Result<ExecutionEvent>>,
    stop: &mut watch::Receiver<bool>,
) -> Result<Action> {
    let stopped = match manager
        .pause_agent_execution(execution_id, workspace_id, timing::WORKSPACE_STOP_TIMEOUT)
        .await
    {
        Ok(stopped) => stopped,
        Err(error) => return Ok(restore_failed(error)),
    };
    let ready = tokio::select! {
        biased;
        action = interrupted(manager, execution_id, incoming, stop) => return action,
        ready = async {
            await_stopped(manager, execution_id, workspace_id, stopped).await?;
            manager.checkpoint_after_agent(workspace_id).await;
            anyhow::Ok(())
        } => ready,
    };
    let resume = async {
        ready?;
        manager.verify_restore_workspace(workspace_id).await?;
        resume(manager, execution_id, workspace_id).await
    }
    .await;
    Ok(resume.unwrap_or_else(restore_failed))
}

/// Resolves when the wrapper finishes or a stop cancels the restore it waits for.
async fn interrupted(
    manager: &Manager,
    execution_id: &str,
    incoming: &mut mpsc::Receiver<Result<ExecutionEvent>>,
    stop: &mut watch::Receiver<bool>,
) -> Result<Action> {
    tokio::select! {
        biased;
        event = incoming.recv() => match event.context("execution disconnected")?? {
            ExecutionEvent::Finished { exit_code } => Ok(Action::Finished(exit_code)),
            _ => Err(anyhow::anyhow!("unexpected event while the agent waits for its restore")),
        },
        changed = stop.changed() => {
            changed?;
            Ok(Action::Stop(requested_reason(manager, execution_id).await))
        }
    }
}

fn restore_failed(error: anyhow::Error) -> Action {
    log!("agent recovery cancelled: {error:#}");
    Action::Stop(Some(format!("automatic restore failed: {error:#}")))
}

/// Whether `shoal done` finished the workspace's assignment, which leaves its
/// stopped agent nothing to do.
async fn assignment_done(manager: &Manager, workspace_id: &str) -> Result<bool> {
    Ok(manager.completion(workspace_id).await?.is_some())
}

fn done_cancels_restore() -> Action {
    Action::Stop(Some("the assignment is done".into()))
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

/// A completed execution no longer needs the restore record its protection
/// stop left; removal is best effort.
pub(super) fn discard_handoff(manager: &Manager, workspace_id: &str, execution_id: &str) {
    let record = recovery::record_path(&manager.paths, workspace_id, execution_id);
    if let Err(error) = recovery::consume(&record) {
        log!("unused overload recovery handoff not removed: {error:#}");
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
    if assignment_done(manager, workspace_id).await? {
        return Ok(done_cancels_restore());
    }
    resume(manager, execution_id, workspace_id).await
}

/// Resume the stopped agent with the workspace's ports, unless a stop
/// cancelled its restore.
async fn resume(manager: &Manager, execution_id: &str, workspace_id: &str) -> Result<Action> {
    let ports = manager.list_ports(Some(workspace_id)).await?;
    if manager.resume_agent_execution(execution_id).await? {
        Ok(Action::Resume(ports))
    } else {
        Ok(Action::Stop(requested_reason(manager, execution_id).await))
    }
}
