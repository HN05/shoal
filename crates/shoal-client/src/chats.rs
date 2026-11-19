//! Chats: where one without a repository runs.
use anyhow::Result;

use crate::connection::{Daemon, request};
use shoal_protocol::protocol::ChatMethod;

/// `root_dir`, where a chat without a repository runs, prepared as
/// registering a repository prepares it.
pub async fn root(daemon: &impl Daemon) -> Result<std::path::PathBuf> {
    let root: shoal_protocol::protocol::ChatRoot = request(daemon, ChatMethod::ChatRoot).await?;
    Ok(root.path)
}
