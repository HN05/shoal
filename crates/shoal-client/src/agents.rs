//! The agents running in a space: the state and turns their hooks report,
//! the user's interrupts, and the sessions they run.
use crate::execution_ids::ExecutionId;
use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::agent::AgentSessionId;
use crate::space_names::SpaceSelector;
use crate::{
    config::permissions::PermissionChange,
    connection::{Daemon, request},
    model::{AgentSession, AgentStatus, PendingRestart, RunningAgent},
    protocol::{AgentMethod, ExecutionMethod, TurnEnd, WakeEnd, WokenTurn},
    sessions::Origin,
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

/// The agents connected in `space`, a workspace or a running chat.
pub async fn running(daemon: &impl Daemon, space: SpaceSelector) -> Result<Vec<RunningAgent>> {
    request(daemon, AgentMethod::SpaceAgents { space }).await
}

/// A login Git sends over HTTPS. Its `Debug` leaves out the password.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCredential {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for GitCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GitCredential")
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// The caller's login for Git over HTTPS to `host`, or none.
pub async fn git_credential(daemon: &impl Daemon, host: String) -> Result<Option<GitCredential>> {
    request(daemon, AgentMethod::GitCredential { host }).await
}

/// The session of the agent in `workspace`, which a fork starts from.
pub async fn session(daemon: &impl Daemon, workspace: SpaceSelector) -> Result<AgentSession> {
    request(daemon, AgentMethod::AgentSession { workspace }).await
}
