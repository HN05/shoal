//! Chats: the parent a new one may go below, and where one without a
//! repository runs.
use anyhow::Result;

use crate::connection::{Daemon, request};
use shoal_protocol::{protocol::ChatMethod, repository::Selector, space_names::SpaceSelector};

/// Refuse `parent`, a space a new chat in `repository`, or without one, in
/// the worktree of `workspace` when given, could not go below.
pub async fn check_parent(
    daemon: &impl Daemon,
    repository: Option<Selector>,
    workspace: Option<SpaceSelector>,
    parent: SpaceSelector,
) -> Result<()> {
    let method = ChatMethod::CheckChatParent {
        repository,
        workspace,
        parent: Some(parent),
    };
    request::<()>(daemon, method).await
}

/// `root_dir`, where a chat without a repository runs, prepared as
/// registering a repository prepares it.
pub async fn root(daemon: &impl Daemon) -> Result<std::path::PathBuf> {
    let root: shoal_protocol::protocol::ChatRoot = request(daemon, ChatMethod::ChatRoot).await?;
    Ok(root.path)
}
