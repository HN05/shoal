use serde::{Deserialize, Serialize};

use crate::space_names::SpaceName;
use crate::space_names::SpaceSelector;

/// Requests about chats in registered checkouts.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatMethod {
    /// The chat whose agent the caller runs itself, in its terminal, a
    /// place it opened or an app, at `origin` where a terminal host runs
    /// it: `chat`, which was recorded for this start, else a new one
    /// `start` describes, recorded with the launch `queue_entry` attached
    /// and linked to what it starts on. Answers with
    /// [`crate::chats::LaunchedChat`].
    LaunchChat {
        start: Box<ChatStart>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat: Option<SpaceSelector>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<crate::sessions::Origin>,
        /// The launch whose slot the chat's agent takes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queue_entry: Option<shoal_ids::queue_entry_ids::QueueEntryId>,
    },
    /// Remove the new chat `chat`, recorded for a start that failed with
    /// `error`, telling its starter as the daemon tells of a chat its own
    /// launch could not start; a chat whose agent ever started stays, and
    /// one already gone leaves nothing to do.
    FailChatStart {
        chat: shoal_ids::space_ids::SpaceId,
        error: String,
    },
    /// Refuse a `parent` a new chat in `repository`, or without one, in the
    /// worktree of `workspace` when given, could not go below.
    CheckChatParent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository: Option<crate::repository::Selector>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<SpaceSelector>,
        /// As [`ChatStart`]'s parent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<SpaceSelector>,
    },
    /// `root_dir`, where a chat without a repository runs, prepared as
    /// registering a repository prepares it.
    ChatRoot,
}

/// The answer to [`ChatMethod::ChatRoot`].
#[derive(Debug, Serialize, Deserialize)]
pub struct ChatRoot {
    pub path: std::path::PathBuf,
}

/// A new chat for the daemon to record and start: where it runs, below
/// which space, under what name, what its agent runs with and starts on,
/// and what it is linked to. What it leaves out the daemon resolves under
/// the chat's settings.
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
    /// What it is linked to once recorded: the issues and PRs it starts
    /// on, assigned, and the opened and related items and assignees a
    /// moved chat brings.
    #[serde(default, skip_serializing_if = "crate::model::SpaceLinks::is_empty")]
    pub links: crate::model::SpaceLinks,
    /// Its agent's first message when no issue gives it one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Arguments passed on to its agent, such as its task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// The priority set on it; a scoped caller gives at most its own
    /// space's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<shoal_schedule::priority::Priority>,
    /// The variables of the terminal whose host opens its place; without
    /// them, the default host opens it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::BTreeMap<String, String>>,
    /// Start its agent at once regardless of the heavy slots, which it
    /// still takes. Scoped callers cannot.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub now: bool,
}
