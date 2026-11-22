//! Tracked executions: what they run, where their executable came from, the
//! agents Shoal stopped or that exited with a session to resume, and the
//! retries of their failed turns.
use crate::connection::{Daemon, Prepared, request};
use shoal_protocol::{
    executions::ExecutionKind, processes::Identity, protocol::ExecutionMethod,
    space_names::SpaceSelector,
};

/// Refuse a caller other than a merge execution the user authorized.
pub async fn check_merge(daemon: &impl Daemon) -> anyhow::Result<()> {
    request(daemon, ExecutionMethod::CheckMerge).await
}

/// Refuse a caller other than an unmerge execution the user authorized.
pub async fn check_unmerge(daemon: &impl Daemon) -> anyhow::Result<()> {
    request(daemon, ExecutionMethod::CheckUnmerge).await
}

/// Tell the daemon that `signal` ends the session of the execution whose
/// scope `daemon` carries, once it ended what the command runs elsewhere.
pub async fn report_session_signal(
    daemon: &impl Daemon,
    signal: shoal_protocol::protocol::SessionSignal,
) -> anyhow::Result<()> {
    request(daemon, ExecutionMethod::SessionSignalled { signal }).await
}

/// A command's run in `space`, as a picker that chooses the space judges
/// it. It has no answer to read, since it is never sent: the command's
/// wrapper registers its run over its own connection, with its identity.
pub fn command_run(space: SpaceSelector) -> Prepared<std::convert::Infallible> {
    let wrapper = Identity {
        pid: 0,
        birth: String::new(),
        unique: None,
    };
    Prepared::new(ExecutionMethod::Execute {
        space,
        wrapper,
        kind: ExecutionKind::Command,
        agent: None,
        model: Default::default(),
        mode: None,
        narrowing: Default::default(),
        new_session: false,
        recover: false,
        source: None,
        reattach: false,
        queue_entry: None,
        demo: None,
        origin: None,
        merge: None,
    })
}
