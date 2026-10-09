//! Long-lived execution connections: the daemon side of the wrapper protocol.
//! The wrapper reports its child and completion; the daemon relays stop and
//! recovery controls and records the outcome.
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::{
    net::{UnixStream, unix::OwnedWriteHalf},
    sync::{mpsc, watch},
    task::JoinSet,
};

use super::{
    execution_recovery,
    workspace::{ExecutionKind, Manager, StartedExecution},
};
use crate::{
    model::Workspace,
    process::identity::Identity,
    protocol::{self, Body, Control, ErrorCode, ExecutionEvent, Response},
};

pub(super) struct ExecutionContext {
    pub workspace: String,
    pub wrapper: Identity,
    pub kind: ExecutionKind,
    pub agent: Option<String>,
    pub recover: bool,
    pub parent_execution: Option<String>,
}

/// An execution whose wrapper holds this connection.
struct Connected {
    id: String,
    kind: ExecutionKind,
    workspace: Workspace,
    agent: Option<String>,
    recover: bool,
    stop: watch::Receiver<bool>,
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
            )
            .await;
    }
    let execution = Connected {
        id: plan.id.clone(),
        kind,
        workspace: plan.workspace.clone(),
        agent,
        recover,
        stop,
    };
    let response = Response::new(request_id, Body::Execution(plan));
    serve(stream, &manager, execution, response).await
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
    finish(manager, execution, writer, result, handoff_claimed).await
}

/// Exchange controls and events until the wrapper reports its exit code.
async fn relay(
    manager: &Manager,
    execution: &mut Connected,
    writer: &mut OwnedWriteHalf,
    incoming: &mut mpsc::Receiver<Result<ExecutionEvent>>,
    handoff_claimed: &mut bool,
) -> Result<i32> {
    let Connected {
        id: execution_id,
        kind,
        workspace,
        recover,
        stop,
        ..
    } = execution;
    let (kind, recover) = (*kind, *recover);
    let mut awaiting_started = true;
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
                            let control = if let Some(reason) = manager.agent_overload_reason(execution_id).await {
                                // Read the policy now, so a reload applies to running agents.
                                recovering = recover && manager.config().overload.recovery.enabled;
                                Control::OverloadStop { recover: recovering, reason }
                            } else if kind == ExecutionKind::Command && manager.stop_saves_records(execution_id).await {
                                Control::Pause
                            } else { Control::Stop };
                            protocol::write(writer, &control).await?;
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
            ExecutionEvent::Finished { exit_code } => return Ok(exit_code),
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
                    Action::Finished(exit_code) => return Ok(exit_code),
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
        manager.agent_overload_reason(&execution_id).await
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
            eprintln!("unused overload recovery handoff not removed: {error:#}");
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
