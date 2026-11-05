//! The agents running in a space: the state and turns their hooks report,
//! the user's interrupts, and the sessions they run.
use anyhow::Result;

use crate::{
    client::connection::{Daemon, request},
    model::{AgentSession, AgentStatus, PendingRestart, RunningAgent},
    protocol::{AgentMethod, ExecutionMethod, TurnEnd, WokenTurn},
    sessions::Origin,
    state::{AgentState, TurnFailure},
};

/// Record `state` for the agent of `workspace`, running `session`;
/// `turn_end` reports the turn that ended with it.
pub async fn set_state(
    daemon: &impl Daemon,
    workspace: String,
    state: AgentState,
    session: Option<String>,
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
pub async fn start_woken_turn(daemon: &impl Daemon, workspace: String) -> Result<bool> {
    let turn: WokenTurn = request(daemon, AgentMethod::StartWokenTurn { workspace }).await?;
    Ok(turn.starts)
}

/// Record the turn of `workspace`'s agent, running `session`, that
/// `failure` ended.
pub async fn report_turn_failure(
    daemon: &impl Daemon,
    workspace: String,
    failure: TurnFailure,
    session: Option<String>,
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
    workspace: String,
    error: String,
) -> Result<()> {
    request(daemon, AgentMethod::ReportStateFailure { workspace, error }).await
}

/// Record that the interrupt key typed into `execution` of `workspace`
/// ended its agent's turn.
pub async fn interrupt_turn(
    daemon: &impl Daemon,
    workspace: String,
    execution: String,
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
/// part of the restart `run`, or join the restart `join` already pending.
/// Returns the place of the wrapper that runs the agent again, when that
/// runs in a terminal host.
pub async fn restart(
    daemon: &impl Daemon,
    execution: String,
    join: Option<PendingRestart>,
    run: Option<String>,
    in_place: bool,
) -> Result<Option<Origin>> {
    request(
        daemon,
        ExecutionMethod::RestartAgent {
            execution,
            join,
            run,
            in_place,
        },
    )
    .await
}

/// The agents connected in `workspace`.
pub async fn running(daemon: &impl Daemon, workspace: String) -> Result<Vec<RunningAgent>> {
    request(daemon, AgentMethod::WorkspaceAgents { workspace }).await
}

/// The session of the agent in `workspace`, which a fork starts from.
pub async fn session(daemon: &impl Daemon, workspace: String) -> Result<AgentSession> {
    request(daemon, AgentMethod::AgentSession { workspace }).await
}
