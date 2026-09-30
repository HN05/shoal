//! A stopped wrapper waits for healthy load or finishes normally when recovery
//! is cancelled. Failed ownership checks never authorize a replacement process.
use anyhow::{Context, Result, ensure};
use tokio::sync::{mpsc, watch};

use super::workspace::Manager;
use crate::{model::PortReservation, protocol::ExecutionEvent};

pub(super) enum Action {
    Resume(Vec<PortReservation>),
    Stop,
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
        return Ok(Action::Stop);
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
        changed = stop.changed() => { changed?; return Ok(Action::Stop); }
        ready = manager.await_overload_recovery(workspace_id) => ready,
    };
    let resume = async {
        ready?;
        prepare_resume(manager, execution_id, workspace_id).await
    }
    .await;
    match resume {
        Ok(action) => Ok(action),
        Err(error) => {
            eprintln!("agent recovery cancelled: {error:#}");
            Ok(Action::Stop)
        }
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
        Ok(Action::Stop)
    }
}
