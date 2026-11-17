//! The chat start worker tests drive: it records each resolved start, and
//! whether the worker acts with a scope, and reports it started in a
//! background place, unless a test made the place of its name fail.
use anyhow::{Context as _, Result};
use shoal_protocol::{chats::StartedChat, scope_tokens::ScopeToken, space_names::SpaceName};
use std::sync::Mutex;

use crate::protocol::ChatStart;

/// The resolved starts the daemon ran its chat start worker for, each with
/// whether the worker acts with the caller's scope.
pub static STARTED: Mutex<Vec<(ChatStart, bool)>> = Mutex::new(Vec::new());

/// The names of chats whose place fails before it starts their agent.
pub static FAILING: Mutex<Vec<SpaceName>> = Mutex::new(Vec::new());

pub(super) fn start(start: &ChatStart, token: Option<&ScopeToken>) -> Result<StartedChat> {
    let scoped = token.is_some();
    STARTED.lock().unwrap().push((start.clone(), scoped));
    let failing = FAILING.lock().unwrap();
    if let Some(name) = (start.name.as_ref()).filter(|name| failing.contains(name)) {
        anyhow::bail!("the place of {name} ended before the chat started");
    }
    Ok(StartedChat::Background(
        shoal_protocol::chats::BackgroundChat {
            id: None,
            chat: start.name.clone().unwrap_or_else(|| "chat".into()),
            agent: start.agent.clone().context("the start names no agent")?,
            repository: None,
            place: "background".into(),
            warnings: Vec::new(),
        },
    ))
}
