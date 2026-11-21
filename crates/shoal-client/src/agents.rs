//! The agents running in a space: the state and turns their hooks report,
//! the user's interrupts, and the sessions they run.
use anyhow::Result;
use shoal_protocol::agents::GitCredential;

use shoal_ids::execution_ids::ExecutionId;

use crate::connection::{Daemon, request};
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    model::{AgentRestart, AgentSession, AgentStatus, RestartOutcome, RunningAgent},
    protocol::{AgentMethod, SubagentChange, TurnEnd, WakeEnd, WokenTurn},
    state::{AgentState, TurnFailure},
};

/// Record `state` for the agent of `workspace`, running `session`, as a
/// hook `made` it; `turn_end` reports the turn that ended with it.
pub async fn set_state(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    state: AgentState,
    session: Option<AgentSessionId>,
    turn_end: Option<TurnEnd>,
    made: Option<std::time::SystemTime>,
) -> Result<AgentStatus> {
    let method = AgentMethod::SetAgentState {
        workspace,
        state,
        session,
        turn_end,
        made,
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
/// `failure` ended, as a hook `made` the report.
pub async fn report_turn_failure(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    failure: TurnFailure,
    session: Option<AgentSessionId>,
    turn_end: TurnEnd,
    made: Option<std::time::SystemTime>,
) -> Result<AgentStatus> {
    let method = AgentMethod::ReportTurnFailure {
        workspace,
        failure,
        session,
        execution: None,
        turn_end,
        made,
    };
    request(daemon, method).await
}

/// Record `change` to the subagents of `space`'s agent, as its hooks
/// report it.
pub async fn report_subagent(
    daemon: &impl Daemon,
    space: SpaceSelector,
    change: SubagentChange,
) -> Result<()> {
    request(daemon, AgentMethod::ReportSubagent { space, change }).await
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

/// Record that the interrupt key typed into `execution` of `space` ended
/// its agent's turn.
pub async fn interrupt_turn(
    daemon: &impl Daemon,
    space: SpaceSelector,
    execution: ExecutionId,
) -> Result<AgentStatus> {
    request(daemon, AgentMethod::InterruptTurn { space, execution }).await
}

/// Tell the daemon that Shoal types into the terminal of `execution` of
/// `space` now, so the keys its agent reads next are not the user's.
pub async fn report_typing(
    daemon: &impl Daemon,
    space: SpaceSelector,
    execution: ExecutionId,
) -> Result<()> {
    let method = AgentMethod::ReportTyping {
        workspace: space,
        execution,
    };
    request(daemon, method).await
}

/// Tell the daemon that the caller attached to the sessions of `space`.
pub async fn report_attach(daemon: &impl Daemon, space: SpaceSelector) -> Result<()> {
    request(daemon, AgentMethod::ReportAttach { space }).await
}

/// The agents connected in the spaces the caller reaches.
pub async fn all(daemon: &impl Daemon) -> Result<Vec<RunningAgent>> {
    request(daemon, AgentMethod::ListAgents).await
}

/// Restart the agent of `space` as `restart` says, answering with what the
/// daemon did: restarted it, took the restart for later or skipped it.
pub async fn restart(
    daemon: &impl Daemon,
    space: SpaceSelector,
    restart: AgentRestart,
) -> Result<RestartOutcome> {
    request(daemon, AgentMethod::RestartAgent { space, restart }).await
}

/// Drop the restart the daemon recorded for the agent of `space`, returning
/// how it would have restarted, if anything was recorded.
pub async fn cancel_restart(
    daemon: &impl Daemon,
    space: SpaceSelector,
) -> Result<Option<AgentRestart>> {
    request(daemon, AgentMethod::CancelRestart { space }).await
}

/// The agents connected in `space`, a workspace or a running chat.
pub async fn running(daemon: &impl Daemon, space: SpaceSelector) -> Result<Vec<RunningAgent>> {
    request(daemon, AgentMethod::SpaceAgents { space }).await
}

/// The caller's login for Git over HTTPS to `host`, or none.
pub async fn git_credential(daemon: &impl Daemon, host: String) -> Result<Option<GitCredential>> {
    request(daemon, AgentMethod::GitCredential { host }).await
}

/// The session of the agent in `space`, which a fork starts from.
pub async fn session(daemon: &impl Daemon, space: SpaceSelector) -> Result<AgentSession> {
    request(daemon, AgentMethod::AgentSession { space }).await
}
