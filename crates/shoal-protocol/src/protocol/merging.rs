use serde::{Deserialize, Serialize};

use super::{Forwarding, Target};
use crate::space_names::SpaceSelector;

/// Requests about how `shoal merge` merges a space's work and what a local
/// merge of a workspace does.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergingMethod {
    /// How `shoal merge` merges the space's work, from the open PRs it
    /// links as assigned or opened.
    MergePath { space: SpaceSelector },
    /// What merging the workspace into the default branch by `method`,
    /// with a squash's `message`, does, after the checks the merge makes
    /// before it changes anything; with `push`, the merge pushes the default
    /// branch afterwards.
    PreviewMerge {
        workspace: SpaceSelector,
        push: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        method: Option<crate::forge::items::action::MergeMethod>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

impl MergingMethod {
    /// Each reads the space's links or the workspace's branch on its
    /// machine.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::MergePath { .. } | Self::PreviewMerge { .. } => Forwarding::Online,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::MergePath { space } => Some(Target::Space(space)),
            Self::PreviewMerge { workspace, .. } => Some(Target::Space(workspace)),
        }
    }
}
