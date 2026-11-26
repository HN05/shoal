//! `shoal internal agent-state` and `turn-failed`.
use std::time::SystemTime;

use anyhow::Result;
use shoal_core::execution::recovery;
use shoal_ids::{agent_session_ids::AgentSessionId, execution_ids::ExecutionId};
use shoal_protocol::{
    executions::FailedTurn, list_status::ListStatus, space_names::SpaceSelector, validate,
};

use crate::{
    agents::{
        hook_reports::{Report, Sending},
        integration,
    },
    ai::IntegrationKind,
    client::{agents, connection::CommandPaths, output},
    model::AgentStatus,
    protocol::TurnEnd,
    state::{AgentState, TurnFailure},
    time::UnixTime,
};

/// Report `state` for the agent of the space `space` names, else the one
/// this runs in, which ends a turn of an agent with integration `ends_turn`
/// when one is named. `json` says whether to print the status as JSON.
pub async fn report_state(
    paths: &CommandPaths,
    json: bool,
    space: Option<SpaceSelector>,
    (state, ends_turn): (AgentState, Option<IntegrationKind>),
) -> Result<i32> {
    let event = super::input::event();
    let report = Report::State {
        state,
        session: event.as_ref().and_then(session_of),
        turn_end: ends_turn.map(|integration| TurnEnd {
            final_message: event.as_ref().and_then(final_message),
            ..turn_end(paths, integration)
        }),
    };
    print(json, send_kept(paths, space, report).await?)
}

/// Report the turn that the hook event of an agent with `integration`
/// says an API error ended, which leaves the agent idle, with the reset its
/// error states. An event the integration cannot read reports an unknown
/// failure.
pub async fn report_failure(
    paths: &CommandPaths,
    json: bool,
    space: Option<SpaceSelector>,
    integration: IntegrationKind,
) -> Result<i32> {
    let event = super::input::event().unwrap_or_default();
    let integration_of = self::integration(integration);
    let report = Report::TurnFailure {
        failure: integration_of
            .turn_failure(&event)
            .unwrap_or(TurnFailure::Other),
        resets_at: integration_of.stated_reset(&event, UnixTime::now()),
        session: session_of(&event),
        turn_end: turn_end(paths, integration),
    };
    print(json, send_kept(paths, space, report).await?)
}

/// Send `report` for the agent of the space `space` selects, kept on disk
/// while it is sent, so that one the daemon does not receive stays for the
/// daemon to find and tell the user about, even when the hook's time limit
/// ends this process first.
async fn send_kept(
    paths: &CommandPaths,
    space: Option<SpaceSelector>,
    report: Report,
) -> Result<AgentStatus> {
    let kept = keep(paths, space.is_none(), &report);
    let made = kept.as_ref().map_or_else(SystemTime::now, Sending::made);
    let sent = send(paths, space, report, made).await;
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
fn keep(paths: &CommandPaths, own: bool, report: &Report) -> Option<Sending> {
    let space = crate::env::own_space().filter(|_| own && crate::env::is_scoped())?;
    let execution = std::env::var(crate::env::EXECUTION_ID)
        .ok()
        .filter(|execution| !execution.is_empty())
        .map(ExecutionId::from);
    Sending::keep(paths, &space, report.clone(), execution).ok()
}

/// Send `report`, which a hook `made`, for the agent of the space `space`
/// selects.
async fn send(
    paths: &CommandPaths,
    space: Option<SpaceSelector>,
    report: Report,
    made: SystemTime,
) -> Result<AgentStatus> {
    let space = super::own_space(paths, space).await?;
    let made = Some(made);
    match report {
        Report::State {
            state,
            session,
            turn_end,
        } => agents::set_state(paths, space, state, session, turn_end, made).await,
        Report::TurnFailure {
            failure,
            resets_at,
            session,
            turn_end,
        } => {
            let failed = FailedTurn { failure, resets_at };
            agents::report_turn_failure(paths, space, failed, session, turn_end, made).await
        }
    }
}

/// What the hook of an agent with `integration` reports when its turn
/// ends: how long its prompt cache lives, as the environment the hook runs
/// in gives it.
fn turn_end(paths: &CommandPaths, integration: IntegrationKind) -> TurnEnd {
    let variable = |name: &str| std::env::var_os(name);
    let lifetime = crate::agents::prompt_cache(integration, &variable, &paths.home);
    TurnEnd {
        prompt_cache_secs: lifetime.map(|lifetime| lifetime.as_secs()),
        final_message: None,
    }
}

fn print(json: bool, status: AgentStatus) -> Result<i32> {
    let label = ListStatus::agent(status.state).label();
    let message = format!("Agent state: {label}");
    shoal_core::outln!("{}", output::line(json, &message, &status)?);
    Ok(0)
}

/// The final message of the turn a `Stop` hook event ends, as Claude Code
/// and Codex pass it, cut past the longest question the daemon keeps so the
/// report stays small and the daemon still sees that it was cut.
fn final_message(event: &serde_json::Value) -> Option<String> {
    let message = event["last_assistant_message"].as_str()?;
    let mut end = message.len().min(validate::MAX_AGENT_MESSAGE_LEN + 1);
    while !message.is_char_boundary(end) {
        end += 1;
    }
    Some(message[..end].to_owned())
}

/// The agent session a hook event names.
fn session_of(event: &serde_json::Value) -> Option<AgentSessionId> {
    event["session_id"]
        .as_str()
        .filter(|session| recovery::valid_session(session))
        .map(|session| session.to_owned().into())
}

#[cfg(test)]
mod tests;
