//! Agent launches: what a start asks for, which the daemon resolves into
//! the space it starts in and the agent it queues there.
use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

use shoal_ids::{agent_session_ids::AgentSessionId, checkpoint_numbers::CheckpointNumber};
use shoal_schedule::priority::Priority;

use crate::{
    agent::{Agent, ModelChoice},
    chats::StartedChat,
    config::{modes::ModeName, permissions::PermissionsConfig},
    forge::items::reference::ItemInput,
    model::{QueueEntry, Workspace},
    protocol::operations::StartedWorkspace,
    space_names::{SpaceName, SpaceSelector},
};

mod target;

pub use target::StartTarget;

/// What `shoal start` takes, as the command line gave it: what the agent
/// starts on, the space it starts in and how its agent runs. What it leaves
/// out the daemon decides, such as the mode, the kind of space and the
/// repository.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Launch {
    /// The positional argument: a registered repository, or an issue, PR
    /// or branch it starts on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<StartTarget>,
    /// The repository `--repo` names, for an issue number or a link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// The branch a workspace starts on, as `git switch` takes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The exact directory of a new worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// The ref a new branch starts from, or an existing one compares with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// The Git profile of a new worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_profile: Option<String>,
    /// The issue the agent starts on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<IssueChoice>,
    /// The workspace whose files a new workspace copies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy: Option<SpaceChoice>,
    /// The checkpoint of the copied workspace whose files it copies instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointNumber>,
    /// Skip a new workspace's setup.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skip_setup: bool,
    /// A new workspace whose every removal deletes it whatever it holds.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disposable: bool,
    /// The existing workspace the agent starts in instead of a new space:
    /// its worker, or a chat in its worktree.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "in")]
    pub in_workspace: Option<SpaceChoice>,
    /// Start in a chat whatever the mode's kind.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub chat: bool,
    /// The mode the agent starts in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ModeName>,
    /// The agent to start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<Agent>,
    /// The model and effort requested over the configured ones.
    #[serde(default, skip_serializing_if = "ModelChoice::is_empty")]
    pub model: ModelChoice,
    /// The session the agent starts from a fork of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<AgentSessionId>,
    /// The new space's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<SpaceName>,
    /// The new space's priority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    /// The permissions given to the agent, whatever its mode's.
    #[serde(default, skip_serializing_if = "PermissionsConfig::is_empty")]
    pub permissions: PermissionsConfig,
    /// The space the new one goes below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SpaceSelector>,
    /// The spaces, queued agents and issues the agent waits for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<String>,
    /// Start the agent at once regardless of the heavy slots.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub now: bool,
    /// What the agent is told to do, passed on to it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task: Vec<String>,
    /// The agent's first message when no issue gives it one, such as the
    /// subject `shoal repo setup` gives its agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// The setup of the repository the launch starts in, whose agent alone
    /// starts in the setup mode.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub repository_setup: bool,
    /// The directory the start is made in, whose checkout or worktree names
    /// the repository or workspace the start leaves out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<PathBuf>,
    /// The variables of the terminal whose host opens the agent's place;
    /// without them, the default host opens it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    /// The environment a new workspace's setup runs in: the caller's,
    /// without its secrets; the daemon's own without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_env: Option<BTreeMap<String, String>>,
    /// Why no terminal host the caller reaches could start its agent in the
    /// background, which refuses a launch that starts one before it creates
    /// anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unplaceable: Option<String>,
}

/// Where a launch started its agent.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Launched {
    /// In a new chat.
    Chat(StartedChat),
    /// As the worker of an existing workspace.
    Worker(Box<QueuedWorker>),
    /// In a new or reopened workspace, which the operation it names
    /// prepares, or will once what it waits for allows, with the agent
    /// waiting in the queue for the preparation.
    Workspace(Box<StartedWorkspace>),
}

/// The worker of an existing workspace a launch queued.
#[derive(Debug, Serialize, Deserialize)]
pub struct QueuedWorker {
    pub workspace: Workspace,
    /// Its entry in the agent queue; none when no agent is named or
    /// configured, so the launch starts none.
    pub entry: Option<QueueEntry>,
}

/// What made a launch, which the journal records with its agent's start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaunchSource {
    /// A start someone made, such as with `shoal start`.
    Manual,
    /// The firing of the trigger `name`.
    Trigger { name: String },
}

impl std::fmt::Display for LaunchSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Manual => f.write_str("manual"),
            Self::Trigger { name } => write!(f, "trigger {name}"),
        }
    }
}

/// The issue a start names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueChoice {
    /// One picked among the open issues that need a plan, in plan mode.
    Pick,
    /// The issue of this number or URL.
    Named(ItemInput),
}

impl IssueChoice {
    /// The issue it names, if it names one.
    pub fn named(&self) -> Option<&ItemInput> {
        match self {
            Self::Pick => None,
            Self::Named(issue) => Some(issue),
        }
    }
}

impl From<Option<ItemInput>> for IssueChoice {
    /// `--issue` with a value, else without one.
    fn from(issue: Option<ItemInput>) -> Self {
        issue.map_or(Self::Pick, Self::Named)
    }
}

/// A space a start names, or the one it is made in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceChoice {
    /// The workspace the start is made in.
    Current,
    Space(SpaceSelector),
}

impl From<Option<SpaceSelector>> for SpaceChoice {
    /// A flag such as `--in` with a value, else without one.
    fn from(space: Option<SpaceSelector>) -> Self {
        space.map_or(Self::Current, Self::Space)
    }
}

impl SpaceChoice {
    /// The space it names, if it names one.
    pub fn named(&self) -> Option<&SpaceSelector> {
        match self {
            Self::Current => None,
            Self::Space(space) => Some(space),
        }
    }
}
