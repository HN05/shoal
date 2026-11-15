//! What settling a start's workspace reads, asked of the daemon.
use anyhow::Result;

use crate::{
    cli::context::Context, client::workspaces::PlannedWorkspace, launches::workspace::LaunchReads,
    model::Workspace,
};

/// The daemon's state as this process reads it.
pub(in crate::cli::commands) struct DaemonReads<'a>(pub &'a Context);

impl LaunchReads for DaemonReads<'_> {
    async fn workspaces(&self) -> Result<Vec<Workspace>> {
        crate::client::workspaces::list(&self.0.paths).await
    }

    async fn check_creation(&self, planned: PlannedWorkspace) -> Result<()> {
        crate::client::workspaces::check_creation(&self.0.paths, planned).await
    }
}
