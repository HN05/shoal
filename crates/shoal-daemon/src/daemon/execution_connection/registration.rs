//! Registering a wrapper's execution: the checks and reservations before its
//! command may run, while keepalives tell the wrapper that the daemon is
//! still at work, and forgetting a registration its wrapper never received.
use std::{future::Future, sync::Arc};

use anyhow::Result;
use tokio::{net::UnixStream, sync::OwnedMutexGuard, time::Instant};

use super::{Connected, ExecutionContext};
use crate::{
    daemon::{
        approvals::Undecided,
        log,
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
        space,
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
    let target = manager.space(&space).await.map_err(refused)?;
    // A command that cannot reattach would stop with the daemon, so an exit
    // that waits for such commands holds new ones back, except the setup or
    // merge of an operation it already waits for.
    if !reattach
        && !manager.operation_runs_in(target.id())
        && let Err(exiting) = manager.gates.admit()
    {
        return Err(Box::new(Body::error(exiting.code(), &exiting)));
    }
    let tracks_agent = kind == ExecutionKind::Command && agent.is_some();
    if let (true, Some(workspace)) = (tracks_agent, target.worktree()) {
        Manager::ensure_holds_agents(workspace).map_err(refused)?;
    }
    let giver = caller.as_ref().map(|caller| &caller.space_id);
    if let Some(agent) = &agent {
        (manager.ensure_agent_held(giver, &space, &agent.mode, &agent.narrowing))
            .await
            .map_err(refused)?;
    }
    // The user attends a chat, whose agent starts at once.
    let admitted = match tracks_agent && !target.space_kind().attended() {
        true => Some(manager.admit_agent(&space).await.map_err(refused)?),
        false => None,
    };
    let takes_entry = tracks_agent.then_some(queue_entry.as_ref()).flatten();
    // An agent takes its launch's reservation of the workspace's one agent,
    // and one without a launch must find no launch holding it.
    let reserved = match (takes_entry, tracks_agent) {
        (Some(entry), _) => Some(manager.reserve_queue_entry(entry).await.map_err(refused)?),
        (None, true) => Some(manager.reserve_worker(&space).await.map_err(refused)?),
        (None, false) => None,
    };
    let request = ExecutionRequest {
        demo,
        place: origin.map(|origin| ExecutionPlace {
            origin,
            agent: agent.is_some(),
        }),
        merge,
        agent: tracks_agent,
    };
    let begun = manager
        .begin_execution_placed(&space, Some(wrapper), kind, caller.as_ref(), request)
        .await;
    let StartedExecution {
        plan,
        space,
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
        (manager.executions).mark_unattachable(&plan.id, &plan.space.id);
    }
    if kind == ExecutionKind::Command
        && let Some(agent) = &agent
    {
        manager
            .track_agent(
                &plan.id,
                agent,
                space.id(),
                space.name(),
                recover,
                std::time::Duration::ZERO,
            )
            .await;
        if let Some(entry) = takes_entry {
            manager.continue_restart(entry, &plan.id).await;
            manager.consume_queue_entry(entry, space.id()).await;
        }
    }
    drop(reserved);
    drop(admitted);
    let execution = Connected {
        id: plan.id.clone(),
        kind,
        space,
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

/// Whether the daemon forgets `execution` when its wrapper never received
/// the registration, rather than recording a disconnected wrapper. Setup,
/// merge and unmerge run nothing before the wrapper has their plan; a
/// command may track an agent or a demo, whose disconnect has its own path.
pub(super) fn cancels_undelivered(execution: &Connected) -> bool {
    execution.kind != ExecutionKind::Command
}

/// Forget the registered `execution`, whose wrapper left before it received
/// its plan, so its command never ran.
pub(super) async fn cancel(manager: &Manager, execution: Connected) -> Result<()> {
    log!(
        "{} execution {} in {} was not delivered: its wrapper left before the daemon registered it",
        execution.kind,
        execution.id,
        execution.space.name()
    );
    manager
        .cancel_execution(&execution.id, execution.kind)
        .await
}

#[cfg(test)]
mod tests;
