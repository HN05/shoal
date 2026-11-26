use serde::{Deserialize, Serialize};

use super::{Forwarding, Target};
use crate::{
    changes::{ChangeNumber, ToolChange},
    space_names::SpaceSelector,
    threads::ThreadNumber,
};

/// Requests about the changes Shoal records of agents' worktrees.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    /// The user edited or saved the files `paths`, by absolute path, as
    /// an editor reports them, which makes a change an agent's command
    /// made to one while it ran mixed.
    ReportEdits { paths: Vec<std::path::PathBuf> },
    /// The changes `changes` names are not its agent's, and for no thread.
    UnclaimChanges {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space: Option<SpaceSelector>,
        changes: Vec<ChangeNumber>,
    },
}

impl ChangeMethod {
    pub(super) fn reads_only(&self) -> bool {
        match self {
            Self::ListChanges { .. } | Self::ChangeDiffs { .. } => true,
            Self::RecordChange { .. }
            | Self::ClaimChanges { .. }
            | Self::ReportEdits { .. }
            | Self::UnclaimChanges { .. } => false,
        }
    }
}

impl ChangeMethod {
    /// A hook records its own agent's changes where it runs; the others go
    /// to the machine of the space whose worktree the changes are in.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::RecordChange { .. } | Self::ReportEdits { .. } => Forwarding::Never,
            Self::ListChanges { .. }
            | Self::ChangeDiffs { .. }
            | Self::ClaimChanges { .. }
            | Self::UnclaimChanges { .. } => Forwarding::Online,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::RecordChange { .. } | Self::ReportEdits { .. } => None,
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
