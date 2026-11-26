use super::{Forwarding, Target};
use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};

/// Requests about the holds that keep spaces from cleanup.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldMethod {
    HoldAcquire {
        space: SpaceSelector,
        name: String,
        reason: Option<String>,
    },
    HoldRelease {
        space: SpaceSelector,
        name: String,
    },
    HoldList {
        space: SpaceSelector,
    },
}

impl HoldMethod {
    pub(super) fn reads_only(&self) -> bool {
        match self {
            Self::HoldList { .. } => true,
            Self::HoldAcquire { .. } | Self::HoldRelease { .. } => false,
        }
    }
}

impl HoldMethod {
    /// Each reads or changes the space's holds.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::HoldAcquire { .. } | Self::HoldRelease { .. } | Self::HoldList { .. } => {
                Forwarding::Online
            }
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::HoldAcquire { space, .. }
            | Self::HoldRelease { space, .. }
            | Self::HoldList { space } => Some(Target::Space(space)),
        }
    }
}
