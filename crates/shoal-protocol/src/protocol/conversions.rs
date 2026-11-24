use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{CreationOptions, Forwarding, Target};
use crate::{
    agent::{Agent, ModelChoice},
    config::{modes::ModeName, permissions::PermissionChange},
    space_names::SpaceSelector,
};

/// How a converted space's agent goes on: under the tool `agent` names,
/// which takes the work over in a new session from copies of the space's
/// sessions so far, told `note` after its mode's instructions, else under
/// its own from a fork of its session; with `model` asked of it and
/// `permissions` changing what it was given.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentChange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<Agent>,
    #[serde(default, flatten)]
    pub model: ModelChoice,
    #[serde(default, skip_serializing_if = "PermissionChange::is_empty")]
    pub permissions: PermissionChange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The answer to [`ConversionMethod::ConvertAgent`]: the space, the tool its
/// agent leaves, if it had one, the tool that takes over, and whether it
/// took over at once, rather than once the running agent's turn ends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSwitch {
    pub space: crate::model::Space,
    pub from: Option<Agent>,
    pub to: Agent,
    pub at_once: bool,
}

impl AgentChange {
    /// Whether it changes nothing of the agent.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Requests that turn a space into the other kind, keeping it with its
/// number, links and children.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversionMethod {
    /// Turn the chat `chat` into a workspace on `branch`, an existing branch
    /// or a new one from `base`, linked to `issue` when given, and start
    /// preparing it in an operation, with `env` as
    /// [`super::OperationMethod::StartOperation`] takes it; its agent
    /// restarts there in `mode`, `worker` by default, as `agent` says, once
    /// its turn ends. Answers with [`super::Body::WorkspaceStarted`].
    ConvertToWorkspace {
        chat: SpaceSelector,
        branch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        issue: Option<crate::forge::items::reference::ItemInput>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
        #[serde(default, skip_serializing_if = "CreationOptions::is_default")]
        options: CreationOptions,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<ModeName>,
        #[serde(default, skip_serializing_if = "AgentChange::is_empty")]
        agent: AgentChange,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<BTreeMap<String, String>>,
    },
    /// Make the workspace a chat in `mode`, `chat` by default, once its
    /// agent's turn ends: its worktree goes as `shoal rm` removes it, and
    /// its agent restarts in the repository's checkout as `agent` says.
    ConvertToChat {
        workspace: SpaceSelector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<ModeName>,
        #[serde(default, skip_serializing_if = "AgentChange::is_empty")]
        agent: AgentChange,
    },
    /// Hand the work of `space` over to the agent tool `agent` names, in
    /// `mode`, the space's own by default: once the turn of its running
    /// agent ends, or at once for a stopped or asleep one. Answers with
    /// [`super::Body::AgentSwitch`].
    ConvertAgent {
        space: SpaceSelector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<ModeName>,
        agent: AgentChange,
    },
}

impl ConversionMethod {
    /// A conversion goes to the machine of the space it converts.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::ConvertToWorkspace { .. }
            | Self::ConvertToChat { .. }
            | Self::ConvertAgent { .. } => Forwarding::Online,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::ConvertToWorkspace { chat: space, .. }
            | Self::ConvertToChat {
                workspace: space, ..
            }
            | Self::ConvertAgent { space, .. } => Some(Target::Space(space)),
        }
    }
}
