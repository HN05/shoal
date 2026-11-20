use serde::{Deserialize, Serialize};

use super::{Forwarding, Target};
use crate::{
    changes::{ChangeNumber, ToolChange},
    space_names::SpaceSelector,
    threads::ThreadNumber,
};

/// Requests about agents' changes: recording them from an agent's hooks,
/// reading them, and saying who made them.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(
    clippy::enum_variant_names,
    reason = "variants are named as their requests are spelled on the wire"
)]
pub enum ChangeMethod {
    /// What a tool call of the caller's agent changed in its space's
    /// worktree, from the agent's hook; the mixed changes it recorded.
    RecordChange { change: ToolChange },
    /// The changes `filter` keeps, in the order they were made.
    ListChanges { filter: ChangeFilter },
    /// The changes `filter` keeps with the lines each removed and added.
    ChangeDiffs { filter: ChangeFilter },
    /// The changes `changes` names are its agent's, and with `thread`,
    /// for that thread.
    ClaimChanges {
        /// The space whose numbers bare ones name, the caller's own by
        /// default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space: Option<SpaceSelector>,
        changes: Vec<ChangeNumber>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread: Option<ThreadNumber>,
    },
    /// The changes `changes` names are not its agent's, and for no thread.
    UnclaimChanges {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space: Option<SpaceSelector>,
        changes: Vec<ChangeNumber>,
    },
}

impl ChangeMethod {
    /// A hook records its own agent's changes where it runs; the others go
    /// to the machine of the space whose worktree the changes are in.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::RecordChange { .. } => Forwarding::Never,
            Self::ListChanges { .. }
            | Self::ChangeDiffs { .. }
            | Self::ClaimChanges { .. }
            | Self::UnclaimChanges { .. } => Forwarding::Online,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::RecordChange { .. } => None,
            Self::ListChanges { filter } | Self::ChangeDiffs { filter } => filter.target(),
            Self::ClaimChanges { space, .. } | Self::UnclaimChanges { space, .. } => {
                space.as_ref().map(Target::Space)
            }
        }
    }
}

impl ChangeFilter {
    /// What a reading acts on: the thread it names, else the space.
    fn target(&self) -> Option<Target<'_>> {
        match self.thread.as_ref().and_then(ThreadNumber::full) {
            Some(thread) => Some(Target::Item(thread)),
            None => self.space.as_ref().map(Target::Space),
        }
    }
}

/// Which changes a reading returns: a thread's, those made since one of
/// its posts with `since`, else a space's latest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChangeFilter {
    /// The space whose changes to read, the caller's own by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<SpaceSelector>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<ThreadNumber>,
    /// The number of one of the thread's posts, counted from 1 as a
    /// thread shows them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<u32>,
}
