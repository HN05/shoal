//! `shoal internal agent-state`: the turn state an agent's hooks report for its workspace.
use std::io::{IsTerminal, Read};

use anyhow::Result;

use crate::{
    cli::{
        client::request,
        context::Context,
        ui::{self, Fallback},
    },
    execution::recovery,
    model::AgentStatus,
    protocol::Method,
    state::AgentState,
};

pub(super) async fn run(
    ctx: &Context,
    workspace: Option<String>,
    state: AgentState,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let session = hook_session();
    let status: AgentStatus = request(
        &ctx.paths,
        Method::SetAgentState {
            workspace,
            state,
            session,
        },
    )
    .await?;
    ctx.emit(
        &format!("Agent state: {}", ui::agent_state_label(status.state)),
        status,
    )?;
    Ok(0)
}

/// The session ID in the event Claude Code and Codex pass their hooks on
/// stdin; a terminal or another input reports none.
fn hook_session() -> Option<String> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let mut event = Vec::new();
    stdin.read_to_end(&mut event).ok()?;
    session_of(&event)
}

fn session_of(event: &[u8]) -> Option<String> {
    let event: serde_json::Value = serde_json::from_slice(event).ok()?;
    event["session_id"]
        .as_str()
        .filter(|session| recovery::valid_session(session))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_events_name_their_session() {
        let event =
            br#"{"session_id":"0b0e5f3c-7d2a-4c55-9f1e-1a2b3c4d5e6f","hook_event_name":"Stop"}"#;
        assert_eq!(
            session_of(event).as_deref(),
            Some("0b0e5f3c-7d2a-4c55-9f1e-1a2b3c4d5e6f")
        );
        assert_eq!(session_of(br#"{"session_id":"--help"}"#), None);
        assert_eq!(session_of(br#"{"hook_event_name":"Stop"}"#), None);
        assert_eq!(session_of(b""), None);
    }
}
