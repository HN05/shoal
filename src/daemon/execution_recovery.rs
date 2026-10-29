//! A stopped wrapper waits until its processes are proven stopped and the
//! agent queue admits its restore, or only for the proof when `shoal restart`
//! stopped it, and finishes normally when recovery is cancelled. Failed
//! ownership checks never authorize a replacement process.
use anyhow::{Context, Result, ensure};
use std::time::Duration;
use tokio::sync::{OwnedRwLockReadGuard, mpsc, watch};

use super::workspace::{Grant, Manager, Pause, StopRequest};
use crate::daemon::log;
use crate::{
    execution::recovery,
    model::PortReservation,
    protocol::{Control, ExecutionEvent, timing},
};

pub(super) enum Action {
    Resume(Vec<PortReservation>),
    Stop(Cancel),
    Finished(i32),
}

/// A cancelled restore, as the wrapper reports it.
pub(super) struct Cancel {
    /// The reason the wrapper shows, if there is one.
    pub reason: Option<String>,
    /// Removal cancelled it, and the removal record keeps the agent.
    pub removed: bool,
}

impl Cancel {
    fn because(reason: impl Into<String>) -> Self {
        Self {
            reason: Some(reason.into()),
            removed: false,
        }
    }

    /// The cancellation of a stop request, such as `shoal stop` or removal.
    fn requested(request: Option<StopRequest>) -> Self {
        Self {
            removed: request
                .as_ref()
                .is_some_and(StopRequest::keeps_removed_agents),
            reason: request.map(|request| request.reason),
        }
    }

    /// The control that tells the waiting wrapper.
    pub(super) fn control(self) -> Control {
        Control::Stop {
            reason: self.reason,
            removed: self.removed,
        }
    }
}

/// `hold` is the operation guard that holds back a daemon update while the
/// wrapper waits, which a wait for an update lets go of.
pub(super) async fn pause(
    manager: &Manager,
    execution_id: &str,
    workspace_id: &str,
    incoming: &mut mpsc::Receiver<Result<ExecutionEvent>>,
    stop: &mut watch::Receiver<bool>,
    recover: bool,
    hold: &mut Option<OwnedRwLockReadGuard<()>>,
) -> Result<Action> {
    ensure!(recover, "execution is not eligible for overload recovery");
    if manager.restart_requested(execution_id).await {
        return restart(manager, execution_id, workspace_id, incoming, stop).await;
    }
    let Some(protection) = manager.agent_overload(execution_id).await else {
        return Ok(Action::Stop(requested_cancel(manager, execution_id).await));
    };
    // The user sent the agent to the queue, so the overload policy does not
    // apply to its restore.
    let moved = protection.is_queue_move();
    // Stop survivors before anything can cancel the restore, so every outcome
    // leaves only processes Shoal cannot stop. The wrapper finishes on each,
    // and the daemon's completion scan decides whether they leave it unknown.
    let stopped = match manager
        .pause_agent_execution(execution_id, workspace_id, timing::PROCESS_PROOF_TIMEOUT)
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
        () = recovery_disabled(manager), if !moved => {
            return Ok(Action::Stop(Cancel::because("automatic recovery was disabled")));
        }
        ready = async {
            await_stopped(manager, execution_id, workspace_id, stopped).await?;
            // Every process the agent left has stopped writing.
            manager.checkpoint_after_agent(workspace_id).await;
            admit_restore(manager, execution_id, workspace_id, moved, hold).await
        } => ready,
    };
    let resume = async {
        let grant = ready?;
        ensure!(
            moved || manager.config().overload.recovery.enabled,
            "overload recovery was disabled while the agent waited"
        );
        prepare_resume(manager, execution_id, workspace_id, grant).await
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
    if let Ok(Action::Stop(cancel)) = &action {
        let reason = cancel
            .reason
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
        .pause_agent_execution(execution_id, workspace_id, timing::PROCESS_PROOF_TIMEOUT)
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
        resume(manager, execution_id, workspace_id, None).await
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
            Ok(Action::Stop(requested_cancel(manager, execution_id).await))
        }
    }
}

/// Wait until the agent queue admits the restore. A wait for an update, or
/// for its turn in the queue after `moved`, may last indefinitely, so it lets
/// a daemon update through, which hands the wait to the next daemon as other
/// shutdowns do, and takes `hold` back before the agent resumes.
async fn admit_restore(
    manager: &Manager,
    execution_id: &str,
    workspace_id: &str,
    moved: bool,
    hold: &mut Option<OwnedRwLockReadGuard<()>>,
) -> Result<Grant> {
    if !moved && !manager.restore_waits_for_update(workspace_id, execution_id) {
        return manager
            .admit_agent_restore(execution_id, workspace_id)
            .await;
    }
    hold.take();
    let grant = manager
        .admit_agent_restore(execution_id, workspace_id)
        .await?;
    *hold = Some(manager.gates.owned_operation().await);
    Ok(grant)
}

fn restore_failed(error: anyhow::Error) -> Action {
    log!("agent recovery cancelled: {error:#}");
    Action::Stop(Cancel::because(format!(
        "automatic restore failed: {error:#}"
    )))
}

/// Whether `shoal done` finished the workspace's assignment, which leaves its
/// stopped agent nothing to do.
async fn assignment_done(manager: &Manager, workspace_id: &str) -> Result<bool> {
    Ok(manager.completion(workspace_id).await?.is_some())
}

fn done_cancels_restore() -> Action {
    Action::Stop(Cancel::because("the assignment is done"))
}

/// How often a waiting restore checks again for survivors it could not stop.
const SURVIVOR_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Wait until none of the stopped agent's processes survive, since a restore
/// must not start beside them, telling the user once what it waits for and a
/// restart's requests what it waits for now.
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
        match manager
            .pause_agent_execution(execution_id, workspace_id, Duration::ZERO)
            .await?
        {
            Pause::Paused => return Ok(()),
            Pause::Unproven(issue) => manager.restart_waits(execution_id, &issue).await,
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
        .pause_agent_execution(execution_id, workspace_id, timing::PROCESS_PROOF_TIMEOUT)
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

/// How a stop, such as `shoal stop`, cancelled the restore.
async fn requested_cancel(manager: &Manager, execution_id: &str) -> Cancel {
    Cancel::requested(manager.executions.stop_request(execution_id).await)
}

/// Resolves once a reload disables overload recovery, so a waiting agent
/// finishes with its restore record instead of waiting for load to recover.
async fn recovery_disabled(manager: &Manager) {
    let mut config = manager.config_state.watch();
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
    grant: Grant,
) -> Result<Action> {
    if assignment_done(manager, workspace_id).await? {
        return Ok(done_cancels_restore());
    }
    resume(manager, execution_id, workspace_id, Some(grant)).await
}

/// Resume the stopped agent with the workspace's ports, unless a stop
/// cancelled its restore. `grant` is what admitted an automatic restore.
async fn resume(
    manager: &Manager,
    execution_id: &str,
    workspace_id: &str,
    grant: Option<Grant>,
) -> Result<Action> {
    let ports = manager.list_ports(Some(workspace_id)).await?;
    if manager.resume_agent_execution(execution_id, grant).await? {
        Ok(Action::Resume(ports))
    } else {
        Ok(Action::Stop(requested_cancel(manager, execution_id).await))
    }
}
