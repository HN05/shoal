//! Shoal's daemon, which the `shoal` command runs as `shoal daemon run`.

mod daemon;

#[cfg(any(test, feature = "test-support"))]
use shoal_core::env;
#[cfg(test)]
use shoal_core::reviews;
#[cfg(any(test, feature = "test-support"))]
use shoal_core::start_plan;
#[cfg(any(test, feature = "test-support"))]
use shoal_core::test_support;
#[cfg(test)]
use shoal_core::{agents, launches, presets, tmp_dirs};
use shoal_core::{
    caches, config, config_file, daemon_handoff, executable_link, execution, fsutil, git, hooks,
    installed, paths, process, profile, refusal, releases, removal, repository, service, sessions,
    shoal_dirs, terminal, tools, transfer, workers,
};
use shoal_forge_types as forge;
use shoal_protocol::time;
#[cfg(test)]
use shoal_protocol::validate;
use shoal_protocol::{agent, demos, model, protocol, state};

/// A daemon on the socket of a temporary state directory, which other
/// crates' tests connect to the way the CLI and apps do, with the fixtures
/// they read.
#[cfg(feature = "test-support")]
pub mod test_daemon {
    pub use crate::daemon::approvals::ApprovalRequestExt;
    pub use crate::daemon::capacity::fake_disk_free;
    pub use crate::daemon::disk::Monitor as DiskMonitor;
    pub use crate::daemon::links::proposals::{link_item, link_space, store_issue_proposal};
    pub use crate::daemon::overload::Pressure;
    pub use crate::daemon::queued_starts::fake::STARTED as STARTED_QUEUED;
    pub use crate::daemon::scope::{Access, Caller};
    pub use crate::daemon::test_daemon::{HANG, TestDaemon};
    pub use crate::daemon::test_support::{chat_launches, configure, overload};
    pub use crate::daemon::{
        executions::StopRequest, manager::Manager, stopped_work::PermissionTargets,
    };
    pub use shoal_daemon_base::operations::fake;
}

/// Run the daemon in this process, given its state, whether its service
/// manager started it and the handoff from the daemon it replaces.
pub async fn run(
    paths: paths::Paths,
    managed: bool,
    handoff: Option<daemon_handoff::Handoff>,
) -> anyhow::Result<()> {
    daemon::run(paths, managed, handoff).await
}

#[cfg(test)]
mod helper_tests {
    shoal_core::helper_tests!();
}
#[cfg(test)]
shoal_core::prepare_test_process!();
