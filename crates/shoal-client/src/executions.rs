//! Tracked executions: what they run, where their executable came from, the
//! agents Shoal stopped or that exited with a session to resume, and the
//! retries of their failed turns.
use crate::connection::{Daemon, request};
use shoal_protocol::protocol::ExecutionMethod;

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
