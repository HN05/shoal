//! Long-lived execution connections: the daemon side of the wrapper protocol.
//! The wrapper reports its child and completion; the daemon relays stop and
//! recovery controls and records the outcome.
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use tokio::{
    net::{UnixStream, unix::OwnedWriteHalf},
    sync::{mpsc, watch},
    task::JoinSet,
};

use super::{
    execution_recovery,
    workspace::{ExecutionKind, Manager, ReattachedExecution, StartedExecution},
};
use crate::daemon::log;
use crate::{
    model::Workspace,
    process::identity::Identity,
    protocol::{self, Body, Control, ErrorCode, ExecutionEvent, Reattach, Response},
};

pub(super) struct ExecutionContext {
    pub workspace: String,
    pub wrapper: Identity,
    pub kind: ExecutionKind,
    pub agent: Option<String>,
    pub recover: bool,
    pub reattach: bool,
    pub parent_execution: Option<String>,
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
) -> Result<()> {
    let ExecutionContext {
        workspace,
        wrapper,
        kind,
        agent,
        recover,
        reattach,
        parent_execution,
    } = context;
    let StartedExecution {
        plan,
        stop,
        _git_guard,
    } = match manager
        .begin_execution(&workspace, Some(wrapper), kind, parent_execution.as_deref())
        .await
    {
        Ok(begun) => begun,
        Err(error) => {
            let body = Body::error(ErrorCode::ExecutionFailed, format!("{error:#}"));
            return protocol::write(&mut stream, &Response::new(request_id, body)).await;
        }
    };
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
    }
    let execution = Connected {
        id: plan.id.clone(),
        kind,
        workspace: plan.workspace.clone(),
        agent,
        recover,
        reattach,
        started: false,
        stop,
    };
    let response = Response::new(request_id, Body::Execution(plan));
    serve(stream, &manager, execution, response).await
}

/// Restore a wrapper's connection after a daemon restart, then serve it like
/// any other. A refusal tells the wrapper to stop its command.
pub(super) async fn reattach(
    mut stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
    request: Reattach,
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
        stop,
    };
    serve(
        stream,
        &manager,
        execution,
        Response::new(request_id, Body::Ok),
    )
    .await
}

/// Relay controls and events after `response` until the wrapper reports
/// completion or disconnects, then record the outcome.
async fn serve(
    stream: UnixStream,
    manager: &Manager,
    mut execution: Connected,
    response: Response,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let (events, mut incoming) = mpsc::channel(4);
    let mut readers = JoinSet::new();
    readers.spawn(read_execution_events(reader, events));
    let mut handoff_claimed = false;
    let result = async {
        protocol::write(&mut writer, &response).await?;
        relay(
            manager,
            &mut execution,
            &mut writer,
            &mut incoming,
            &mut handoff_claimed,
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
    finish(manager, execution, writer, result, handoff_claimed).await
}

/// Exchange controls and events until the wrapper reports its exit code or
/// detaches for a daemon shutdown.
async fn relay(
    manager: &Manager,
    execution: &mut Connected,
    writer: &mut OwnedWriteHalf,
    incoming: &mut mpsc::Receiver<Result<ExecutionEvent>>,
    handoff_claimed: &mut bool,
) -> Result<Ended> {
    let Connected {
        id: execution_id,
        kind,
        workspace,
        recover,
        reattach,
        started,
        stop,
        ..
    } = execution;
    let (kind, recover, reattach) = (*kind, *recover, *reattach);
    let mut awaiting_started = !*started;
    let mut sent_stop = false;
    let mut recovering = false;
    loop {
        let event = if awaiting_started {
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
                            let control = if let Some(protection) = manager.agent_overload(execution_id).await {
                                // Read the policy now, so a reload applies to running agents.
                                recovering = recover && manager.config().overload.recovery.enabled;
                                Control::OverloadStop {
                                    recover: recovering,
                                    reason: protection.reason,
                                    resumes_when: protection.resumes_when,
                                }
                            } else if reattach && manager.shutting_down() {
                                Control::Detach
                            } else if kind == ExecutionKind::Command && let Some(reason) = manager.resumable_stop_reason(execution_id).await {
                                Control::Pause { reason: Some(reason) }
                            } else { Control::Stop };
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
                manager
                    .record_execution_child(execution_id.clone(), child, group_id)
                    .await?;
                protocol::write(writer, &Control::Started).await?;
            }
            ExecutionEvent::Finished { exit_code } => return Ok(Ended::Finished(exit_code)),
            ExecutionEvent::Paused => {
                *handoff_claimed = true;
                if !recovering {
                    continue;
                }
                use execution_recovery::Action;
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
                    Action::Stop => protocol::write(writer, &Control::Stop).await?,
                    Action::Finished(exit_code) => return Ok(Ended::Finished(exit_code)),
                }
            }
        }
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
) -> Result<()> {
    let Connected {
        id: execution_id,
        kind,
        workspace,
        agent,
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
    let complete = manager
        .finish_execution(execution_id.clone(), kind, result.as_ref().ok().copied())
        .await?;
    if overload_reason.is_some() && !handoff_claimed && complete {
        let record =
            crate::execution::recovery::record_path(&manager.paths, &workspace.id, &execution_id);
        if let Err(error) = std::fs::remove_file(&record)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            log!("unused overload recovery handoff not removed: {error:#}");
        }
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
            },
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
                parent_execution: None,
            },
        ));
        let mut legacy = BufReader::new(client);
        let response: Response = read(&mut legacy).await;
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
            Control::Pause { reason: Some(reason) } if reason == "the Shoal daemon shut down"
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
}
