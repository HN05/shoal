//! Chats: starting, restoring and handing over their agents, their
//! parents and scope, and listing them.
use anyhow::Result;

use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use shoal_protocol::space_names::SpaceSelector;

use crate::connection::{Daemon, request};
use shoal_protocol::{
    chats::LaunchedChat,
    protocol::{ChatMethod, ChatStart},
    repository::Selector,
    sessions::Origin,
};

/// The chat whose agent the caller runs itself, at `origin` in a terminal
/// host when it runs in one: `chat`, which the daemon recorded for this
/// start, else the one the launch `queue_entry` recorded while it waited,
/// else a new one `start` describes, which the daemon records with that
/// launch attached and links to what it starts on.
pub async fn launch(
    daemon: &impl Daemon,
    start: ChatStart,
    chat: Option<&SpaceId>,
    origin: Option<Origin>,
    queue_entry: Option<&QueueEntryId>,
) -> Result<LaunchedChat> {
    let method = ChatMethod::LaunchChat {
        start: Box::new(start),
        chat: chat.map(SpaceSelector::id),
        origin,
        queue_entry: queue_entry.cloned(),
    };
    request(daemon, method).await
}

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
