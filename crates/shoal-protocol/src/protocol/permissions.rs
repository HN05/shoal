use serde::{Deserialize, Serialize};

use super::{Forwarding, Target};
use crate::space_names::SpaceSelector;

/// Requests for the effective permissions of a space or of an agent's
/// launch in one.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMethod {
    /// The effective permissions of an agent starting in `mode` with
    /// `narrowing` in the workspace, which its launch's push rules follow.
    LaunchPermissions {
        workspace: SpaceSelector,
        mode: crate::config::modes::ModeName,
        #[serde(
            default,
            skip_serializing_if = "crate::config::permissions::PermissionsConfig::is_empty"
        )]
        narrowing: crate::config::permissions::PermissionsConfig,
    },
    /// The effective permissions of the calling scoped process's own space
    /// for an action in the registered repository `repository`, the most
    /// its `--permission` gives; the user holds every permission.
    HeldPermissions {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository: Option<crate::repository::Selector>,
    },
}

impl PermissionMethod {
    pub(super) fn reads_only(&self) -> bool {
        match self {
            Self::LaunchPermissions { .. } | Self::HeldPermissions { .. } => false,
        }
    }
}

impl PermissionMethod {
    /// A launch's permissions are those of the workspace's machine; the
    /// caller's own are the caller's machine's.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::LaunchPermissions { .. } => Forwarding::Online,
            Self::HeldPermissions { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::LaunchPermissions { workspace, .. } => Some(Target::Space(workspace)),
            Self::HeldPermissions { .. } => None,
        }
    }
}
