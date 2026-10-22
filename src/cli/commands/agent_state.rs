//! `shoal internal agent-state`: the turn state an agent's hooks report for its workspace.
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
    protocol::Method,
    state::AgentState,
};

pub(super) async fn run(
    ctx: &Context,
    workspace: Option<String>,
    state: AgentState,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::OwnWorkspace).await?;
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

/// How long stdin may stay silent before the command stops waiting for a hook
/// event: hooks receive theirs before they start, while a caller that
/// inherited an open pipe or socket may never write or close it.
const EVENT_WAIT: Duration = Duration::from_secs(2);

/// The session ID in the event Claude Code and Codex pass their hooks on
/// stdin; a terminal, a silent input or another input reports none.
fn hook_session() -> Option<String> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() || !readable_within(stdin.as_fd(), EVENT_WAIT) {
        return None;
    }
    session_of(stdin.lock())
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

/// The session of the first JSON value in `event`, read without waiting for
/// the input to close.
fn session_of(event: impl Read) -> Option<String> {
    let event: serde_json::Value = serde_json::Deserializer::from_reader(event)
        .into_iter()
        .next()?
        .ok()?;
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
            session_of(&event[..]).as_deref(),
            Some("0b0e5f3c-7d2a-4c55-9f1e-1a2b3c4d5e6f")
        );
        assert_eq!(session_of(&br#"{"session_id":"--help"}"#[..]), None);
        assert_eq!(session_of(&br#"{"hook_event_name":"Stop"}"#[..]), None);
        assert_eq!(session_of(&b""[..]), None);
    }

    #[test]
    fn open_inputs_report_their_event_without_closing() -> std::io::Result<()> {
        use std::io::Write;

        let (reader, mut writer) = std::io::pipe()?;
        assert!(!readable_within(reader.as_fd(), Duration::from_millis(50)));
        writer.write_all(br#"{"session_id":"0b0e5f3c-7d2a-4c55-9f1e-1a2b3c4d5e6f"}"#)?;
        assert!(readable_within(reader.as_fd(), Duration::from_millis(50)));
        assert_eq!(
            session_of(&reader).as_deref(),
            Some("0b0e5f3c-7d2a-4c55-9f1e-1a2b3c4d5e6f")
        );
        drop(writer);
        assert!(readable_within(reader.as_fd(), Duration::from_millis(50)));
        Ok(())
    }
}
