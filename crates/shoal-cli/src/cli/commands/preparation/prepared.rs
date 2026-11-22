//! The workspace a launch started its agent in, as a start's workspace.
use anyhow::{Context as _, Result};

use crate::protocol::operations::StartedWorkspace;
use shoal_protocol::protocol::{Launched, Started};

/// The workspace a launch started its agent in, as a start's workspace: a
/// new or reopened one with its preparation, or an existing one whose
/// worker it queued, with nothing left to prepare.
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
