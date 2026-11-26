use anyhow::{Context as _, Result};

use crate::protocol::operations::StartedWorkspace;
use shoal_protocol::protocol::{Launched, Started};

/// A new or reopened workspace comes with its preparation; an existing one
/// whose worker the launch queued has nothing left to prepare.
pub(in crate::cli::commands) fn workspace_start(launched: Launched) -> Result<StartedWorkspace> {
    let Launched {
        space,
        entry,
        warnings,
        start,
        ..
    } = launched;
    let (operation, reused) = match start {
        Started::Preparing { operation, reused } => (operation, reused),
        Started::Worker => (None, true),
        Started::Queued { .. } | Started::Detached(_) => {
            anyhow::bail!("the daemon started the agent in no workspace")
        }
    };
    let workspace =
        (space.into_workspace().ok()).context("the daemon's launch is no workspace's")?;
    Ok(StartedWorkspace {
        operation,
        workspace: Box::new(workspace),
        reused,
        entry,
        warnings,
    })
}
