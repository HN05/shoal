//! A stopped wrapper waits for healthy load or finishes normally when recovery
//! is cancelled. Failed ownership checks never authorize a replacement process.
use anyhow::{Context, Result, ensure};
use tokio::sync::{mpsc, watch};

use super::workspace::Manager;
use crate::daemon::log;
use crate::{model::PortReservation, protocol::ExecutionEvent};

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
    manager
        .pause_agent_execution(execution_id, workspace_id)
        .await?;
    // Only this successful pause proves termination. Recovery refusals after
    // this point must let the wrapper report completion, rather than disconnect.
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
        ready = manager.await_overload_recovery(workspace_id) => ready,
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
    match resume {
        Ok(action) => Ok(action),
        Err(error) => {
            log!("agent recovery cancelled: {error:#}");
            Ok(Action::Stop(Some(format!(
                "automatic restore failed: {error:#}"
            ))))
        }
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
    let ports = manager.inspect_workspace(workspace_id).await?.ports;
    if manager.resume_agent_execution(execution_id).await? {
        Ok(Action::Resume(ports))
    } else {
        Ok(Action::Stop(requested_reason(manager, execution_id).await))
    }
}
