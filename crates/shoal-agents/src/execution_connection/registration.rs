//! Registering a wrapper's execution: the checks and reservations before its
//! command may run, while keepalives tell the wrapper that the daemon is
//! still at work, and forgetting a registration its wrapper never received.
use std::{future::Future, sync::Arc};

use anyhow::Result;
use tokio::{
    net::UnixStream,
    sync::{OwnedMutexGuard, watch},
    time::Instant,
};

use super::RecordChange;
use super::{Connected, ExecutionContext};
use crate::Agents;
use crate::daemon::Daemon;
use crate::executions::ExecutionKind;
use crate::executions::ExecutionRequest;
use crate::executions::StartedExecution;
use crate::log;
use crate::model::ExecutionPlan;
use crate::places::ExecutionPlace;
use crate::protocol;
use crate::protocol::Body;
use crate::protocol::ErrorCode;
use crate::protocol::Response;
use crate::protocol::timing;
use shoal_protocol::approvals::Undecided;

/// An execution the daemon registered, with the plan its wrapper runs.
pub struct Registered {
    pub execution: Connected,
    pub plan: ExecutionPlan,
    /// Kept until the execution ends.
    pub git_guard: Option<OwnedMutexGuard<()>>,
}

/// Run `work`, sending the wrapper of request `request_id` a keepalive each
/// [`timing::REGISTRATION_KEEPALIVE`] until it ends. Returns what `work`
/// returned and whether the wrapper still listened to the last keepalive.
pub async fn keeping_alive<T>(
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
pub async fn register(
    agents: &Arc<Agents>,
    daemon: &dyn Daemon,
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
    let target = daemon.space(&space).await.map_err(refused)?;
    // A command that cannot reattach would stop with the daemon, so an exit
    // that waits for such commands holds new ones back, except the setup or
    // merge of an operation it already waits for.
    if !reattach
        && !agents.operations.runs_in(&target.id)
        && let Err(exiting) = agents.gates.admit()
    {
        return Err(Box::new(Body::error(exiting.code(), &exiting)));
    }
    let tracks_agent = kind == ExecutionKind::Command && agent.is_some();
    let giver = caller.as_ref();
    if let Some(agent) = &agent {
        (daemon.ensure_agent_held(giver, &space, &agent.mode, &agent.narrowing))
            .await
            .map_err(refused)?;
    }
    // An agent may meet a conversion in progress, of either kind of space.
    let admitted = match tracks_agent {
        true => Some(agents.admit_agent(daemon, &space).await.map_err(refused)?),
        false => None,
    };
    let takes_entry = tracks_agent.then_some(queue_entry.as_ref()).flatten();
    // An agent takes its launch's reservation of the workspace's one agent,
    // and one without a launch must find no launch holding it.
    let reserved = match (takes_entry, tracks_agent) {
        (Some(entry), _) => Some(agents.reserve_queue_entry(entry).await.map_err(refused)?),
        (None, true) => Some(
            agents
                .reserve_worker(daemon, &space)
                .await
                .map_err(refused)?,
        ),
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
    let begun = agents
        .begin_execution_placed(
            daemon,
            &space,
            Some(wrapper),
            kind,
            caller.as_ref(),
            request,
        )
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
                    daemon.notify_approval(&request).await;
                    Box::new(Body::ApprovalRequest(request))
                }
                Err(error) => refused(error),
            });
        }
    };
    if !reattach {
        (agents.executions).mark_unattachable(&plan.id, &plan.space.id);
    }
    let mut source = None;
    if kind == ExecutionKind::Command
        && let Some(agent) = &agent
    {
        agents
            .track_agent(
                daemon,
                &plan.id,
                agent,
                &space.id,
                &space.name,
                recover,
                std::time::Duration::ZERO,
            )
            .await;
        if let Some(entry) = takes_entry {
            agents.continue_restart(entry, &plan.id).await;
            source = agents.consume_queue_entry(entry, &space.id).await;
        }
    }
    // Released before the restart waits for the space's PR gate, which
    // completion holds while it waits for queue admission.
    drop(reserved);
    drop(admitted);
    if kind == ExecutionKind::Command && agent.as_ref().is_some_and(|agent| agent.new_session) {
        restart_watches(daemon, &space, stop.clone()).await;
    }
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
        takes_narrowing: true,
        // Its child's start says whether it does.
        reports_status: false,
        stopped: false,
        restore_cancelled: None,
        stop,
        records: RecordChange::of(&plan),
        source,
        memory: Default::default(),
    };
    Ok(Registered {
        execution,
        plan,
        git_guard: _git_guard,
    })
}

/// Restart the watches of `space`, whose agent begins a new session, before
/// the agent runs: what its items held by then is history, which its first
/// message points it to. A `stop` meanwhile ends the restart, so the
/// connection relays it; the agent it stops needs no watches.
async fn restart_watches(
    daemon: &dyn Daemon,
    space: &crate::model::Space,
    stop: watch::Receiver<bool>,
) {
    if let Err(error) = daemon.restart_watches(&space.id, stop).await {
        log!("{}'s watches not restarted: {error:#}", space.name);
    }
}

/// Whether the daemon forgets `execution` when its wrapper never received
/// the registration, rather than recording a disconnected wrapper. Setup,
/// merge and unmerge run nothing before the wrapper has their plan; a
/// command may track an agent or a demo, whose disconnect has its own path.
pub fn cancels_undelivered(execution: &Connected) -> bool {
    execution.kind != ExecutionKind::Command
}

/// Forget the registered `execution`, whose wrapper left before it received
/// its plan, so its command never ran.
pub async fn cancel(agents: &Agents, execution: Connected) -> Result<()> {
    log!(
        "{} execution {} in {} was not delivered: its wrapper left before the daemon registered it",
        execution.kind,
        execution.id,
        &execution.space.name
    );
    agents.cancel_execution(&execution.id, execution.kind).await
}
