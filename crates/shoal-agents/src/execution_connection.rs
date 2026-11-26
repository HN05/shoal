//! Long-lived execution connections: the daemon side of the wrapper protocol.
//! Once [`registration`] registers the execution, the wrapper reports its
//! child and completion; the daemon relays stop and recovery controls and
//! records the outcome.
use crate::control::ProtectionExt;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::executions::TrackedAgent;
use shoal_protocol::launches::LaunchSource;
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use tokio::{
    net::UnixStream,
    sync::{OwnedRwLockReadGuard, mpsc, watch},
    task::JoinSet,
};

use crate::Agents;
use crate::daemon::Daemon;
use crate::execution_recovery;
use crate::executions::Completing;
use crate::executions::ExecutionKind;
use crate::executions::ReattachedExecution;
use crate::executions::StopRequest;
use crate::model::Protection;
use crate::model::Space;
use crate::places::ExecutionPlace;
use crate::process::identity::Identity;
use crate::program_status::Reports;
use crate::protocol;
use crate::protocol::Body;
use crate::protocol::Control;
use crate::protocol::ErrorCode;
use crate::protocol::ExecutionEvent;
use crate::protocol::Reattach;
use crate::protocol::Response;
use crate::protocol::ScopeMemory;
use registration::Registered;
use shoal_daemon_base::callers::Caller;
use shoal_daemon_base::events::AgentEvent;
use shoal_daemon_base::events::EventKind;
use shoal_protocol::space_names::SpaceSelector;

pub struct ExecutionContext {
    pub space: SpaceSelector,
    pub wrapper: Identity,
    pub kind: ExecutionKind,
    pub agent: Option<TrackedAgent>,
    pub recover: bool,
    pub reattach: bool,
    /// The starting queue entry whose slot this agent takes over.
    pub queue_entry: Option<shoal_ids::queue_entry_ids::QueueEntryId>,
    /// The demo the command runs.
    pub demo: Option<shoal_protocol::demos::Launch>,
    /// Where the wrapper runs in a terminal host.
    pub origin: Option<shoal_core::sessions::Origin>,
    /// How a merge runs.
    pub merge: Option<crate::model::MergeOptions>,
    /// The scoped caller starting the execution.
    pub caller: Option<Caller>,
}

/// An execution whose wrapper holds this connection.
pub struct Connected {
    id: ExecutionId,
    kind: ExecutionKind,
    space: Space,
    agent: Option<TrackedAgent>,
    recover: bool,
    /// The wrapper keeps its command running when the daemon shuts down.
    reattach: bool,
    /// The wrapper has reported its running child.
    started: bool,
    /// The wrapper reattached while waiting to restore its stopped agent.
    awaiting_restore: bool,
    /// The daemon resumed the stopped agent, whose restored child has not
    /// started.
    resuming: bool,
    /// The wrapper took over no agent queue entry, so `shoal resume` started
    /// it rather than the daemon, and the agent it restores works on the
    /// assignment again.
    unqueued: bool,
    /// The wrapper reads [`Control::RestoreWaits`].
    hears_restore_waits: bool,
    /// The wrapper restores its agent under the narrowing a
    /// [`Control::Resume`] gives.
    takes_narrowing: bool,
    /// The wrapper's terminal reads the space's status reports.
    reports_status: bool,
    /// The daemon stopped the command before it finished.
    stopped: bool,
    /// Why Shoal cancelled on its own the automatic restore the wrapper
    /// waited for, which the agent's exit alert reports.
    restore_cancelled: Option<String>,
    stop: watch::Receiver<bool>,
    /// What the execution changes in the merge records once it ends.
    records: Option<RecordChange>,
    /// What made the launch whose queue entry the agent took over, which
    /// its start records.
    source: Option<LaunchSource>,
    /// What the command's scope recorded of its memory, as the wrapper
    /// reported it.
    memory: ReportedMemory,
}

/// What the scope of an execution's run recorded of its memory, as the
/// wrapper reports it: a pause reports the run it stopped, a run restored
/// after it reports its own, and the end reports the last run's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ReportedMemory(Option<ScopeMemory>);

impl ReportedMemory {
    /// The run a pause stopped recorded `reported`.
    fn paused(&mut self, reported: Option<ScopeMemory>) {
        self.0 = reported;
    }

    /// A run started, which reports its own, so a paused run's goes with
    /// its pause.
    fn started(&mut self) {
        self.0 = None;
    }

    /// The command finished, reporting `reported` for its last run, or
    /// nothing after its pause reported it.
    fn finished(&mut self, reported: Option<ScopeMemory>) {
        self.0 = reported.or(self.0);
    }

    /// What the last run recorded, for the execution's end.
    fn last(self) -> Option<ScopeMemory> {
        self.0
    }
}

/// How the wrapper's connection ended.
enum Ended {
    Finished(i32),
    /// The daemon is shutting down and the wrapper will reattach.
    Detached,
}

/// Register the wrapper's execution, then serve it until completion.
pub async fn execute(
    mut stream: UnixStream,
    request_id: u64,
    agents: Arc<Agents>,
    daemon: Arc<dyn Daemon>,
    context: ExecutionContext,
    operation: OwnedRwLockReadGuard<()>,
) -> Result<()> {
    let registration = registration::register(&agents, &*daemon, context);
    let (registered, listening) =
        registration::keeping_alive(&mut stream, request_id, registration).await;
    let Registered {
        execution,
        plan,
        git_guard: _git_guard,
    } = match registered {
        Ok(registered) => registered,
        Err(refusal) => {
            return protocol::write(&mut stream, &Response::new(request_id, *refusal)).await;
        }
    };
    let response = Response::new(request_id, Body::Execution(plan));
    let delivered = listening && protocol::write(&mut stream, &response).await.is_ok();
    if !delivered && registration::cancels_undelivered(&execution) {
        return registration::cancel(&agents, execution).await;
    }
    serve(stream, &agents, &*daemon, execution, None, operation).await
}

/// Restore a wrapper's connection after a daemon restart, then serve it like
/// any other, sending it only the controls its protocol `version` reads. A refusal
/// tells the wrapper to stop its command.
pub async fn reattach(
    mut stream: UnixStream,
    request_id: u64,
    version: u32,
    agents: Arc<Agents>,
    daemon: Arc<dyn Daemon>,
    request: Reattach,
    operation: OwnedRwLockReadGuard<()>,
) -> Result<()> {
    let reattached = async {
        // Where the platform reports it, the peer must be the recorded wrapper.
        let peer = stream.peer_cred()?.pid();
        ensure!(
            peer.is_none_or(|pid| u32::try_from(pid).ok() == Some(request.wrapper.pid)),
            "only the execution's wrapper can reattach it"
        );
        agents.reattach_execution(&*daemon, &request).await
    }
    .await;
    let ReattachedExecution { space, stop } = match reattached {
        Ok(reattached) => reattached,
        Err(error) => {
            let body = Body::error(ErrorCode::ExecutionFailed, format!("{error:#}"));
            return protocol::write(&mut stream, &Response::new(request_id, body)).await;
        }
    };
    let execution = Connected {
        id: request.execution,
        kind: ExecutionKind::Command,
        space,
        agent: request.agent.map(|name| TrackedAgent {
            name,
            model: request.model,
            mode: (request.mode).unwrap_or(crate::config::modes::ModeName::DEFAULT),
            narrowing: request.narrowing,
            new_session: false,
        }),
        recover: request.recover,
        reattach: true,
        started: true,
        awaiting_restore: request.awaiting_restore.is_some(),
        resuming: false,
        // Its agent started before the daemon restarted.
        unqueued: false,
        hears_restore_waits: version >= protocol::RESTORE_WAITS,
        takes_narrowing: version >= protocol::RESUME_NARROWING,
        reports_status: request.reports_status,
        stopped: false,
        restore_cancelled: None,
        stop,
        records: None,
        source: None,
        memory: ReportedMemory::default(),
    };
    serve(
        stream,
        &agents,
        &*daemon,
        execution,
        Some(Response::new(request_id, Body::Ok)),
        operation,
    )
    .await
}

/// Relay controls and events after `response`, unless the caller already sent
/// it, until the wrapper reports completion or disconnects, then record the
/// outcome. A wrapper reattaches only while its child runs with the daemon's
/// acknowledgement or while it waits to restore its agent. So the
/// registration's `operation` holds back a daemon update until the child runs,
/// and an overload stop or a reattachment while waiting holds it back until the
/// agent is restored. A running execution holds back no update; recording its
/// changes takes the guard again.
async fn serve(
    stream: UnixStream,
    agents: &Agents,
    daemon: &dyn Daemon,
    mut execution: Connected,
    response: Option<Response>,
    operation: OwnedRwLockReadGuard<()>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let (events, mut incoming) = mpsc::channel(4);
    let mut readers = JoinSet::new();
    readers.spawn(read_execution_events(reader, events));
    let mut handoff_claimed = false;
    let mut unattachable = (!execution.started || execution.awaiting_restore).then_some(operation);
    let responded = match &response {
        Some(response) => protocol::write(&mut writer, response).await,
        None => Ok(()),
    };
    let controls = Controls::new(writer);
    let result = match responded {
        Ok(()) => {
            relay_reporting(
                agents,
                daemon,
                &mut execution,
                &controls,
                &mut incoming,
                &mut handoff_claimed,
                &mut unattachable,
            )
            .await
        }
        Err(error) => Err(error),
    };
    readers.abort_all();
    // The Git gate the registration holds keeps the default branch as the
    // execution left it until the records and the lookup of a pushed merge's
    // runs follow it.
    if let Some(change) = &execution.records
        && !matches!(result, Ok(Ended::Detached))
    {
        daemon.update_merge_records(change).await;
        daemon.follow_pushed_merge_runs(change).await;
    }
    let result = match result {
        Ok(Ended::Detached) => {
            agents.detach_execution(&execution.id).await;
            return Ok(());
        }
        Ok(Ended::Finished(exit_code)) => {
            agents
                .executions
                .set_completing(&execution.id, Completing::Reported);
            Ok(exit_code)
        }
        Err(error) => Err(error),
    };
    // A restore ends when its wrapper finishes. One that lost its connection
    // keeps its place: its wrapper reattaches, or once its execution ends
    // the daemon starts its agent elsewhere.
    if execution.agent.is_some() && result.is_ok() {
        agents.remove_restore(&execution.id).await;
    }
    let operation = match unattachable {
        Some(operation) => operation,
        None => match agents.gates.try_owned_operation() {
            Some(operation) => operation,
            // An update is restarting the daemon; the wrapper reports its
            // exit to the replacement.
            None if execution.reattach && result.is_ok() => {
                agents.detach_execution(&execution.id).await;
                return Ok(());
            }
            None => agents.gates.owned_operation().await,
        },
    };
    finish(
        agents,
        daemon,
        execution,
        controls,
        result,
        handoff_claimed,
        operation,
    )
    .await
}

/// Relay as [`relay`] does, with the space's status reports going out
/// beside it once the wrapper says its terminal reads them, until the relay
/// ends.
async fn relay_reporting(
    agents: &Agents,
    daemon: &dyn Daemon,
    execution: &mut Connected,
    controls: &Controls,
    incoming: &mut mpsc::Receiver<Result<ExecutionEvent>>,
    handoff_claimed: &mut bool,
    unattachable: &mut Option<OwnedRwLockReadGuard<()>>,
) -> Result<Ended> {
    let (wants_reports, wanted) = watch::channel(execution.reports_status);
    let (ending, ended) = watch::channel(false);
    let reports = Reports {
        space: execution.space.clone(),
        execution: execution.id.clone(),
        agent: execution.agent.as_ref().map(|agent| agent.name.clone()),
    };
    let relayed = async {
        let result = relay(
            agents,
            daemon,
            execution,
            controls,
            &wants_reports,
            incoming,
            handoff_claimed,
            unattachable,
        )
        .await;
        // A report being sent goes out whole before the connection's end.
        ending.send_replace(true);
        result
    };
    let (result, ()) = tokio::join!(
        relayed,
        reports.run(agents, daemon, controls, wanted, ended)
    );
    result
}

/// Exchange controls and events until the wrapper reports its exit code or
/// detaches for a daemon shutdown.
#[allow(
    clippy::too_many_arguments,
    reason = "the connection's state, beside the service and the daemon"
)]
async fn relay(
    agents: &Agents,
    daemon: &dyn Daemon,
    execution: &mut Connected,
    controls: &Controls,
    wants_reports: &watch::Sender<bool>,
    incoming: &mut mpsc::Receiver<Result<ExecutionEvent>>,
    handoff_claimed: &mut bool,
    unattachable: &mut Option<OwnedRwLockReadGuard<()>>,
) -> Result<Ended> {
    let Connected {
        id: execution_id,
        kind,
        space,
        agent,
        recover,
        reattach,
        started,
        awaiting_restore,
        resuming,
        unqueued,
        hears_restore_waits,
        takes_narrowing,
        stopped,
        restore_cancelled,
        stop,
        source,
        memory,
        ..
    } = execution;
    let reattach = *reattach;
    let stopping = Stopping {
        execution_id,
        kind: *kind,
        agent: agent.is_some(),
        recover: *recover,
        reattach,
    };
    let mut awaiting_started = !*started;
    // A wrapper that reattached while waiting continues its pause here.
    let mut resumed_pause = std::mem::take(awaiting_restore);
    let mut sent_stop = resumed_pause;
    let mut recovering = resumed_pause;
    // Keeps permission changes out of the record a resumed agent restores
    // from until it starts.
    let mut restored_record = None;
    loop {
        let event = if std::mem::take(&mut resumed_pause) {
            ExecutionEvent::Paused { memory: None }
        } else if awaiting_started {
            incoming.recv().await.context("execution disconnected")??
        } else {
            let event = incoming.recv();
            tokio::pin!(event);
            loop {
                tokio::select! {
                    event = &mut event => break event.context("execution disconnected")??,
                    changed = stop.changed(), if !sent_stop => {
                        changed?;
                        if *stop.borrow_and_update() {
                            let PreparedStop { control, recovery } = prepare_stop(agents,stopping).await;
                            if let Some(recovery) = recovery {
                                recovering = recovery.recovering;
                                if unattachable.is_none() {
                                    *unattachable = recovery.update_hold;
                                }
                            }
                            controls.send(&control).await?;
                            if matches!(control, Control::Detach) {
                                return Ok(Ended::Detached);
                            }
                            sent_stop = true;
                        }
                    }
                }
            }
        };
        match event {
            ExecutionEvent::Started {
                child,
                group_id,
                restored,
                restored_from,
                reports_status,
            } => {
                awaiting_started = false;
                if reports_status {
                    wants_reports.send_replace(true);
                }
                *started = true;
                // The started agent restored from the record, which goes
                // while claimed: the wrapper consumes it only once this
                // start is acknowledged.
                if let Some(claim) = restored_record.take() {
                    execution_recovery::discard_handoff(agents, &space.id, execution_id);
                    drop(claim);
                }
                let resumed_here = std::mem::take(resuming);
                let operation = match unattachable.take() {
                    Some(operation) => operation,
                    None => agents.gates.owned_operation().await,
                };
                agents
                    .record_execution_child(execution_id.clone(), child, group_id)
                    .await?;
                let left = match agent {
                    Some(_) => (agents.left_places(&space.id, execution_id, &restored_from)).await,
                    None => Vec::new(),
                };
                if let Some(tracked) = agent {
                    if !restored {
                        agents.forget_last_sessions(&space.id);
                    } else {
                        agents.carry_restored_session(&space.id, execution_id, &restored_from);
                        if !resumed_here && *unqueued {
                            // A restore the agent queue starts withdrew it when
                            // queued if `shoal resume` asked for it.
                            daemon.reopen_for_resume(&space.id).await;
                        }
                    }
                    agents.agent_started(execution_id).await;
                    let started = AgentEvent {
                        source: source.take(),
                        ..agent_event(tracked, execution_id, None)
                    };
                    (shoal_daemon_base::events::record_agent(
                        &agents.store,
                        &space.id,
                        EventKind::AgentStarted,
                        started,
                    ))
                    .await;
                }
                memory.started();
                controls.send(&Control::Started).await?;
                // A slow host close holds back no daemon update; a stop
                // that arrives meanwhile waits in `stop` until it returns.
                drop(operation);
                agents.close_left_places(daemon, &space.id, left).await;
            }
            ExecutionEvent::Finished {
                exit_code,
                memory: reported,
            } => {
                *stopped = sent_stop;
                memory.finished(reported);
                return Ok(Ended::Finished(exit_code));
            }
            ExecutionEvent::ChildExited => {
                // A daemon update waits from the release until the next run
                // registers, as it does for a start, so it cannot detach the
                // wrapper while that registration waits for its answer.
                let operation = match unattachable.take() {
                    Some(operation) => operation,
                    None => agents.gates.owned_operation().await,
                };
                // A refusal stops the demo instead of letting it run again.
                let control = match agents
                    .release_exited_child(daemon, execution_id, &space.id)
                    .await
                {
                    Ok(()) => Control::ChildReleased,
                    Err(error) => {
                        sent_stop = true;
                        Control::Stop {
                            reason: Some(format!("{error:#}")),
                            removed: false,
                        }
                    }
                };
                controls.send(&control).await?;
                *unattachable = Some(operation);
            }
            // A wake that crossed the restore it asks for, or reached a
            // wrapper no restore waits for, has nothing left to start.
            ExecutionEvent::Wake => {}
            ExecutionEvent::Register { origin } => {
                // As for a start, a daemon update waits until the next child
                // runs, since the wrapper cannot reattach before it does.
                let operation = match unattachable.take() {
                    Some(operation) => operation,
                    None => agents.gates.owned_operation().await,
                };
                let place = origin.map(|origin| ExecutionPlace {
                    origin: *origin,
                    agent: agent.is_some(),
                });
                let control = register_again(agents, daemon, execution_id, &space.id, place).await;
                sent_stop |= matches!(control, Control::Stop { .. });
                controls.send(&control).await?;
                *unattachable = Some(operation);
            }
            ExecutionEvent::Paused { memory: reported } => {
                *handoff_claimed = true;
                // Named again by the stop's alert once the wrapper finishes
                // without a restore; a restored run reports its own.
                memory.paused(reported);
                if let Some(tracked) = agent {
                    let stopped = AgentEvent {
                        oom_kills: reported.map_or(0, |memory| memory.oom_kills),
                        ..agent_event(tracked, execution_id, None)
                    };
                    (shoal_daemon_base::events::record_agent(
                        &agents.store,
                        &space.id,
                        EventKind::AgentStopped,
                        stopped,
                    ))
                    .await;
                }
                // A wrapper whose own session ended saves its agent unasked,
                // which stops the agent as a requested pause does.
                sent_stop = true;
                if !recovering {
                    execution_recovery::stop_survivors(agents, daemon, execution_id, &space.id)
                        .await;
                    continue;
                }
                use execution_recovery::{Action, Resumed};
                // An update waits for the restore; other shutdowns hand the
                // wait to the next daemon.
                if unattachable.is_none() {
                    *unattachable = agents.gates.try_owned_operation();
                }
                let wrapper = execution_recovery::Wrapper {
                    notices: hears_restore_waits.then_some(controls),
                    runs_under: agent.as_ref().map(|agent| agent.narrowing),
                    takes_narrowing: *takes_narrowing,
                };
                match execution_recovery::pause(
                    agents,
                    daemon,
                    execution_id,
                    &space.id,
                    incoming,
                    stop,
                    wrapper,
                    unattachable,
                )
                .await?
                {
                    Action::Resume(resumed) => {
                        *handoff_claimed = false;
                        awaiting_started = true;
                        *resuming = true;
                        sent_stop = false;
                        let Resumed {
                            ports,
                            narrowing,
                            claim,
                        } = resumed;
                        restored_record = claim;
                        if let (Some(tracked), Some(narrowing)) = (agent.as_mut(), narrowing) {
                            tracked.narrowing = narrowing;
                        }
                        let executable = crate::installed::started().identity();
                        let resume = Control::Resume {
                            ports,
                            executable,
                            narrowing,
                        };
                        controls.send(&resume).await?;
                    }
                    // The next daemon continues the wait once the wrapper reattaches.
                    Action::Stop(_) if reattach && agents.executions.shutting_down() => {
                        controls.send(&Control::Detach).await?;
                        return Ok(Ended::Detached);
                    }
                    Action::Stop(cancel) => {
                        *restore_cancelled = cancel.unrequested_reason().map(str::to_owned);
                        controls.send(&cancel.control()).await?;
                    }
                    Action::Finished(exit_code) => {
                        *stopped = true;
                        return Ok(Ended::Finished(exit_code));
                    }
                }
            }
        }
    }
}

/// The plan of execution `id` of the space `space_id`, whose wrapper
/// registers again from `place`, or the stop that refuses it.
async fn register_again(
    agents: &Agents,
    daemon: &dyn Daemon,
    id: &ExecutionId,
    space_id: &SpaceId,
    place: Option<ExecutionPlace>,
) -> Control {
    match agents
        .reregister_execution(daemon, id, space_id, place)
        .await
    {
        Ok(plan) => Control::Registered {
            plan: Box::new(plan),
        },
        Err(error) => Control::Stop {
            reason: Some(format!("{error:#}")),
            removed: false,
        },
    }
}

/// What decides the control a stop sends to a connected execution.
#[derive(Clone, Copy)]
struct Stopping<'a> {
    execution_id: &'a ExecutionId,
    kind: ExecutionKind,
    /// The execution runs a tracked agent.
    agent: bool,
    recover: bool,
    reattach: bool,
}

/// The control a stop sends and, for an overload stop, its recovery.
struct PreparedStop {
    control: Control,
    recovery: Option<OverloadRecovery>,
}

/// Whether an overload stop's agent waits to be restored.
struct OverloadRecovery {
    recovering: bool,
    /// The update hold protection took, passed to the connection only while
    /// the agent recovers so an update waits for the restore.
    update_hold: Option<OwnedRwLockReadGuard<()>>,
}

/// Choose the control for a stop: an overload stop, a restart, a detach for
/// a daemon shutdown the wrapper outlives, or the execution's stop request.
async fn prepare_stop(agents: &Agents, stopping: Stopping<'_>) -> PreparedStop {
    if let Some(protection) = agents.agent_overload(stopping.execution_id).await {
        return prepare_overload_stop(agents, stopping, protection).await;
    }
    if agents.restart_requested(stopping.execution_id).await {
        return prepare_restart();
    }
    let control = if stopping.reattach && agents.executions.shutting_down() {
        Control::Detach
    } else {
        requested_stop(
            agents.executions.stop_request(stopping.execution_id).await,
            stopping,
        )
    };
    PreparedStop {
        control,
        recovery: None,
    }
}

/// Stop an overloaded agent, recovering it when the execution and current
/// policy allow. A stop without recovery releases protection's update hold.
async fn prepare_overload_stop(
    agents: &Agents,
    stopping: Stopping<'_>,
    protection: Protection,
) -> PreparedStop {
    // Read the policy now, so a reload applies to running agents.
    let recovering = stopping.recover
        && (!protection.follows_overload_recovery()
            || agents.config.current().machine.overload.recovery.enabled);
    let update_hold = agents
        .take_update_hold(stopping.execution_id)
        .await
        .filter(|_| recovering);
    PreparedStop {
        control: Control::OverloadStop {
            recover: recovering,
            reason: protection.reason,
            resumes_when: protection.resumes_when,
        },
        recovery: Some(OverloadRecovery {
            recovering,
            update_hold,
        }),
    }
}

/// A restart stops the agent as protection does, so wrappers of every
/// release restore it, and recovers it whatever the overload policy.
fn prepare_restart() -> PreparedStop {
    let Protection {
        reason,
        resumes_when,
    } = Protection::restart();
    PreparedStop {
        control: Control::OverloadStop {
            recover: true,
            reason,
            resumes_when,
        },
        recovery: Some(OverloadRecovery {
            recovering: true,
            update_hold: None,
        }),
    }
}

/// Pause a command whose stop request saves it for `shoal resume`, and stop
/// anything else.
fn requested_stop(request: Option<StopRequest>, stopping: Stopping<'_>) -> Control {
    match request {
        Some(request)
            if stopping.kind == ExecutionKind::Command && request.saves(stopping.agent) =>
        {
            Control::Pause {
                removed: request.keeps_removed_agents(),
                shutdown: request.before_shutdown(),
                reason: Some(request.reason),
            }
        }
        request => Control::Stop {
            reason: request.map(|request| request.reason),
            removed: false,
        },
    }
}

/// Record completion, tell the user about agent exits, and acknowledge the
/// wrapper. `result` is the reported exit code or the connection's failure.
/// An agent whose restart continues in a new wrapper did not exit: that
/// wrapper starts once this one is acknowledged.
async fn finish(
    agents: &Agents,
    daemon: &dyn Daemon,
    execution: Connected,
    controls: Controls,
    result: Result<i32>,
    handoff_claimed: bool,
    _operation: OwnedRwLockReadGuard<()>,
) -> Result<()> {
    let Connected {
        id: execution_id,
        kind,
        space,
        agent,
        started,
        resuming,
        stopped,
        restore_cancelled,
        memory,
        ..
    } = execution;
    let memory = memory.last();
    if let Some(memory) = memory {
        daemon.record_scope_memory(&execution_id, memory).await;
    }
    let oom_kills = memory.map_or(0, |memory| memory.oom_kills);
    let mut overload_reason = if agent.is_some() {
        agents
            .agent_overload(&execution_id)
            .await
            .map(|protection| protection.reason)
    } else {
        None
    };
    // A restart whose stop the agent never took saved a handoff for nothing.
    let restarting = agent.is_some() && agents.restart_before_resume(&execution_id).await;
    let renewed = agent.is_some() && agents.restart_continues_elsewhere(&execution_id).await;
    // Its last session keeps the turn it ended in, which goes with its record.
    let turn = match agent {
        Some(_) => agents.reported_turn(&execution_id).await,
        None => None,
    };
    // A demo's viewer that spawns from here on would outlive the final
    // inventory, so its spawn waits, then finds the execution completing.
    let spawning = daemon.viewer_spawn_lock(&execution_id);
    let spawning = spawning.lock().await;
    let reported = daemon
        .reported_end(&execution_id, result.as_ref().ok().copied())
        .await;
    // Setup instead fails on what it leaves running, and merge runs Git.
    let cleared = if kind == ExecutionKind::Command && result.is_ok() {
        agents
            .executions
            .set_completing(&execution_id, Completing::StoppingSurvivors);
        agents.stop_exited_processes(daemon, &execution_id).await
    } else {
        None
    };
    // The record goes with the execution, so a start waiting on it learns
    // from the daemon that its command never ran.
    let unrun = daemon
        .unrun_demo_launch(&execution_id)
        .await
        .unwrap_or_default();
    agents
        .executions
        .set_completing(&execution_id, Completing::Verifying);
    let complete = agents
        .finish_execution_scanned(daemon, execution_id.clone(), kind, reported, cleared)
        .await?;
    drop(spawning);
    daemon.end_viewer(&execution_id);
    if let Some(launch) = unrun {
        agents.executions.remember_unrun_launch(launch);
    }
    // A wrapper that finished before the agent it resumed started failed to
    // restore it, so its record stays the agent's.
    let failed_restore = result.as_ref().ok().filter(|_| resuming).copied();
    if let Some(agent) = &agent
        && result.is_ok()
    {
        let exited = started && !stopped && failed_restore.is_none();
        agents
            .end_agent_session(daemon, &space.id, agent, &execution_id, exited, turn)
            .await;
    }
    if (overload_reason.is_some() || restarting) && !handoff_claimed && complete {
        execution_recovery::discard_handoff(agents, &space.id, &execution_id);
        overload_reason = None;
    }
    let requeued = if let Some(agent) = &agent
        && let Some(code) = failed_restore
    {
        agents
            .queue_failed_restore(daemon, &space, &agent.name, &execution_id, code)
            .await
    } else {
        false
    };
    if let Some(agent) = agent.as_ref().filter(|_| !requeued && !renewed) {
        let code = result.as_ref().ok().copied();
        let (kind, code) = match stopped {
            true => (EventKind::AgentStopped, None),
            false => (EventKind::AgentExited, code),
        };
        let ended = AgentEvent {
            oom_kills,
            ..agent_event(agent, &execution_id, code)
        };
        shoal_daemon_base::events::record_agent(&agents.store, &space.id, kind, ended).await;
        agents
            .notify_agent_exit(
                daemon,
                &space,
                &agent.name,
                result.as_ref().ok().copied(),
                complete,
                overload_reason
                    .as_deref()
                    .map(|reason| crate::control::OverloadEnd {
                        execution: &execution_id,
                        reason,
                        restore_cancelled: restore_cancelled.as_deref(),
                    }),
                oom_kills,
            )
            .await;
    }
    let acknowledged = if result.is_ok() {
        controls.send(&Control::Finished { complete }).await
    } else {
        Ok(())
    };
    if renewed {
        let finished = result.is_ok() && complete;
        agents
            .restart_in_new_wrapper(daemon, &space, &execution_id, finished)
            .await;
    }
    // A bounded hook can outlast the wrapper's acknowledgement deadline.
    if let Some(agent) = agent.filter(|_| !renewed) {
        agents
            .post_agent_exit(
                daemon,
                &space,
                &agent.name,
                result.as_ref().ok().copied(),
                complete,
            )
            .await;
    }
    acknowledged?;
    result.map(|_| ())
}

/// What a space's log records of the agent that execution `id` runs.
fn agent_event(agent: &TrackedAgent, id: &ExecutionId, exit_code: Option<i32>) -> AgentEvent {
    AgentEvent {
        name: agent.name.clone(),
        execution_id: Some(id.to_owned()),
        exit_code,
        oom_kills: 0,
        source: None,
    }
}

/// Keep frame reads alive across control changes so stop/recovery cannot discard
/// a partially received execution event.
async fn read_execution_events(
    reader: tokio::net::unix::OwnedReadHalf,
    events: mpsc::Sender<Result<ExecutionEvent>>,
) {
    let mut reader = tokio::io::BufReader::new(reader);
    loop {
        let event = protocol::read_buffered::<ExecutionEvent>(&mut reader).await;
        let failed = event.is_err();
        if events.send(event).await.is_err() || failed {
            break;
        }
    }
}

pub mod controls;
pub mod registration;

pub use controls::Controls;

pub use shoal_daemon_base::merges::RecordChange;

#[cfg(test)]
mod tests;
