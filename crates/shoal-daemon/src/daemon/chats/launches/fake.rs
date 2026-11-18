//! The chat start worker tests drive: it records each resolved start, and
//! whether the worker acts with a scope, and reports it started in a
//! background place, unless a test made the place of its name fail or an
//! app run its agent detached; each for the daemon that ran it.
use anyhow::Result;
use shoal_protocol::{
    launches::{DetachedRun, Started},
    scope_tokens::ScopeToken,
    space_names::SpaceName,
};

use crate::{daemon::per_daemon::PerDaemon, paths::Paths, protocol::ChatStart};
use shoal_core::chat_handoff::HandedStart;

static STARTED: PerDaemon<(ChatStart, bool)> = PerDaemon::new();

static FAILING: PerDaemon<SpaceName> = PerDaemon::new();

static DETACHED: PerDaemon<SpaceName> = PerDaemon::new();

/// The resolved starts the daemon of `paths` ran its chat start worker
/// for, each with whether the worker acts with the caller's scope.
pub fn started(paths: &Paths) -> Vec<(ChatStart, bool)> {
    STARTED.of(paths)
}

/// Make the place of the daemon's chat `name` fail before it starts its
/// agent.
#[cfg(test)]
pub(crate) fn fail(paths: &Paths, name: SpaceName) {
    FAILING.push(paths, name);
}

/// Make an app run the agent of the daemon's chat `name` detached.
#[cfg(test)]
pub(crate) fn detach(paths: &Paths, name: SpaceName) {
    DETACHED.push(paths, name);
}

pub(super) fn start(
    paths: &Paths,
    handed: &HandedStart,
    token: Option<&ScopeToken>,
) -> Result<Started> {
    let start = &handed.start;
    let scoped = token.is_some();
    STARTED.push(paths, (start.clone(), scoped));
    if let Some(name) = (start.name.as_ref()).filter(|name| FAILING.contains(paths, name)) {
        anyhow::bail!("the place of {name} ended before the chat started");
    }
    if let Some(name) = (start.name.as_ref()).filter(|name| DETACHED.contains(paths, name)) {
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
