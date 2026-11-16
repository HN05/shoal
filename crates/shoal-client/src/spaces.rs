//! Spaces of either kind: every space the caller sees.
use anyhow::Result;

use crate::connection::{Daemon, request};
use shoal_protocol::{
    model::{Space, Spaces},
    protocol::WorkspaceMethod,
};

/// The workspaces and chats, running or stopped, visible to this caller;
/// the daemon filters scoped requests.
pub async fn list(daemon: &impl Daemon) -> Result<Spaces> {
    let spaces: Vec<Space> = request(daemon, WorkspaceMethod::ListSpaces).await?;
    Ok(Spaces(spaces))
}
