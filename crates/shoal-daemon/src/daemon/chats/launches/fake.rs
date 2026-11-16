//! The chat start worker tests drive: it records each resolved start, and
//! whether the worker acts with a scope, and reports it started in a
//! background place.
use anyhow::{Context as _, Result};
use shoal_protocol::{chats::StartedChat, scope_tokens::ScopeToken};
use std::sync::Mutex;

use crate::protocol::ChatStart;

/// The resolved starts the daemon ran its chat start worker for, each with
/// whether the worker acts with the caller's scope.
pub static STARTED: Mutex<Vec<(ChatStart, bool)>> = Mutex::new(Vec::new());

pub(super) fn start(start: &ChatStart, token: Option<&ScopeToken>) -> Result<StartedChat> {
    let scoped = token.is_some();
    STARTED.lock().unwrap().push((start.clone(), scoped));
    Ok(StartedChat::Background(
        shoal_protocol::chats::BackgroundChat {
            id: None,
            chat: start.name.clone().unwrap_or_else(|| "chat".into()),
            agent: start.agent.clone().context("the start names no agent")?,
            repository: None,
            place: "background".into(),
        },
    ))
}
