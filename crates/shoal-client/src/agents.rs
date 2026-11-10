//! The agents running in a space: the state and turns their hooks report,
//! the user's interrupts, and the sessions they run.
use crate::execution_ids::ExecutionId;
use anyhow::Result;

use crate::agent::AgentSessionId;
use crate::space_ids::SpaceId;
use crate::space_names::SpaceSelector;
use crate::{
    config::permissions::PermissionChange,
    connection::{Daemon, request},
    model::{AgentSession, AgentStatus, PendingRestart, RunningAgent},
    protocol::{AgentMethod, ChatMethod, ExecutionMethod, TurnEnd, WakeEnd, WokenTurn},
    sessions::Origin,
    state::{AgentState, TurnFailure},
};

/// Record `state` for the agent of `workspace`, running `session`;
/// `turn_end` reports the turn that ended with it.
pub async fn set_state(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    state: AgentState,
    session: Option<AgentSessionId>,
    turn_end: Option<TurnEnd>,
) -> Result<AgentStatus> {
    let method = AgentMethod::SetAgentState {
        workspace,
        state,
        session,
        turn_end,
    };
    request(daemon, method).await
}

/// Record the agent of `workspace` working on the turn a wake starts.
/// Whether the turn starts: a stop of the agent that came first leaves the
/// update for its restore.
pub async fn start_woken_turn(daemon: &impl Daemon, workspace: SpaceSelector) -> Result<bool> {
    let turn: WokenTurn = request(daemon, AgentMethod::StartWokenTurn { workspace }).await?;
    Ok(turn.starts)
}

/// Record the turn of `workspace`'s agent, running `session`, that
/// `failure` ended.
pub async fn report_turn_failure(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    failure: TurnFailure,
    session: Option<AgentSessionId>,
    turn_end: TurnEnd,
) -> Result<AgentStatus> {
    let method = AgentMethod::ReportTurnFailure {
        workspace,
        failure,
        session,
        execution: None,
        turn_end,
    };
    request(daemon, method).await
}

/// Tell the user that a report from `workspace` failed with `error`.
pub async fn report_state_failure(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    error: String,
) -> Result<()> {
    request(daemon, AgentMethod::ReportStateFailure { workspace, error }).await
}

/// Tell the daemon that the wake hook of `space`'s agent ended for `ended`
/// without starting the agent's turn.
pub async fn report_wake_end(
    daemon: &impl Daemon,
    space: SpaceSelector,
    ended: WakeEnd,
) -> Result<()> {
    request(daemon, AgentMethod::ReportWakeEnd { space, ended }).await
}

/// Record that the interrupt key typed into `execution` of `workspace`
/// ended its agent's turn.
pub async fn interrupt_turn(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    execution: ExecutionId,
) -> Result<AgentStatus> {
    request(
        daemon,
        AgentMethod::InterruptTurn {
            workspace,
            execution,
        },
    )
    .await
}

/// The agents connected in every workspace.
pub async fn all(daemon: &impl Daemon) -> Result<Vec<RunningAgent>> {
    request(daemon, ExecutionMethod::ListAgents).await
}

/// Restart the agent of `execution` in a new wrapper, or `in_place`, as
/// part of the restart `run`, or join the restart `join` already pending,
/// making `change` to the permissions it was given. Returns the
/// place of the wrapper that runs the agent again, when that runs in a
/// terminal host.
pub async fn restart(
    daemon: &impl Daemon,
    execution: ExecutionId,
    join: Option<PendingRestart>,
    run: Option<String>,
    in_place: bool,
    change: Option<PermissionChange>,
) -> Result<Option<Origin>> {
    request(
        daemon,
        ExecutionMethod::RestartAgent {
            execution,
            join,
            run,
            in_place,
            change,
        },
    )
    .await
}

/// The agents of the running chats, and of those a restart stopped to start
/// again, each listed by its chat's ID as its execution and space.
pub async fn of_chats(daemon: &impl Daemon) -> Result<Vec<RunningAgent>> {
    request(daemon, ChatMethod::ChatAgents).await
}

/// Restart the agent of the running chat `chat` in a background place, as
/// part of the restart `run`, or join the restart `join` already pending,
/// making `change` to the permissions its agent was given. Returns the
/// place the chat runs in again, when it runs in a terminal host.
pub async fn restart_chat(
    daemon: &impl Daemon,
    chat: &SpaceId,
    join: Option<PendingRestart>,
    run: Option<String>,
    change: Option<PermissionChange>,
) -> Result<Option<Origin>> {
    let method = ChatMethod::RestartChat {
        chat: chat.clone(),
        join,
        run,
        change,
    };
    request(daemon, method).await
}

/// The agents connected in `workspace`.
pub async fn running(daemon: &impl Daemon, workspace: SpaceSelector) -> Result<Vec<RunningAgent>> {
    request(daemon, AgentMethod::WorkspaceAgents { workspace }).await
}

/// The session of the agent in `workspace`, which a fork starts from.
pub async fn session(daemon: &impl Daemon, workspace: SpaceSelector) -> Result<AgentSession> {
    request(daemon, AgentMethod::AgentSession { workspace }).await
}
