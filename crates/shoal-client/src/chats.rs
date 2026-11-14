//! Chats: starting, restoring and handing over their agents, their names,
//! parents and scope, listing them and removing them.
use crate::space_ids::SpaceId;
use anyhow::Result;

use crate::space_names::SpaceName;
use crate::space_names::SpaceSelector;

use crate::{
    connection::{Daemon, request},
    model::{Chat, ChatHome, RenamedChat},
    output::chat::{RemovedChat, StartedChat},
    protocol::{ChatMethod, ChatScope, ChatStart},
    repository::Selector,
    scope_tokens::ScopeToken,
    sessions::Origin,
};

/// Start the chat `start` describes in a background place the daemon
/// opens, or in the app that runs its agent detached, resolving what it
/// leaves out under the chat's settings, and return it once it runs its
/// agent.
pub async fn launch(daemon: &impl Daemon, start: ChatStart) -> Result<StartedChat> {
    let start = Box::new(start);
    request(daemon, ChatMethod::LaunchChat { start }).await
}

/// What a new chat is recorded as: its name, its agent, the mode the agent
/// starts in and the space it goes below.
#[derive(Clone, Copy)]
pub struct NewChat<'a> {
    pub name: &'a SpaceName,
    pub agent: &'a crate::agent::Agent,
    pub mode: &'a crate::config::modes::ModeName,
    /// The permissions `--permission` gives the agent, whatever the mode's.
    pub narrowing: crate::config::permissions::PermissionsConfig,
    /// The space it goes below; a scoped caller's daemon puts it below the
    /// caller's own space without one.
    pub parent: Option<&'a SpaceSelector>,
    /// The priority set on it.
    pub priority: Option<crate::priority::Priority>,
}

/// Record the `chat` at `home`, running at `origin` in a terminal host when
/// it runs in one, whose agent the execution wrapper then runs.
pub async fn start(
    daemon: &impl Daemon,
    home: ChatHome<'_>,
    chat: NewChat<'_>,
    origin: Option<Origin>,
) -> Result<Chat> {
    request(
        daemon,
        ChatMethod::StartChat {
            repository: home.repository_id().map(Into::into),
            workspace: home
                .workspace()
                .map(|workspace| SpaceSelector::id(&workspace.id)),
            name: chat.name.clone(),
            agent: chat.agent.to_owned(),
            mode: Some(chat.mode.to_owned()),
            narrowing: chat.narrowing,
            origin,
            parent: chat.parent.cloned(),
            priority: chat.priority,
        },
    )
    .await
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
    let root: crate::protocol::ChatRoot = request(daemon, ChatMethod::ChatRoot).await?;
    Ok(root.path)
}

/// The scope token `chat`'s agent carries.
pub async fn scope(daemon: &impl Daemon, chat: &Chat) -> Result<ScopeToken> {
    let chat = SpaceSelector::id(&chat.id);
    let scope: ChatScope = request(daemon, ChatMethod::ChatScope { chat }).await?;
    Ok(scope.token)
}

/// Record `origin` as the place `chat`, recorded before its queued agent
/// started, runs in.
pub async fn place(daemon: &impl Daemon, chat: &Chat, origin: Origin) -> Result<()> {
    let chat = SpaceSelector::id(&chat.id);
    request(daemon, ChatMethod::PlaceChat { chat, origin }).await
}

/// Rename `chat` to `name`.
pub async fn rename(
    daemon: &impl Daemon,
    chat: SpaceSelector,
    name: SpaceName,
) -> Result<RenamedChat> {
    request(daemon, ChatMethod::RenameChat { chat, name }).await
}

/// Running chats.
pub async fn list(daemon: &impl Daemon) -> Result<Vec<Chat>> {
    let mut chats = recorded(daemon).await?;
    chats.retain(|chat| chat.running);
    Ok(chats)
}

/// Running and stopped chats.
pub async fn recorded(daemon: &impl Daemon) -> Result<Vec<Chat>> {
    request(daemon, ChatMethod::ListChats).await
}

/// Remove the chat `chat`, by ID or name, stopping its agent: first the
/// spaces below it, of which `children`, when given, are the IDs the caller
/// saw and the only ones it takes.
pub async fn remove(
    daemon: &impl Daemon,
    chat: &SpaceId,
    children: Option<Vec<SpaceId>>,
) -> Result<RemovedChat> {
    request(
        daemon,
        ChatMethod::RemoveChat {
            chat: SpaceSelector::id(chat),
            children,
        },
    )
    .await
}
