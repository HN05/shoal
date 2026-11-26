use super::{Forwarding, Target};
use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};

/// Requests to fetch the secrets a repository configures.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretMethod {
    /// Confirm the caller may fetch a configured secret, requesting approval
    /// when it is gated; the daemon never sees the value.
    SecretAcquire {
        workspace: SpaceSelector,
        name: String,
        request: crate::secrets::SecretRequest,
    },
    SecretRelease {
        workspace: SpaceSelector,
        name: String,
    },
    SecretCatalog {
        workspace: SpaceSelector,
    },
}

impl SecretMethod {
    /// A secret's grant is a lease taken for the caller; what is
    /// configured is state.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::SecretCatalog { .. } => Forwarding::Online,
            Self::SecretAcquire { .. } | Self::SecretRelease { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::SecretAcquire { workspace, .. }
            | Self::SecretRelease { workspace, .. }
            | Self::SecretCatalog { workspace } => Some(Target::Space(workspace)),
        }
    }
}
