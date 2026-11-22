//! Shoal's daemon, which the `shoal` command runs as `shoal daemon run`.

mod daemon;

#[cfg(test)]
use shoal_core::reviews;
#[cfg(any(test, feature = "test-support"))]
use shoal_core::test_support;
use shoal_core::{
    agents, caches, config, config_file, daemon_handoff, daemon_owner, env, execution,
    fetched_copies, fsutil, git, git_profile, hooks, installed, launches, paths, presets, process,
    profile, refusal, releases, removal, repository, service, sessions, shoal_dirs, start_plan,
    terminal, tmp_dirs, tools, transfer, workers,
};

use shoal_forge_types as forge;
use shoal_protocol::time;
use shoal_protocol::{agent, demos, model, protocol, state, validate};

/// A daemon on the socket of a temporary state directory, which other
/// crates' tests connect to the way the CLI and apps do, with the fixtures
/// they read.
#[cfg(feature = "test-support")]
pub mod test_daemon {
    pub use crate::daemon::approvals::ApprovalRequestExt;
    pub use crate::daemon::capacity::fake_disk_free;
    pub use crate::daemon::disk::Monitor as DiskMonitor;
    pub use crate::daemon::links::proposals::{link_item, link_space, store_issue_proposal};
    pub use crate::daemon::operations::fake;
    pub use crate::daemon::overload::Pressure;
    pub use crate::daemon::queued_starts::fake::STARTED as STARTED_QUEUED;
    pub use crate::daemon::scope::Caller;
    pub use crate::daemon::test_daemon::{HANG, TestDaemon};
    pub use crate::daemon::test_support::{chat_launches, configure, overload};
    pub use crate::daemon::{
        executions::StopRequest, manager::Manager, stopped_work::PermissionTargets,
    };
}

/// Run the daemon in this process, given its state, whether its service
/// manager started it and the handoff from the daemon it replaces. The future
/// is boxed here, where the daemon's futures are laid out: laying them out in
/// another crate, as the `shoal` binary would, exceeds rustc's query depth
/// limit.
pub fn run(
    paths: paths::Paths,
    managed: bool,
    handoff: Option<daemon_handoff::Handoff>,
) -> futures_util::future::LocalBoxFuture<'static, anyhow::Result<()>> {
    Box::pin(daemon::run(paths, managed, handoff))
}

#[cfg(test)]
mod helper_tests {
    shoal_core::helper_tests!();
}
#[cfg(test)]
shoal_core::prepare_test_process!();
