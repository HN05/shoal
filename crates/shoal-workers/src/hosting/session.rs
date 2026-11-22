//! The session worker: what a host runs in the place it opens for a
//! tracked agent's session, which records the place as its space's, closes
//! it with a workspace when told to, and runs the agent.
use std::{ffi::OsString, path::PathBuf};

use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    executions::TrackedAgent, launches::LaunchSource, space_names::SpaceSelector,
};

use super::{Caller, close_here_when_done, record_owned_here};
use crate::sessions::HostName;

/// A tracked agent's session in the place a host opened for it.
pub struct Session {
    pub space: SpaceId,
    pub agent: TrackedAgent,
    /// The host that opened the place.
    pub host: HostName,
    /// The host whose place closes once the workspace is done, if it does.
    pub close_in: Option<HostName>,
    /// Recovery records of the agents it replaces, consumed once it starts.
    pub records: Vec<PathBuf>,
    /// Whether the user restored the agent by hand.
    pub manual: bool,
    pub command: Vec<OsString>,
}

/// Run `session` in the place this process runs in.
pub async fn run_session(caller: Caller<'_>, session: Session) -> Result<i32> {
    let Session {
        space,
        agent,
        host,
        close_in,
        records,
        manual,
        command,
    } = session;
    record_owned_here(caller, host, &space).await;
    if let Some(host) = close_in {
        close_here_when_done(caller, host, &space).await;
    }
    let selector = SpaceSelector::Id(space);
    if records.is_empty() {
        return crate::execution::run(caller.paths, selector, command, Some(agent)).await;
    }
    let source = manual.then_some(LaunchSource::Manual);
    crate::execution::run_recovery(caller.paths, selector, command, agent, (records, source)).await
}
