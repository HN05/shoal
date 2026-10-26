//! `shoal internal agent-state` and `turn-failed`: the turn state, and the
//! failure that ended a turn, that an agent's hooks report for its workspace.
use std::{
    io::{IsTerminal, Read},
    os::fd::{AsFd, AsRawFd, BorrowedFd},
    time::{Duration, Instant},
};

use anyhow::Result;

use crate::{
    cli::{
        client::request,
        context::Context,
        ui::{self, Fallback},
    },
    execution::recovery,
    model::AgentStatus,
    protocol::AgentMethod,
    state::{AgentState, TurnFailure},
};

pub(super) async fn run(
    ctx: &Context,
    workspace: Option<String>,
    state: AgentState,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::OwnWorkspace).await?;
    let session = hook_event().as_ref().and_then(session_of);
    let status: AgentStatus = request(
        &ctx.paths,
        AgentMethod::SetAgentState {
            workspace,
            state,
            session,
        },
    )
    .await?;
    emit(ctx, status)
}

/// Report the turn that the hook event of an agent with `integration`
/// says an API error ended, which leaves the agent idle. An event the
/// integration cannot read reports an unknown failure.
pub(super) async fn turn_failed(
    ctx: &Context,
    workspace: Option<String>,
    integration: crate::ai::IntegrationKind,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::OwnWorkspace).await?;
    let event = hook_event().unwrap_or_default();
    let failure =
        crate::cli::agents::turn_failure(integration, &event).unwrap_or(TurnFailure::Other);
    let status: AgentStatus = request(
        &ctx.paths,
        AgentMethod::ReportTurnFailure {
            workspace,
            failure,
            session: session_of(&event),
        },
    )
    .await?;
    emit(ctx, status)
}

fn emit(ctx: &Context, status: AgentStatus) -> Result<i32> {
    ctx.emit(
        &format!("Agent state: {}", ui::agent_state_label(status.state)),
        status,
    )?;
    Ok(0)
}

/// How long stdin may stay silent before the command stops waiting for a hook
/// event: hooks receive theirs before they start, while a caller that
/// inherited an open pipe or socket may never write or close it.
const EVENT_WAIT: Duration = Duration::from_secs(2);

/// The event Claude Code and Codex pass their hooks on stdin; a terminal, a
/// silent input or another input reports none.
fn hook_event() -> Option<serde_json::Value> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() || !readable_within(stdin.as_fd(), EVENT_WAIT) {
        return None;
    }
    first_value(stdin.lock())
}

/// Whether `fd` has input or has reached its end within `wait`.
fn readable_within(fd: BorrowedFd<'_>, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout = deadline.saturating_duration_since(Instant::now());
        let timeout = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
        match unsafe { libc::poll(&mut poll, 1, timeout) } {
            -1 if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => {}
            ready => return ready > 0,
        }
    }
}

/// The first JSON value in `input`, read without waiting for it to close.
fn first_value(input: impl Read) -> Option<serde_json::Value> {
    serde_json::Deserializer::from_reader(input)
        .into_iter()
        .next()?
        .ok()
}

/// The agent session a hook event names.
fn session_of(event: &serde_json::Value) -> Option<String> {
    event["session_id"]
        .as_str()
        .filter(|session| recovery::valid_session(session))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests;
