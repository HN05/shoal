//! Registering a wrapper's execution: the checks and reservations before its
//! command may run, while keepalives tell the wrapper that the daemon is
//! still at work.
use std::{future::Future, sync::Arc};

use anyhow::Result;
use tokio::{net::UnixStream, sync::OwnedMutexGuard, time::Instant};

use super::{Connected, ExecutionContext};
use crate::{
    daemon::{
        approvals::Undecided,
        workspace::{
            ExecutionKind, ExecutionRequest, Manager, RecordChange, StartedExecution,
            places::ExecutionPlace,
        },
    },
    model::ExecutionPlan,
    protocol::{self, Body, ErrorCode, Response, timing},
};

/// An execution the daemon registered, with the plan its wrapper runs.
pub(super) struct Registered {
    pub execution: Connected,
    pub plan: ExecutionPlan,
    /// Kept until the execution ends.
    pub git_guard: Option<OwnedMutexGuard<()>>,
}

/// Run `work`, sending the wrapper of request `request_id` a keepalive each
/// [`timing::REGISTRATION_KEEPALIVE`] until it ends. Returns what `work`
/// returned and whether the wrapper still listened to the last keepalive.
pub(super) async fn keeping_alive<T>(
    stream: &mut UnixStream,
    request_id: u64,
    work: impl Future<Output = T>,
) -> (T, bool) {
    let interval = timing::REGISTRATION_KEEPALIVE;
    let mut ticks = tokio::time::interval_at(Instant::now() + interval, interval);
    let mut listening = true;
    tokio::pin!(work);
    loop {
        tokio::select! {
            done = &mut work => return (done, listening),
            _ = ticks.tick(), if listening => {
                let keepalive = Response::new(request_id, Body::Keepalive);
                listening = protocol::write(stream, &keepalive).await.is_ok();
            }
        }
    }
}

/// Register the execution `context` asks for, or the body that refuses it.
pub(super) async fn register(
    manager: &Arc<Manager>,
    context: ExecutionContext,
) -> Result<Registered, Box<Body>> {
    let ExecutionContext {
        workspace,
        wrapper,
        kind,
        agent,
        recover,
        reattach,
        queue_entry,
        demo,
        origin,
        merge,
        caller,
    } = context;
    let refused = |error: anyhow::Error| {
        Box::new(Body::error(
            ErrorCode::ExecutionFailed,
            format!("{error:#}"),
        ))
    };
    if kind == ExecutionKind::Command && agent.is_some() {
        let workspace = manager.workspace(&workspace).await.map_err(refused)?;
        Manager::ensure_holds_agents(&workspace).map_err(refused)?;
    }
    let giver = caller.as_ref().map(|caller| &caller.space_id);
    if let Some(agent) = &agent {
        (manager.ensure_workspace_agent_held(giver, &workspace, &agent.mode, &agent.narrowing))
            .await
            .map_err(refused)?;
    }
    let admitted = match kind == ExecutionKind::Command && agent.is_some() {
        true => Some(manager.admit_agent(&workspace).await.map_err(refused)?),
        false => None,
    };
    let takes_entry = (kind == ExecutionKind::Command && agent.is_some())
        .then_some(queue_entry.as_ref())
        .flatten();
    let reserved = match takes_entry {
        Some(entry) => Some(manager.reserve_queue_entry(entry).await.map_err(refused)?),
        None => None,
    };
    let request = ExecutionRequest {
        demo,
        place: origin.map(|origin| ExecutionPlace {
            origin,
            agent: agent.is_some(),
        }),
        merge,
    };
    let begun = manager
        .begin_execution_placed(&workspace, Some(wrapper), kind, caller.as_ref(), request)
        .await;
    let StartedExecution {
        plan,
        stop,
        _git_guard,
    } = match begun {
        Ok(begun) => begun,
        Err(error) => {
            return Err(match error.downcast::<Undecided>() {
                Ok(Undecided(request)) => {
                    manager.notify_approval(&request).await;
                    Box::new(Body::ApprovalRequest(request))
                }
                Err(error) => refused(error),
            });
        }
    };
    if !reattach {
        manager.executions.mark_unattachable(&plan.id);
    }
    if kind == ExecutionKind::Command
        && let Some(agent) = &agent
    {
        manager
            .track_agent(
                &plan.id,
                agent,
                &plan.workspace.id,
                &plan.workspace.name,
                recover,
                std::time::Duration::ZERO,
            )
            .await;
        if let Some(entry) = takes_entry {
            manager.continue_restart(entry, &plan.id).await;
            manager.consume_queue_entry(entry, &plan.workspace.id).await;
        }
    }
    drop(reserved);
    drop(admitted);
    let execution = Connected {
        id: plan.id.clone(),
        kind,
        workspace: plan.workspace.clone(),
        agent,
        recover,
        reattach,
        started: false,
        awaiting_restore: false,
        resuming: false,
        unqueued: takes_entry.is_none(),
        // Registration requires this daemon's protocol.
        hears_restore_waits: true,
        stopped: false,
        stop,
        records: RecordChange::of(&plan),
    };
    Ok(Registered {
        execution,
        plan,
        git_guard: _git_guard,
    })
}

#[cfg(test)]
mod tests;
