//! Long-lived execution connections: the daemon side of the wrapper protocol.
//! The wrapper reports its child and completion; the daemon relays stop and
//! recovery controls and records the outcome.
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use tokio::{
    net::{UnixStream, unix::OwnedWriteHalf},
    sync::{OwnedRwLockReadGuard, mpsc, watch},
    task::JoinSet,
};

use super::{
    approvals::Undecided,
    execution_recovery,
    scope::Caller,
    workspace::{ExecutionKind, Manager, ReattachedExecution, Save, StartedExecution, StopRequest},
};
use crate::{
    model::Workspace,
    process::identity::Identity,
    protocol::{self, Body, Control, ErrorCode, ExecutionEvent, Protection, Reattach, Response},
};

pub(super) struct ExecutionContext {
    pub workspace: String,
    pub wrapper: Identity,
    pub kind: ExecutionKind,
    pub agent: Option<String>,
    pub recover: bool,
    pub reattach: bool,
    /// The starting queue entry whose slot this agent takes over.
    pub queue_entry: Option<String>,
    /// The demo the command runs.
    pub demo: Option<crate::demos::Launch>,
    /// The scoped caller starting the execution.
    pub caller: Option<Caller>,
}

/// An execution whose wrapper holds this connection.
struct Connected {
    id: String,
    kind: ExecutionKind,
    workspace: Workspace,
    agent: Option<String>,
    recover: bool,
    /// The wrapper keeps its command running when the daemon shuts down.
    reattach: bool,
    /// The wrapper has reported its running child.
    started: bool,
    /// The wrapper reattached while waiting to restore its stopped agent.
    awaiting_restore: bool,
    /// The daemon stopped the command before it finished.
    stopped: bool,
    stop: watch::Receiver<bool>,
}

/// How the wrapper's connection ended.
enum Ended {
    Finished(i32),
    /// The daemon is shutting down and the wrapper will reattach.
    Detached,
}

/// Register the wrapper's execution, then serve it until completion.
pub(super) async fn execute(
    mut stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
    context: ExecutionContext,
    operation: OwnedRwLockReadGuard<()>,
) -> Result<()> {
    let ExecutionContext {
        workspace,
        wrapper,
        kind,
        agent,
        recover,
        reattach,
        queue_entry,
        demo,
        caller,
    } = context;
    let StartedExecution {
        plan,
        stop,
        _git_guard,
    } = match manager
        .begin_execution_running(&workspace, Some(wrapper), kind, caller.as_ref(), demo)
        .await
    {
        Ok(begun) => begun,
        Err(error) => {
            let body = match error.downcast::<Undecided>() {
                Ok(Undecided(request)) => {
                    manager.notify_approval(&request.workspace, &request).await;
                    Body::ApprovalRequest(request)
                }
                Err(error) => Body::error(ErrorCode::ExecutionFailed, format!("{error:#}")),
            };
            return protocol::write(&mut stream, &Response::new(request_id, body)).await;
        }
    };
    if !reattach {
        manager.mark_unattachable(&plan.id);
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
        if let Some(entry) = &queue_entry {
            manager.consume_queue_entry(entry, &plan.workspace.id).await;
        }
    }
    let execution = Connected {
        id: plan.id.clone(),
        kind,
        workspace: plan.workspace.clone(),
        agent,
        recover,
        reattach,
        started: false,
        awaiting_restore: false,
        stopped: false,
        stop,
    };
    let response = Response::new(request_id, Body::Execution(plan));
    serve(stream, &manager, execution, response, operation).await
}

/// Restore a wrapper's connection after a daemon restart, then serve it like
/// any other. A refusal tells the wrapper to stop its command.
pub(super) async fn reattach(
    mut stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
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
        manager.reattach_execution(&request).await
    }
    .await;
    let ReattachedExecution { workspace, stop } = match reattached {
        Ok(reattached) => reattached,
        Err(error) => {
            let body = Body::error(ErrorCode::ExecutionFailed, format!("{error:#}"));
            return protocol::write(&mut stream, &Response::new(request_id, body)).await;
        }
    };
    let execution = Connected {
        id: request.execution,
        kind: ExecutionKind::Command,
        workspace,
        agent: request.agent,
        recover: request.recover,
        reattach: true,
        started: true,
        awaiting_restore: request.awaiting_restore.is_some(),
        stopped: false,
        stop,
    };
    serve(
        stream,
        &manager,
        execution,
        Response::new(request_id, Body::Ok),
        operation,
    )
    .await
}

/// Relay controls and events after `response` until the wrapper reports
/// completion or disconnects, then record the outcome. A wrapper reattaches
/// only while its child runs with the daemon's acknowledgement or while it
/// waits to restore its agent, so the registration's `operation` holds back a
/// daemon update until the child runs, as does an overload stop, or a
/// reattachment while waiting, until the agent is restored. Otherwise a
/// running execution does not; recording its changes takes the guard again.
async fn serve(
    stream: UnixStream,
    manager: &Manager,
    mut execution: Connected,
    response: Response,
    operation: OwnedRwLockReadGuard<()>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let (events, mut incoming) = mpsc::channel(4);
    let mut readers = JoinSet::new();
    readers.spawn(read_execution_events(reader, events));
    let mut handoff_claimed = false;
    let mut unattachable = (!execution.started || execution.awaiting_restore).then_some(operation);
    let result = async {
        protocol::write(&mut writer, &response).await?;
        relay(
            manager,
            &mut execution,
            &mut writer,
            &mut incoming,
            &mut handoff_claimed,
            &mut unattachable,
        )
        .await
    }
    .await;
    readers.abort_all();
    let result = match result {
        Ok(Ended::Detached) => {
            manager.detach_execution(&execution.id).await;
            return Ok(());
        }
        Ok(Ended::Finished(exit_code)) => Ok(exit_code),
        Err(error) => Err(error),
    };
    // A restore ends when its wrapper finishes. One that lost its connection
    // reattaches, so it keeps its place for the start grace period, as a
    // detached one does for the next daemon.
    if execution.agent.is_some() {
        match &result {
            Ok(_) => manager.remove_restore(&execution.id).await,
            Err(_) => manager.keep_restore_place(&execution.id).await,
        }
    }
    let operation = match unattachable {
        Some(operation) => operation,
        None => match manager.background_operations.clone().try_read_owned() {
            Ok(operation) => operation,
            // An update is restarting the daemon; the wrapper reports its
            // exit to the replacement.
            Err(_) if execution.reattach && result.is_ok() => {
                manager.detach_execution(&execution.id).await;
                return Ok(());
            }
            Err(_) => manager.background_operations.clone().read_owned().await,
        },
    };
    finish(
        manager,
        execution,
        writer,
        result,
        handoff_claimed,
        operation,
    )
    .await
}

/// Exchange controls and events until the wrapper reports its exit code or
/// detaches for a daemon shutdown.
async fn relay(
    manager: &Manager,
    execution: &mut Connected,
    writer: &mut OwnedWriteHalf,
    incoming: &mut mpsc::Receiver<Result<ExecutionEvent>>,
    handoff_claimed: &mut bool,
    unattachable: &mut Option<OwnedRwLockReadGuard<()>>,
) -> Result<Ended> {
    let Connected {
        id: execution_id,
        kind,
        workspace,
        agent,
        recover,
        reattach,
        started,
        awaiting_restore,
        stopped,
        stop,
        ..
    } = execution;
    let reattach = *reattach;
    let stopping = Stopping {
        execution_id: execution_id.as_str(),
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
    loop {
        let event = if std::mem::take(&mut resumed_pause) {
            ExecutionEvent::Paused
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
                            let PreparedStop { control, recovery } = prepare_stop(manager, stopping).await;
                            if let Some(recovery) = recovery {
                                recovering = recovery.recovering;
                                if unattachable.is_none() {
                                    *unattachable = recovery.update_hold;
                                }
                            }
                            protocol::write(writer, &control).await?;
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
            ExecutionEvent::Started { child, group_id } => {
                awaiting_started = false;
                *started = true;
                let _operation = match unattachable.take() {
                    Some(operation) => operation,
                    None => manager.background_operations.clone().read_owned().await,
                };
                manager
                    .record_execution_child(execution_id.clone(), child, group_id)
                    .await?;
                if agent.is_some() {
                    manager.forget_last_session(&workspace.id);
                }
                protocol::write(writer, &Control::Started).await?;
            }
            ExecutionEvent::Finished { exit_code } => {
                *stopped = sent_stop;
                return Ok(Ended::Finished(exit_code));
            }
            ExecutionEvent::ChildExited => manager.release_exited_child(execution_id).await?,
            ExecutionEvent::Paused => {
                *handoff_claimed = true;
                // A wrapper whose own session ended saves its agent unasked,
                // which stops the agent as a requested pause does.
                sent_stop = true;
                if !recovering {
                    execution_recovery::stop_survivors(manager, execution_id, &workspace.id).await;
                    continue;
                }
                use execution_recovery::Action;
                // An update waits for the restore; other shutdowns hand the
                // wait to the next daemon.
                if unattachable.is_none() {
                    *unattachable = manager.background_operations.clone().try_read_owned().ok();
                }
                match execution_recovery::pause(
                    manager,
                    execution_id,
                    &workspace.id,
                    incoming,
                    stop,
                    recovering,
                )
                .await?
                {
                    Action::Resume(ports) => {
                        *handoff_claimed = false;
                        awaiting_started = true;
                        sent_stop = false;
                        protocol::write(writer, &Control::Resume { ports }).await?;
                    }
                    // The next daemon continues the wait once the wrapper reattaches.
                    Action::Stop(_) if reattach && manager.shutting_down() => {
                        protocol::write(writer, &Control::Detach).await?;
                        return Ok(Ended::Detached);
                    }
                    Action::Stop(reason) => {
                        protocol::write(writer, &Control::Stop { reason }).await?
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

/// What decides the control a stop sends to a connected execution.
#[derive(Clone, Copy)]
struct Stopping<'a> {
    execution_id: &'a str,
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

/// Choose the control for a stop: an overload stop, a detach for a daemon
/// shutdown the wrapper outlives, or the execution's stop request.
async fn prepare_stop(manager: &Manager, stopping: Stopping<'_>) -> PreparedStop {
    if let Some(protection) = manager.agent_overload(stopping.execution_id).await {
        return prepare_overload_stop(manager, stopping, protection).await;
    }
    let control = if stopping.reattach && manager.shutting_down() {
        Control::Detach
    } else {
        requested_stop(manager.stop_request(stopping.execution_id).await, stopping)
    };
    PreparedStop {
        control,
        recovery: None,
    }
}

/// Stop an overloaded agent, recovering it when the execution and current
/// policy allow. A stop without recovery releases protection's update hold.
async fn prepare_overload_stop(
    manager: &Manager,
    stopping: Stopping<'_>,
    protection: Protection,
) -> PreparedStop {
    // Read the policy now, so a reload applies to running agents.
    let recovering = stopping.recover && manager.config().overload.recovery.enabled;
    let update_hold = manager
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

/// Pause a command whose stop request saves it for `shoal resume`, and stop
/// anything else.
fn requested_stop(request: Option<StopRequest>, stopping: Stopping<'_>) -> Control {
    match request {
        Some(request)
            if stopping.kind == ExecutionKind::Command && request.saves(stopping.agent) =>
        {
            Control::Pause {
                removed: request.save == Save::RemovedAgents,
                reason: Some(request.reason),
            }
        }
        request => Control::Stop {
            reason: request.map(|request| request.reason),
        },
    }
}

/// Record completion, tell the user about agent exits, and acknowledge the
/// wrapper. `result` is the reported exit code or the connection's failure.
async fn finish(
    manager: &Manager,
    execution: Connected,
    mut writer: OwnedWriteHalf,
    result: Result<i32>,
    handoff_claimed: bool,
    _operation: OwnedRwLockReadGuard<()>,
) -> Result<()> {
    let Connected {
        id: execution_id,
        kind,
        workspace,
        agent,
        started,
        stopped,
        ..
    } = execution;
    let mut overload_reason = if agent.is_some() {
        manager
            .agent_overload(&execution_id)
            .await
            .map(|protection| protection.reason)
    } else {
        None
    };
    if agent.is_some() && result.is_ok() {
        manager.stop_exited_child_processes(&execution_id).await;
    }
    let complete = manager
        .finish_execution(execution_id.clone(), kind, result.as_ref().ok().copied())
        .await?;
    if let Some(agent) = &agent
        && result.is_ok()
    {
        manager
            .end_agent_session(&workspace, agent, &execution_id, started && !stopped)
            .await;
    }
    if overload_reason.is_some() && !handoff_claimed && complete {
        execution_recovery::discard_handoff(manager, &workspace.id, &execution_id);
        overload_reason = None;
    }
    if let Some(agent) = &agent {
        manager
            .notify_agent_exit(
                &workspace,
                agent,
                result.as_ref().ok().copied(),
                complete,
                overload_reason
                    .as_deref()
                    .map(|reason| (execution_id.as_str(), reason)),
            )
            .await;
    }
    let acknowledged = if result.is_ok() {
        protocol::write(&mut writer, &Control::Finished { complete }).await
    } else {
        Ok(())
    };
    // A bounded hook can outlast the wrapper's acknowledgement deadline.
    if let Some(agent) = agent {
        manager
            .post_agent_exit(&workspace, &agent, result.as_ref().ok().copied(), complete)
            .await;
    }
    acknowledged?;
    result.map(|_| ())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{manager, repository};
    use tokio::io::BufReader;

    async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(std::time::Duration::from_secs(60), future)
            .await
            .expect("execution connection stalled")
    }

    async fn read<T: serde::de::DeserializeOwned>(stream: &mut BufReader<UnixStream>) -> T {
        bounded(protocol::read_buffered(stream)).await.unwrap()
    }

    #[tokio::test]
    async fn scoped_landing_answers_with_its_approval_request() {
        let (root, manager) = manager().await;
        let path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "lander".into(), None, None, None)
            .await
            .unwrap();
        let (client, server) = UnixStream::pair().unwrap();
        let executing = tokio::spawn(execute(
            server,
            3,
            manager.clone(),
            ExecutionContext {
                workspace: workspace.id.clone(),
                wrapper: crate::process::identity::capture(std::process::id())
                    .unwrap()
                    .unwrap(),
                kind: ExecutionKind::Land,
                agent: None,
                recover: false,
                reattach: false,
                queue_entry: None,
                demo: None,
                caller: Some(Caller {
                    workspace_id: workspace.id.clone(),
                    execution: None,
                }),
            },
            manager.background_operations.clone().read_owned().await,
        ));
        let response: Response = read(&mut BufReader::new(client)).await;
        bounded(executing).await.unwrap().unwrap();
        let Body::ApprovalRequest(request) = response.body else {
            panic!("landing was not held for approval: {:?}", response.body);
        };
        assert_eq!(request.target, crate::daemon::approvals::Target::Land);
        let notifications = manager.notifications(true, 10).await.unwrap();
        assert!(
            notifications
                .iter()
                .any(|n| n.message.contains(&format!("approval {}:", request.id))),
            "{notifications:?}"
        );
    }

    #[tokio::test]
    async fn shutdown_detaches_reattached_wrappers_and_stops_older_ones() {
        let (root, manager) = manager().await;
        let path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "detach".into(), None, None, None)
            .await
            .unwrap();
        let wrapper = crate::process::identity::capture(std::process::id())
            .unwrap()
            .unwrap();
        // A capable wrapper from before the restart.
        let started = manager
            .begin_execution(
                &workspace.id,
                Some(wrapper.clone()),
                ExecutionKind::Command,
                None,
            )
            .await
            .unwrap();
        let detaching = started.plan.id.clone();
        manager
            .record_execution_child(detaching.clone(), Some(wrapper.clone()), wrapper.pid)
            .await
            .unwrap();
        manager.detach_execution(&detaching).await;
        let (client, server) = UnixStream::pair().unwrap();
        let reattached = tokio::spawn(reattach(
            server,
            7,
            manager.clone(),
            Reattach {
                execution: detaching.clone(),
                wrapper: wrapper.clone(),
                child: Some(wrapper.clone()),
                group_id: wrapper.pid,
                scope_token: started.plan.scope_token.clone(),
                agent: None,
                recover: false,
                running_ms: 0,
                awaiting_restore: None,
            },
            manager.background_operations.clone().read_owned().await,
        ));
        let mut capable = BufReader::new(client);
        let response: Response = read(&mut capable).await;
        assert!(matches!(response.body, Body::Ok) && response.id == 7);
        // A wrapper that cannot reattach, registered over the same protocol.
        let (client, server) = UnixStream::pair().unwrap();
        let executing = tokio::spawn(execute(
            server,
            8,
            manager.clone(),
            ExecutionContext {
                workspace: workspace.id.clone(),
                wrapper: wrapper.clone(),
                kind: ExecutionKind::Command,
                agent: None,
                recover: false,
                reattach: false,
                queue_entry: None,
                demo: None,
                caller: None,
            },
            manager.background_operations.clone().read_owned().await,
        ));
        let mut legacy = BufReader::new(client);
        let response: Response = read(&mut legacy).await;
        assert!(manager.has_unattachable_connections());
        let Body::Execution(plan) = response.body else {
            panic!("execution was not registered");
        };
        protocol::write(
            legacy.get_mut(),
            &ExecutionEvent::Started {
                child: None,
                group_id: wrapper.pid,
            },
        )
        .await
        .unwrap();
        assert!(matches!(read(&mut legacy).await, Control::Started));
        let shutdown = tokio::spawn({
            let manager = manager.clone();
            async move { manager.stop_for_shutdown().await }
        });
        assert!(matches!(read(&mut capable).await, Control::Detach));
        bounded(reattached).await.unwrap().unwrap();
        assert!(!manager.execution_connected(&detaching).await);
        assert!(matches!(
            read(&mut legacy).await,
            Control::Pause { reason: Some(reason), removed: false } if reason == "the Shoal daemon shut down"
        ));
        protocol::write(legacy.get_mut(), &ExecutionEvent::Finished { exit_code: 0 })
            .await
            .unwrap();
        assert!(matches!(
            read(&mut legacy).await,
            Control::Finished { complete: true }
        ));
        bounded(executing).await.unwrap().unwrap();
        bounded(shutdown).await.unwrap();
        assert!(!manager.has_unattachable_connections());
        // The detached execution awaits reattachment; the stopped one is gone.
        let executions = manager
            .inspect_workspace(&workspace.id)
            .await
            .unwrap()
            .executions;
        assert_eq!(executions.len(), 1);
        assert_eq!(executions[0].id, detaching);
        assert_ne!(executions[0].id, plan.id);
    }

    #[tokio::test]
    async fn running_executions_leave_daemon_operations_idle() {
        let (root, manager) = manager().await;
        let path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "idle".into(), None, None, None)
            .await
            .unwrap();
        let wrapper = crate::process::identity::capture(std::process::id())
            .unwrap()
            .unwrap();
        let (client, server) = UnixStream::pair().unwrap();
        let executing = tokio::spawn(execute(
            server,
            1,
            manager.clone(),
            ExecutionContext {
                workspace: workspace.id.clone(),
                wrapper: wrapper.clone(),
                kind: ExecutionKind::Command,
                agent: None,
                recover: false,
                reattach: true,
                caller: None,
                queue_entry: None,
                demo: None,
            },
            manager.background_operations.clone().read_owned().await,
        ));
        let mut wrapper_side = BufReader::new(client);
        let response: Response = read(&mut wrapper_side).await;
        assert!(matches!(response.body, Body::Execution(_)));
        // Until its child is recorded, the wrapper cannot reattach.
        assert!(manager.background_operations.try_write().is_err());
        protocol::write(
            wrapper_side.get_mut(),
            &ExecutionEvent::Started {
                child: None,
                group_id: wrapper.pid,
            },
        )
        .await
        .unwrap();
        assert!(matches!(read(&mut wrapper_side).await, Control::Started));
        // An update can take every operation while the command still runs.
        let update = bounded(manager.background_operations.write()).await;
        // A command exiting meanwhile reports to the replacement daemon.
        protocol::write(
            wrapper_side.get_mut(),
            &ExecutionEvent::Finished { exit_code: 0 },
        )
        .await
        .unwrap();
        bounded(executing).await.unwrap().unwrap();
        drop(update);
        let executions = manager
            .inspect_workspace(&workspace.id)
            .await
            .unwrap()
            .executions;
        assert_eq!(executions.len(), 1);
        assert!(!manager.execution_connected(&executions[0].id).await);
    }
}
