//! The chat start worker tests drive: it records each resolved start, and
//! whether the worker acts with a scope, and reports it started in a
//! background place, unless a test made the place of its name fail or an
//! app run its agent detached.
use anyhow::Result;
use shoal_protocol::{
    launches::{DetachedRun, Started},
    scope_tokens::ScopeToken,
    space_names::SpaceName,
};
use std::sync::Mutex;

use crate::protocol::ChatStart;
use shoal_core::chat_handoff::HandedStart;

/// The resolved starts the daemon ran its chat start worker for, each with
/// whether the worker acts with the caller's scope.
pub static STARTED: Mutex<Vec<(ChatStart, bool)>> = Mutex::new(Vec::new());

/// The names of chats whose place fails before it starts their agent.
pub static FAILING: Mutex<Vec<SpaceName>> = Mutex::new(Vec::new());

/// The names of chats whose agent an app runs detached.
pub static DETACHED: Mutex<Vec<SpaceName>> = Mutex::new(Vec::new());

pub(super) fn start(handed: &HandedStart, token: Option<&ScopeToken>) -> Result<Started> {
    let start = &handed.start;
    let scoped = token.is_some();
    STARTED.lock().unwrap().push((start.clone(), scoped));
    let failing = FAILING.lock().unwrap();
    if let Some(name) = (start.name.as_ref()).filter(|name| failing.contains(name)) {
        anyhow::bail!("the place of {name} ended before the chat started");
    }
    let detached = DETACHED.lock().unwrap();
    if let Some(name) = (start.name.as_ref()).filter(|name| detached.contains(name)) {
        return Ok(Started::Detached(DetachedRun {
            execution_id: name.as_str().into(),
            pid: 1,
            log: format!("{name}.log").into(),
            delivery: Default::default(),
        }));
    }
    Ok(Started::Background {
        place: "background".into(),
    })
}
