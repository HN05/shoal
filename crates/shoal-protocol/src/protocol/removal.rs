use crate::removal::UnsavedWork;
use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};

/// What a stop at the user's request does with the agent launches waiting
/// to start in the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueuedLaunches {
    /// Cancel them, as `shoal stop` does, so no agent starts in a workspace
    /// the user stopped.
    Cancel,
    /// Leave them queued, as `shoal daemon stop` and `shoal repo move` do,
    /// which start the stopped work again later.
    Keep,
}

/// What a stop at the user's request does with the workspace's processes
/// Shoal does not track, which nothing saves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UntrackedProcesses {
    /// Stop them, as `shoal stop` does, other than process `requester` and
    /// the processes that started it.
    Stop {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requester: Option<u32>,
    },
    /// Leave them running, as `shoal repo move` does, which starts only the
    /// stopped agents and commands again.
    Keep,
}

/// Requests that stop, remove, clean up, export and restore spaces.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalMethod {
    /// The workspaces and chats `stop --all` stops.
    StopTargets,
    /// Stop the agents and commands of the workspace or chat `space`,
    /// saving them for `shoal restore`.
    StopSpace {
        space: SpaceSelector,
        /// The caller confirmed that executions Shoal cannot prove ended did.
        #[serde(default)]
        acknowledge_unproven: bool,
        /// What becomes of the agent launches waiting to start in it, which
        /// the answer lists when the stop cancels them.
        launches: QueuedLaunches,
        /// What becomes of its untracked processes, which the answer counts.
        processes: UntrackedProcesses,
    },
    /// Refuse now what stopping the workspace or chat `space` would refuse,
    /// so a batch checks every target before stopping any.
    CheckStop { space: SpaceSelector },
    /// Stop the untracked processes of the workspace or chat `space` that
    /// `selection` takes, other than process `requester` and the processes
    /// that started it.
    StopProcesses {
        space: SpaceSelector,
        selection: crate::processes::Selection,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requester: Option<u32>,
    },
    /// What removing the workspace or chat `space` would do, with its
    /// worktree's changed files when `include_changes`.
    CheckRemoval {
        space: SpaceSelector,
        #[serde(default)]
        include_changes: bool,
    },
    /// Remove the workspace or chat `space`, stopping its agent, first the
    /// spaces below it, deleting only the `unsaved` work the caller saw.
    RemoveSpace {
        space: SpaceSelector,
        unsaved: UnsavedWork,
        /// Leave the spaces the user holds, as when the removal of a space
        /// above it on another machine came from a space's process.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        keep_user_holds: bool,
        /// The cause of the removal of the space above it on another
        /// machine, which takes it as that removal's child, as a removal
        /// takes a child here; none for a removal asked of it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        above: Option<crate::model::EventCause>,
    },
    /// Remove `space` as `RemoveSpace` does, answering once removal has
    /// reserved it and passed the checks it makes before changing anything,
    /// while the rest goes on in the background.
    StartRemoval {
        space: SpaceSelector,
        unsaved: UnsavedWork,
    },
    /// Fold a space into one above it, which takes its work and records,
    /// then remove it as `StartRemoval` does.
    AbsorbSpace(Box<crate::absorb::Absorb>),
    /// Remove idle and merged cleanup candidates without waiting for their
    /// idle delay, and empty shared caches nothing is using.
    Cleanup {
        dry_run: bool,
        #[serde(flatten)]
        selection: crate::cleanup::Selection,
    },
    /// Removed spaces that can still be restored, those the caller may.
    RemovedSpaces,
    /// The removed space `space` names, once the caller may restore it.
    RemovedSpace { space: SpaceSelector },
    /// Open a removed space again from its removal record.
    RestoreSpace { space: SpaceSelector },
    /// Delete the agents' session files the restore that opened `space`
    /// returned, once the caller has put them in place.
    DropRestoredSessions { space: SpaceSelector },
    /// End a space here through the removal path, with the spaces below it
    /// that `carrying` carries, or take a removed space with those its
    /// removal took, and write them for another Shoal to the new file
    /// `file`. The spaces below that it does not carry stay here.
    ExportSpace {
        space: SpaceSelector,
        unsaved: UnsavedWork,
        file: std::path::PathBuf,
        #[serde(default)]
        carrying: crate::transfer::Carrying,
    },
    /// Stop the agents of the workspace or chat `space` and of the spaces
    /// below it that `carrying` carries, saving their sessions, and write
    /// them for another Shoal to the new file `file` without ending them
    /// here, as `shoal move --ssh` does before its import there.
    ExportMovingSpace {
        space: SpaceSelector,
        file: std::path::PathBuf,
        #[serde(default)]
        carrying: crate::transfer::Carrying,
    },
    /// End `space` here with the spaces below it that `carrying` carries,
    /// once another Shoal imported their move, deleting only the `unsaved`
    /// work the caller saw: each workspace only while it still has what
    /// its exported checkpoint in `checkpoints` keeps.
    EndMovedSpace {
        space: SpaceSelector,
        unsaved: UnsavedWork,
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        checkpoints: std::collections::BTreeMap<
            shoal_ids::space_ids::SpaceId,
            shoal_ids::checkpoint_numbers::CheckpointNumber,
        >,
        #[serde(default)]
        carrying: crate::transfer::Carrying,
    },
    /// Record a space another Shoal exported as removed here, keeping its
    /// agents' session files with the record, and open it again.
    ImportSpace(Box<crate::transfer::SpaceImport>),
}
