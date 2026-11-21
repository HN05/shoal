use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{CreationOptions, Forwarding, Target};
use crate::space_names::SpaceSelector;

/// Requests that turn a space into the other kind, keeping it with its
/// number, links and children.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversionMethod {
    /// Turn the chat `chat` into a workspace on `branch`, an existing branch
    /// or a new one from `base`, linked to `issue` when given, and start
    /// preparing it in an operation, with `env` as
    /// [`super::OperationMethod::StartOperation`] takes it; its agent
    /// restarts there in `mode`, `worker` by default, once its turn ends.
    /// Answers with [`super::Body::WorkspaceStarted`].
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
        mode: Option<crate::config::modes::ModeName>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<BTreeMap<String, String>>,
    },
    /// Make the workspace a chat in `mode`, `chat` by default, once its
    /// agent's turn ends: its worktree goes as `shoal rm` removes it, and
    /// its agent restarts in the repository's checkout.
    ConvertToChat {
        workspace: SpaceSelector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<crate::config::modes::ModeName>,
    },
}

impl ConversionMethod {
    /// A conversion goes to the machine of the space it converts.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::ConvertToWorkspace { .. } | Self::ConvertToChat { .. } => Forwarding::Online,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::ConvertToWorkspace { chat: space, .. }
            | Self::ConvertToChat {
                workspace: space, ..
            } => Some(Target::Space(space)),
        }
    }
}
