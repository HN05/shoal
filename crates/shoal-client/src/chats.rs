//! Chats: starting, restoring and handing over their agents, their names,
//! parents and scope, listing them and removing them.
use anyhow::Result;

use crate::space_names::SpaceName;
use crate::space_names::SpaceSelector;

use crate::{
    connection::{Daemon, request},
    model::{Chat, ChatHome, RenamedChat},
    output::chat::RemovedChat,
    protocol::{ChatMethod, ScopeToken},
    repository::Selector,
    sessions::Origin,
};

/// What a new chat is recorded as: its name, its agent, the mode the agent
/// starts in and the space it goes below.
#[derive(Clone, Copy)]
pub struct NewChat<'a> {
    pub name: &'a SpaceName,
    pub agent: &'a str,
    pub mode: &'a str,
    /// The `--permission` narrowing of the mode's permissions.
    pub permissions: crate::config::permissions::PermissionsConfig,
    /// The space it goes below; a scoped caller's daemon puts it below the
    /// caller's own space without one.
    pub parent: Option<&'a SpaceSelector>,
}

/// Record the `chat` whose agent replaces this process at `home`, running
/// at `origin` in a terminal host when it runs in one, or when `hands_over`
/// runs in a session this process starts and [`move_to`] names.
pub async fn start(
    daemon: &impl Daemon,
    home: ChatHome<'_>,
    chat: NewChat<'_>,
    origin: Option<Origin>,
    hands_over: bool,
) -> Result<Chat> {
    request(
        daemon,
        ChatMethod::StartChat {
            repository: home.repository_id().map(|id| id.to_owned().into()),
            workspace: home
                .workspace()
                .map(|workspace| SpaceSelector::id(&workspace.id)),
            name: chat.name.clone(),
            agent: chat.agent.to_owned(),
            mode: Some(chat.mode.to_owned()),
            permissions: chat.permissions,
            pid: std::process::id(),
            origin,
            hands_over,
            parent: chat.parent.cloned(),
        },
    )
    .await
}

/// Refuse a name a new chat could not take now.
pub async fn check_name(daemon: &impl Daemon, name: &SpaceName) -> Result<()> {
    let name = name.clone();
    request::<()>(daemon, ChatMethod::CheckChatName { name }).await
}

/// Refuse `parent`, a space a new chat in `repository`, or without one, in
/// the worktree of `workspace` when given, could not go below.
pub async fn check_parent(
    daemon: &impl Daemon,
    repository: Option<Selector>,
    workspace: Option<SpaceSelector>,
    parent: String,
) -> Result<()> {
    let method = ChatMethod::CheckChatParent {
        repository,
        workspace,
        parent: Some(SpaceSelector::parse(&parent)),
    };
    request::<()>(daemon, method).await
}

/// Refuse the restore of the stopped chat `chat`, by ID, that the daemon
/// would refuse this process, before anything opens a place for it.
pub async fn check_restore(daemon: &impl Daemon, chat: &str) -> Result<()> {
    let chat = SpaceSelector::id(chat);
    request::<()>(daemon, ChatMethod::CheckChatRestore { chat }).await
}

/// `root_dir`, where a chat without a repository runs, prepared as
/// registering a repository prepares it.
pub async fn root(daemon: &impl Daemon) -> Result<std::path::PathBuf> {
    let root: crate::protocol::ChatRoot = request(daemon, ChatMethod::ChatRoot).await?;
    Ok(root.path)
}

/// `name`, or its lowest `-2`, `-3`, ... variant, that a new chat could take now.
pub async fn free_name(daemon: &impl Daemon, name: String) -> Result<SpaceName> {
    request(daemon, ChatMethod::FreeChatName { name }).await
}

/// The scope token `chat`'s agent carries.
pub async fn scope(daemon: &impl Daemon, chat: &Chat) -> Result<String> {
    let chat = SpaceSelector::id(&chat.id);
    let scope: ScopeToken = request(daemon, ChatMethod::ChatScope { chat }).await?;
    Ok(scope.token)
}

/// Hand `chat` to process `pid`, whose agent now runs it.
pub async fn move_to(daemon: &impl Daemon, chat: &Chat, pid: u32) -> Result<Chat> {
    request(
        daemon,
        ChatMethod::MoveChat {
            chat: SpaceSelector::id(&chat.id),
            pid,
        },
    )
    .await
}

/// Rename `chat` to `name`.
pub async fn rename(
    daemon: &impl Daemon,
    chat: SpaceSelector,
    name: SpaceName,
) -> Result<RenamedChat> {
    request(daemon, ChatMethod::RenameChat { chat, name }).await
}

/// Running chats; listing them ends those whose agent exited.
pub async fn list(daemon: &impl Daemon) -> Result<Vec<Chat>> {
    let mut chats = recorded(daemon).await?;
    chats.retain(|chat| chat.restore.is_none());
    Ok(chats)
}

/// Running and stopped chats.
pub async fn recorded(daemon: &impl Daemon) -> Result<Vec<Chat>> {
    request(daemon, ChatMethod::ListChats).await
}

/// [`start`], or for a start that `restores` a stopped chat, by ID,
/// [`restore`], which keeps the chat's own name, agent and mode.
pub async fn record(
    daemon: &impl Daemon,
    home: ChatHome<'_>,
    chat: NewChat<'_>,
    origin: Option<Origin>,
    hands_over: bool,
    restores: Option<&str>,
) -> Result<Chat> {
    match restores {
        Some(restored) => restore(daemon, restored, origin, hands_over).await,
        None => start(daemon, home, chat, origin, hands_over).await,
    }
}

/// Run the stopped chat `chat`, by ID, again in this process, as [`start`]
/// records a new chat.
pub async fn restore(
    daemon: &impl Daemon,
    chat: &str,
    origin: Option<Origin>,
    hands_over: bool,
) -> Result<Chat> {
    request(
        daemon,
        ChatMethod::RestoreChat {
            chat: SpaceSelector::parse(chat),
            pid: std::process::id(),
            origin,
            hands_over,
        },
    )
    .await
}

/// Report that this process could not restore the stopped chat `chat`, which
/// waits for `shoal resume` again if this process had taken it over.
pub async fn save(daemon: &impl Daemon, chat: &str, error: &anyhow::Error) -> Result<()> {
    request::<()>(
        daemon,
        ChatMethod::SaveChat {
            chat: SpaceSelector::parse(chat),
            pid: std::process::id(),
            error: format!("{error:#}"),
        },
    )
    .await
}

/// `ran`, a start in this process, after reporting its failure when it
/// `restores` a stopped chat: the chat waits for `shoal resume` again, if this
/// process had taken it over, and the user learns why.
pub async fn keep_failed_restore<T>(
    daemon: &impl Daemon,
    restores: Option<&str>,
    ran: Result<T>,
) -> Result<T> {
    if let (Err(failure), Some(chat)) = (&ran, restores)
        && let Err(error) = save(daemon, chat, failure).await
    {
        eprintln!("warning: the chat could not wait for shoal resume again: {error:#}");
    }
    ran
}

/// Remove the chat `chat`, by ID or name, stopping its agent: first the
/// spaces below it, of which `children`, when given, are the IDs the caller
/// saw and the only ones it takes.
pub async fn remove(
    daemon: &impl Daemon,
    chat: &str,
    children: Option<Vec<String>>,
) -> Result<RemovedChat> {
    request(
        daemon,
        ChatMethod::RemoveChat {
            chat: SpaceSelector::parse(chat),
            children,
        },
    )
    .await
}
