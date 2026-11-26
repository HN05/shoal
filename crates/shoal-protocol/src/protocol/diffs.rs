use serde::{Deserialize, Serialize};

use super::{Forwarding, Lifetime, Target};
use crate::space_names::SpaceSelector;

/// Requests for what a workspace changed against its base and what
/// conflicts with it, which the daemon reads in its worktree.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffMethod {
    DiffBase {
        workspace: SpaceSelector,
    },
    Conflicts {
        workspace: SpaceSelector,
        target: Option<String>,
    },
    /// The workspace's changes since its branch's fork point, as `git diff`
    /// writes them in its worktree, shown as `display` says: answered with
    /// [`super::Body::Diff`] frames as Git writes, the last with its exit
    /// code. It counts as no use of the workspace.
    Diff {
        workspace: SpaceSelector,
        #[serde(default)]
        display: crate::diffs::DiffDisplay,
    },
}

impl DiffMethod {
    pub(super) fn reads_only(&self) -> bool {
        match self {
            Self::DiffBase { .. } | Self::Conflicts { .. } | Self::Diff { .. } => true,
        }
    }
}

impl DiffMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::Diff { .. } => Lifetime::Open,
            Self::DiffBase { .. } | Self::Conflicts { .. } => Lifetime::Reply,
        }
    }

    /// Each reads the worktree on the workspace's machine, a diff's answer
    /// coming as that machine's Git writes it.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::DiffBase { .. } | Self::Conflicts { .. } => Forwarding::Online,
            Self::Diff { .. } => Forwarding::Streamed,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::DiffBase { workspace }
            | Self::Conflicts { workspace, .. }
            | Self::Diff { workspace, .. } => Some(Target::Space(workspace)),
        }
    }
}
