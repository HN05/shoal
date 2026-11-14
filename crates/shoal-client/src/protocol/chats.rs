use crate::space_ids::SpaceId;
use serde::{Deserialize, Serialize};

use super::Lifetime;
use crate::space_names::SpaceName;
use crate::space_names::SpaceSelector;

/// Requests about chats in registered checkouts.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatMethod {
    /// Start the chat `start` describes in a background place, as `shoal
    /// start` starts one without a terminal, once it runs its agent.
    /// Answers with [`super::Body::StartedChat`].
    LaunchChat {
        start: Box<ChatStart>,
    },
    /// Record a chat named `name`, whose agent its wrapper then registers
    /// as an execution of the chat, in the worktree of `workspace` when
    /// given, which is in `repository`; without either, a chat without a
    /// repository.
    StartChat {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository: Option<crate::repository::Selector>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<SpaceSelector>,
        name: SpaceName,
        agent: crate::agent::Agent,
        /// The mode its agent starts in; `chat` when omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<crate::config::modes::ModeName>,
        /// The permissions `--permission` gives the agent, whatever the mode's.
        #[serde(
            default,
            skip_serializing_if = "crate::config::permissions::PermissionsConfig::is_empty"
        )]
        narrowing: crate::config::permissions::PermissionsConfig,
        /// Where the chat runs in a terminal host.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<crate::sessions::Origin>,
        /// The space the chat goes below, which must be in the repository;
        /// none puts it at the top.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<SpaceSelector>,
        /// The priority set on the chat; a scoped caller gives at most its
        /// own space's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        priority: Option<crate::priority::Priority>,
        /// The queued launch whose start records the chat, which a scoped
        /// process may have queued: the chat is then confined as that
        /// process's start would be.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queue_entry: Option<crate::queue::QueueEntryId>,
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
    /// The scope token the agent of the chat `chat`, by ID, carries, which
    /// confines it to the chat and the spaces below it.
    ChatScope {
        chat: SpaceSelector,
    },
    /// Record `origin` as the place the chat `chat`, recorded before its
    /// queued agent started, now runs in, unless it has one: a place Shoal
    /// opened for it becomes the chat's, as for a chat recorded there.
    PlaceChat {
        chat: SpaceSelector,
        origin: crate::sessions::Origin,
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
            // A start waits for requests of the chat start worker it runs.
            Self::LaunchChat { .. } => Lifetime::Open,
            _ => Lifetime::Reply,
        }
    }
}

/// The answer to [`ChatMethod::ChatRoot`].
#[derive(Debug, Serialize, Deserialize)]
pub struct ChatRoot {
    pub path: std::path::PathBuf,
}

/// A new chat for the daemon to start in a background place: where it runs,
/// below which space, under what name, and what its agent runs with and
/// starts on. What it leaves out the daemon resolves under the chat's
/// settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatStart {
    /// The registered repository in whose checkout it runs; with neither
    /// this nor `workspace`, it runs in `root_dir` without a repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<crate::repository::Selector>,
    /// The ready workspace in whose worktree it runs, in that workspace's
    /// repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<SpaceSelector>,
    /// The space it goes below; a scoped caller's own space without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SpaceSelector>,
    /// Its name; without one, its issue's title or its place's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<SpaceName>,
    /// The agent it runs; `agent.default` without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<crate::agent::Agent>,
    /// The mode its agent starts in; `chat` without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<crate::config::modes::ModeName>,
    /// The model and effort requested over the configured ones.
    #[serde(default, skip_serializing_if = "crate::agent::ModelChoice::is_empty")]
    pub model: crate::agent::ModelChoice,
    /// The permissions given to its agent, whatever its mode's.
    #[serde(
        default,
        skip_serializing_if = "crate::config::permissions::PermissionsConfig::is_empty"
    )]
    pub narrowing: crate::config::permissions::PermissionsConfig,
    /// The issue it starts on, by number or URL, which it is linked to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<crate::forge::items::reference::ItemInput>,
    /// Arguments passed on to its agent, such as its task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// The priority set on it; a scoped caller gives at most its own
    /// space's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<crate::priority::Priority>,
    /// The variables of the terminal whose host opens its place; without
    /// them, the default host opens it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::BTreeMap<String, String>>,
}
