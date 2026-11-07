//! What a snapshot observed: the kind of thing it read and the state it
//! saw that thing in.
use serde::{Deserialize, Deserializer, de::IntoDeserializer};

use crate::{
    ci::JobState,
    items::{
        item::ItemState,
        link::{ItemKind, LinkKind},
    },
    state::states,
};

states!(
    /// What a snapshot reads.
    SnapshotKind {
        Issue => "issue",
        Pr => "pr",
        Run => "run",
        /// A workspace branch, whose local merge check it records.
        Branch => "branch",
    }
);

states!(
    /// An issue's or PR's state, or a run's.
    SnapshotState {
        Open => "open",
        Closed => "closed",
        Merged => "merged",
        /// A run held for approval or for a runner, which has yet to run.
        Waiting => "waiting",
        Running => "running",
        Failed => "failed",
        Finished => "finished",
    }
);

impl From<ItemKind> for SnapshotKind {
    fn from(kind: ItemKind) -> Self {
        match kind {
            ItemKind::Issue => Self::Issue,
            ItemKind::Pr => Self::Pr,
        }
    }
}

impl From<LinkKind> for SnapshotKind {
    fn from(kind: LinkKind) -> Self {
        kind.item().map_or(Self::Run, Self::from)
    }
}

impl From<ItemState> for SnapshotState {
    fn from(state: ItemState) -> Self {
        match state {
            ItemState::Open => Self::Open,
            ItemState::Closed => Self::Closed,
            ItemState::Merged => Self::Merged,
        }
    }
}

impl From<JobState> for SnapshotState {
    fn from(state: JobState) -> Self {
        match state {
            JobState::Waiting => Self::Waiting,
            JobState::Running => Self::Running,
            JobState::Failed => Self::Failed,
            JobState::Finished => Self::Finished,
        }
    }
}

/// A saved snapshot's state, which snapshots that saw none save as empty.
pub(super) fn saved<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<SnapshotState>, D::Error> {
    match String::deserialize(deserializer)?.as_str() {
        "" => Ok(None),
        state => SnapshotState::deserialize(state.into_deserializer()).map(Some),
    }
}
