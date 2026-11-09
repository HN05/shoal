use crate::space_ids::SpaceId;
use serde::{Deserialize, Serialize};

use super::Lifetime;
use crate::space_names::SpaceName;
use crate::space_names::SpaceSelector;

/// Requests about chats in registered checkouts.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatMethod {
    /// Record a chat named `name` whose agent replaces process `pid`, in
    /// the worktree of `workspace` when given, which is in `repository`;
    /// without either, a chat without a repository.
    StartChat {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository: Option<crate::repository::Selector>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<SpaceSelector>,
        name: SpaceName,
        agent: String,
        /// The mode its agent starts in; `chat` when omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
        /// The permissions `--permission` gives the agent, whatever the mode's.
        #[serde(
            default,
            skip_serializing_if = "crate::config::permissions::PermissionsConfig::is_empty"
        )]
        narrowing: crate::config::permissions::PermissionsConfig,
        pid: u32,
        /// Where the chat runs in a terminal host.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<crate::sessions::Origin>,
        /// The recording process hands the chat to a session it starts,
        /// with [`Self::MoveChat`]; until then the chat keeps its name.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        hands_over: bool,
        /// The space the chat goes below, which must be in the repository;
        /// none puts it at the top.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<SpaceSelector>,
    },
    /// Run the stopped chat `chat`, by ID, again as process `pid`, as
    /// [`Self::StartChat`] records a new one, under `narrowing`, which
    /// replaces the one it recorded.
    RestoreChat {
        chat: SpaceSelector,
        pid: u32,
        #[serde(
            default,
            skip_serializing_if = "crate::config::permissions::PermissionsConfig::is_empty"
        )]
        narrowing: crate::config::permissions::PermissionsConfig,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<crate::sessions::Origin>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        hands_over: bool,
    },
    /// Refuse the restore of the stopped chat `chat`, by ID, that
    /// [`Self::RestoreChat`] would refuse the caller.
    CheckChatRestore {
        chat: SpaceSelector,
    },
    /// Process `pid` could not restore the stopped chat `chat`, failing with
    /// `error`; the chat waits for `shoal resume` again if `pid` had taken
    /// it over.
    SaveChat {
        chat: SpaceSelector,
        pid: u32,
        error: String,
    },
    /// Record that `chat`'s agent now runs as process `pid`, for an agent
    /// started detached instead of in place of the recording process.
    MoveChat {
        chat: SpaceSelector,
        pid: u32,
    },
    ListChats,
    /// Remove the chat `chat`, by ID or name, stopping its agent: first
    /// the spaces below it, of which `children`, when given, are the IDs the
    /// caller saw and the only ones it takes.
    RemoveChat {
        chat: SpaceSelector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        children: Option<Vec<SpaceId>>,
    },
    /// Give the running chat `chat`, by ID or name, the name `name`; the
    /// reply also names the chat as it was before.
    RenameChat {
        chat: SpaceSelector,
        name: SpaceName,
    },
    /// Refuse a name a new chat could not take now.
    CheckChatName {
        name: SpaceName,
    },
    /// Refuse a `parent` a new chat in `repository`, or without one, in the
    /// worktree of `workspace` when given, could not go below.
    CheckChatParent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository: Option<crate::repository::Selector>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<SpaceSelector>,
        /// As [`Self::StartChat`]'s parent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<SpaceSelector>,
    },
    /// `name`, or its lowest `-2`, `-3`, ... variant, that a new chat could
    /// take now.
    FreeChatName {
        name: String,
    },
    /// The scope token the agent of the chat `chat`, by ID, carries, which
    /// confines it to the chat and the spaces below it.
    ChatScope {
        chat: SpaceSelector,
    },
    /// The agents of the running chats, and of those a restart stopped to
    /// start again, each by its chat's ID, as `ListAgents` lists agents.
    ChatAgents,
    /// Save the session of the running chat `chat`, by ID, stop its agent
    /// and restore it in a background place, answering with the place it
    /// runs in once it runs again.
    /// With `join`, only wait for that restart the run selected, which may
    /// have ended already. `run` names the `shoal restart` run that asks.
    /// `change` changes the permissions the chat's agent was given.
    RestartChat {
        chat: SpaceSelector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        join: Option<crate::model::PendingRestart>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        change: Option<crate::config::permissions::PermissionChange>,
    },
    /// `root_dir`, where a chat without a repository runs, prepared as
    /// registering a repository prepares it.
    ChatRoot,
}

/// The scope token a chat's agent carries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatScope {
    pub token: crate::scope_tokens::ScopeToken,
}

impl ChatMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            // A restart waits for requests of the `shoal resume` it runs.
            Self::RestartChat { .. } => Lifetime::Open,
            _ => Lifetime::Reply,
        }
    }
}

/// The answer to [`ChatMethod::ChatRoot`].
#[derive(Debug, Serialize, Deserialize)]
pub struct ChatRoot {
    pub path: std::path::PathBuf,
}
