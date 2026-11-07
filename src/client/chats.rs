//! Chats: starting, restoring and handing over their agents, their names,
//! parents and scope, listing them and removing them.
use anyhow::Result;

use crate::{
    client::{
        connection::{Daemon, request},
        output::chat::RemovedChat,
        repository::Selector,
        sessions::Origin,
    },
    model::{Chat, ChatHome, RenamedChat},
    protocol::{ChatMethod, ScopeToken},
};

/// What a new chat is recorded as: its name, its agent, the mode the agent
/// starts in and the space it goes below.
#[derive(Clone, Copy)]
pub struct NewChat<'a> {
    pub name: &'a str,
    pub agent: &'a str,
    pub mode: &'a str,
    /// The space it goes below; a scoped caller's daemon puts it below the
    /// caller's own space without one.
    pub parent: Option<&'a str>,
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
            workspace: home.workspace().map(|workspace| workspace.id.clone()),
            name: chat.name.to_owned(),
            agent: chat.agent.to_owned(),
            mode: Some(chat.mode.to_owned()),
            pid: std::process::id(),
            origin,
            hands_over,
            parent: chat.parent.map(str::to_owned),
        },
    )
    .await
}

/// Refuse a name a new chat could not take now.
pub async fn check_name(daemon: &impl Daemon, name: &str) -> Result<()> {
    request::<()>(
        daemon,
        ChatMethod::CheckChatName {
            name: name.to_owned(),
        },
    )
    .await
}

/// Refuse `parent`, a space a new chat in `repository`, or without one, in
/// the worktree of `workspace` when given, could not go below.
pub async fn check_parent(
    daemon: &impl Daemon,
    repository: Option<Selector>,
    workspace: Option<String>,
    parent: String,
) -> Result<()> {
    let method = ChatMethod::CheckChatParent {
        repository,
        workspace,
        parent: Some(parent),
    };
    request::<()>(daemon, method).await
}

/// Refuse the restore of the stopped chat `chat`, by ID, that the daemon
/// would refuse this process, before anything opens a place for it.
pub async fn check_restore(daemon: &impl Daemon, chat: String) -> Result<()> {
    request::<()>(daemon, ChatMethod::CheckChatRestore { chat }).await
}

/// `root_dir`, where a chat without a repository runs, prepared as
/// registering a repository prepares it.
pub async fn root(daemon: &impl Daemon) -> Result<std::path::PathBuf> {
    let root: crate::protocol::ChatRoot = request(daemon, ChatMethod::ChatRoot).await?;
    Ok(root.path)
}

/// `name`, or its lowest `-2`, `-3`, ... variant, that a new chat could take now.
pub async fn free_name(daemon: &impl Daemon, name: String) -> Result<String> {
    request(daemon, ChatMethod::FreeChatName { name }).await
}

/// The scope token `chat`'s agent carries.
pub async fn scope(daemon: &impl Daemon, chat: &Chat) -> Result<String> {
    let chat = chat.id.clone();
    let scope: ScopeToken = request(daemon, ChatMethod::ChatScope { chat }).await?;
    Ok(scope.token)
}

/// Hand `chat` to process `pid`, whose agent now runs it.
pub async fn move_to(daemon: &impl Daemon, chat: &Chat, pid: u32) -> Result<Chat> {
    request(
        daemon,
        ChatMethod::MoveChat {
            chat: chat.id.clone(),
            pid,
        },
    )
    .await
}

/// Rename `chat` to `name`.
pub async fn rename(daemon: &impl Daemon, chat: String, name: String) -> Result<RenamedChat> {
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
            chat: chat.to_owned(),
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
            chat: chat.to_owned(),
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
            chat: chat.to_owned(),
            children,
        },
    )
    .await
}
