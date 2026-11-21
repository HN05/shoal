use serde::{Deserialize, Serialize};

use super::{Forwarding, Target};
use crate::space_names::SpaceSelector;

/// Requests about the places a space's executions run in and those Shoal
/// created for it, a workspace's or a chat's.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaceMethod {
    /// Where the space's executions run and the places Shoal created for
    /// it, a workspace's or a chat's.
    SpacePlaces { space: SpaceSelector },
    /// Record a place Shoal created for the workspace, which it closes once
    /// the workspace is removed.
    RecordOwnedPlace {
        workspace: SpaceSelector,
        origin: crate::sessions::Origin,
    },
    /// The places Shoal created for every workspace.
    OwnedPlaces,
    /// Close `place`, a place Shoal created for `workspace`, while the
    /// daemon's record still names that workspace its owner. The workspace
    /// may already be removed, so the request names it by ID.
    CloseOwnedPlace {
        workspace: shoal_ids::space_ids::SpaceId,
        place: crate::sessions::Origin,
    },
    /// Whose `place` is, as the daemon knows it for the workspace or chat
    /// `space`.
    PlaceOwnership {
        space: SpaceSelector,
        place: crate::sessions::Origin,
    },
}

impl PlaceMethod {
    /// The places and terminal hosts are the caller's machine's.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::SpacePlaces { .. }
            | Self::RecordOwnedPlace { .. }
            | Self::OwnedPlaces
            | Self::CloseOwnedPlace { .. }
            | Self::PlaceOwnership { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::RecordOwnedPlace { workspace, .. } => Some(Target::Space(workspace)),
            Self::SpacePlaces { space } | Self::PlaceOwnership { space, .. } => {
                Some(Target::Space(space))
            }
            Self::OwnedPlaces | Self::CloseOwnedPlace { .. } => None,
        }
    }
}
