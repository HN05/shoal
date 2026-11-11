//! `shoal internal agent-state` and `turn-failed`: the turn state, and the
//! failure that ended a turn, that an agent's hooks report for its space.
use crate::client::execution_ids::ExecutionId;
use std::{
    io::{IsTerminal, Read},
    os::fd::{AsFd, AsRawFd, BorrowedFd},
    time::{Duration, Instant, SystemTime},
};

use anyhow::Result;

use crate::client::space_names::SpaceSelector;
use crate::{
    agents::hook_reports::{Report, Sending},
    ai::IntegrationKind,
    cli::{
        context::Context,
        ui::{self, Fallback},
    },
    client::agents,
    execution::recovery,
    model::AgentStatus,
    protocol::TurnEnd,
    state::{AgentState, TurnFailure},
};

/// Report `state` for the agent of the space this runs in, which ends a
/// turn of an agent with integration `ends_turn` when one is named.
pub(super) async fn run(
    ctx: &Context,
    space: Option<SpaceSelector>,
    state: AgentState,
    ends_turn: Option<IntegrationKind>,
) -> Result<i32> {
    let event = hook_event();
    let report = Report::State {
        state,
        session: event.as_ref().and_then(session_of),
        turn_end: ends_turn.map(|integration| TurnEnd {
            final_message: event.as_ref().and_then(final_message),
            ..turn_end(ctx, integration)
        }),
    };
    emit(ctx, send_kept(ctx, space, report).await?)
}

/// Report the agent of `space` working on the turn a wake starts, which no
/// hook of its own reports starting. Whether the turn starts: a stop of the
/// agent that came first leaves the update for its restore.
pub(super) async fn start_woken_turn(ctx: &Context, space: SpaceSelector) -> Result<bool> {
    agents::start_woken_turn(&ctx.paths, space.clone()).await
}

/// Report the turn that the hook event of an agent with `integration`
/// says an API error ended, which leaves the agent idle. An event the
/// integration cannot read reports an unknown failure.
pub(super) async fn turn_failed(
    ctx: &Context,
    space: Option<SpaceSelector>,
    integration: IntegrationKind,
) -> Result<i32> {
    let event = hook_event().unwrap_or_default();
    let report = Report::TurnFailure {
        failure: crate::cli::agents::turn_failure(integration, &event)
            .unwrap_or(TurnFailure::Other),
        session: session_of(&event),
        turn_end: turn_end(ctx, integration),
    };
    emit(ctx, send_kept(ctx, space, report).await?)
}

/// Send `report` for the agent of the space `space` selects, kept on disk
/// while it is sent, so that one the daemon does not receive stays for the
/// daemon to find and tell the user about, even when the hook's time limit
/// ends this process first.
async fn send_kept(
    ctx: &Context,
    space: Option<SpaceSelector>,
    report: Report,
) -> Result<AgentStatus> {
    let kept = keep(ctx, space.is_none(), &report);
    let made = kept.as_ref().map_or_else(SystemTime::now, Sending::made);
    let sent = send(ctx, space, report, made).await;
    if let Some(kept) = kept {
        match &sent {
            Ok(_) => kept.delivered(),
            Err(error) => kept.failed(format!("{error:#}")),
        }
    }
    sent
}

/// `report` kept on disk when the hook of a workspace process's own space,
/// as `own` says, makes it. One that cannot be kept is sent all the same.
fn keep(ctx: &Context, own: bool, report: &Report) -> Option<Sending> {
    let space = crate::env::own_space().filter(|_| own && crate::env::is_scoped())?;
    let execution = std::env::var(crate::env::EXECUTION_ID)
        .ok()
        .filter(|execution| !execution.is_empty())
        .map(ExecutionId::from);
    Sending::keep(&ctx.paths, &space, report.clone(), execution).ok()
}

/// Send `report`, which a hook `made`, for the agent of the space `space`
/// selects.
async fn send(
    ctx: &Context,
    space: Option<SpaceSelector>,
    report: Report,
    made: SystemTime,
) -> Result<AgentStatus> {
    let space = ui::select_space(ctx, space, Fallback::OwnWorkspace).await?;
    let made = Some(made);
    match report {
        Report::State {
            state,
            session,
            turn_end,
        } => agents::set_state(&ctx.paths, space, state, session, turn_end, made).await,
        Report::TurnFailure {
            failure,
            session,
            turn_end,
        } => agents::report_turn_failure(&ctx.paths, space, failure, session, turn_end, made).await,
    }
}

/// What the hook of an agent with `integration` reports when its turn
/// ends: how long its prompt cache lives, as the environment the hook runs
/// in gives it.
fn turn_end(ctx: &Context, integration: IntegrationKind) -> TurnEnd {
    let variable = |name: &str| std::env::var_os(name);
    let lifetime = crate::agents::prompt_cache(integration, &variable, &ctx.paths.home);
    TurnEnd {
        prompt_cache_secs: lifetime.map(|lifetime| lifetime.as_secs()),
        final_message: None,
    }
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
pub(super) fn hook_event() -> Option<serde_json::Value> {
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

/// The final message of the turn a `Stop` hook event ends, as Claude Code
/// and Codex pass it, cut past the longest question the daemon keeps so the
/// report stays small and the daemon still sees that it was cut.
fn final_message(event: &serde_json::Value) -> Option<String> {
    let message = event["last_assistant_message"].as_str()?;
    let mut end = message
        .len()
        .min(crate::validate::MAX_AGENT_MESSAGE_LEN + 1);
    while !message.is_char_boundary(end) {
        end += 1;
    }
    Some(message[..end].to_owned())
}

/// The agent session a hook event names.
fn session_of(event: &serde_json::Value) -> Option<crate::client::agent::AgentSessionId> {
    event["session_id"]
        .as_str()
        .filter(|session| recovery::valid_session(session))
        .map(|session| session.to_owned().into())
}

#[cfg(test)]
mod tests;
