use super::{Forwarding, Target};
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
    /// saving them for `shoal resume`.
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
    /// The removed space `space` names by ID, with where its record is
    /// kept: here, or on the other joined machine that keeps it, which is
    /// asked for it; refused where the caller may not restore it.
    LocateRemovedSpace { space: SpaceSelector },
    /// Open a removed space again from its removal record.
    RestoreSpace { space: SpaceSelector },
    /// Delete the agents' session files the restore that opened `space`
    /// returned, once the caller has put them in place.
    DropRestoredSessions { space: SpaceSelector },
    /// Write the removal record of the removed space `space` names by ID,
    /// which stays, with those of the spaces its removal took that
    /// `carrying` carries, for another Shoal to the new file `file`.
    ExportRemovedSpace {
        space: SpaceSelector,
        file: std::path::PathBuf,
        #[serde(default)]
        carrying: crate::transfer::Carrying,
    },
    /// Write a copy of a workspace or chat, with copies of the spaces below
    /// it that `carrying` carries, for another Shoal to the new file
    /// `file`, leaving them running: a workspace at its checkpoint
    /// `checkpoint`, or at a new one, and those below at new ones.
    CopySpace {
        space: SpaceSelector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkpoint: Option<shoal_ids::checkpoint_numbers::CheckpointNumber>,
        file: std::path::PathBuf,
        #[serde(default)]
        carrying: crate::transfer::Carrying,
    },
    /// Record the move of the workspace or chat `space` and the spaces
    /// below it that `carrying` carries to `destination`, letting it delete
    /// the `unsaved` work the caller saw, then stop their agents, saving
    /// their sessions, and write their package for another Shoal to the
    /// new file the destination takes, without ending them here.
    ExportMovingSpace {
        space: SpaceSelector,
        unsaved: UnsavedWork,
        destination: crate::transfer::MoveDestination,
        #[serde(default)]
        carrying: crate::transfer::Carrying,
        /// Pack again under the move's record, as only the move's finisher
        /// does; any other move of a space with one is refused.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        again: bool,
    },
    /// End `space` here with the spaces below it that its recorded move
    /// carries, once the destination has their package, as the move's
    /// record allows; the move ends either way.
    EndMovedSpace { space: SpaceSelector },
    /// Forget the recorded move of `space`, which failed, answering with the
    /// executions whose stopped work its spaces saved once it stopped their
    /// agents.
    AbandonMove { space: SpaceSelector },
    /// Read a package another Shoal exported, cloning the repositories it
    /// needs that are not here, record its spaces as removed here once the
    /// whole package checks out, and open its own space again.
    ImportPackage(Box<crate::transfer::PackageImport>),
    /// Deliver the package of the recorded move of `space` to the joined
    /// machine it goes to, through the hub; answered with how the import
    /// there went, and refused when it is unknown whether that machine
    /// claimed the space.
    DeliverMove { space: SpaceSelector },
    /// Deliver the package of a copy of the space `space` to the joined
    /// machine `destination` names, through the hub; answered with how the
    /// import there went.
    DeliverCopy {
        space: SpaceSelector,
        destination: crate::transfer::MachineDestination,
    },
    /// The package of `space` held here for the joined machine that
    /// started its move: the recorded move's, or with `copy` a copy's or,
    /// for a removed space, its record's, which goes once read; answered
    /// with its size, then its bytes on the request's connection.
    ReadHeldPackage {
        space: SpaceSelector,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        copy: bool,
    },
    /// Give back the recorded move of `space` that holds its package here
    /// for the joined machine that started it, which could not take it:
    /// `space` stays here, its agents' restores queued.
    ReleaseHeldMove { space: SpaceSelector },
    /// Ask the hub whether the joined machine the recorded move of `space`
    /// goes to claimed the space, which then no longer can when it did
    /// not; answered with how the hub settled it.
    SettleMove { space: SpaceSelector },
    /// The removal records of the spaces the removal of `space` took that
    /// `carrying` carries, which its record carries on a move.
    TakenRecords {
        space: SpaceSelector,
        #[serde(default)]
        carrying: crate::transfer::Carrying,
    },
    /// Deliver the removal record of `space`, with those of the spaces its
    /// removal took that `carrying` carries, to the joined machine
    /// `destination` names, which records them as removed there, their
    /// claims at the hub carrying `ticket`, and forget here those it
    /// recorded; answered with their package and how its delivery went.
    DeliverRemovedSpace {
        space: SpaceSelector,
        destination: crate::transfer::MachineDestination,
        #[serde(default)]
        carrying: crate::transfer::Carrying,
        ticket: shoal_ids::move_tickets::MoveTicket,
    },
    /// Settle at the hub the move `ticket` of the removal record of
    /// `space`, with the records of the spaces `carried`, to the joined
    /// machine `machine` names, whose arrival that machine could not
    /// report, and forget here each record that machine claimed; answered
    /// with how the hub settled the space's own.
    SettleRemovedMove {
        space: SpaceSelector,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        carried: Vec<shoal_ids::space_ids::SpaceId>,
        machine: String,
        ticket: shoal_ids::move_tickets::MoveTicket,
    },
    /// Take the package of a move another joined machine sends on the
    /// request's connection once the first reply asks for it, and continue
    /// its spaces here as `shoal restore --from -` does, their claims at
    /// the hub carrying `ticket`, which a copy lacks, with their agents'
    /// restores queued when `resume`, and tell their agents that the spaces
    /// moved here from the machine named `from`, or when `removed`, a
    /// removed space's package, only record them as removed here; answered
    /// with how the import went.
    ReceiveMove {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ticket: Option<shoal_ids::move_tickets::MoveTicket>,
        resume: bool,
        from: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        removed: bool,
    },
}

impl RemovalMethod {
    /// A stop goes to the space's machine, waiting for it while it is
    /// offline, and a removal there too, as does the end of a move another
    /// machine took the package of, the read of a package it holds and its
    /// return, and a removed space's record is read where it is kept; an
    /// absorb needs both worktrees, and a restore, the other steps of a
    /// move, an export and the caller's cleanup run where the caller does.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::StopSpace { .. } => Forwarding::Queued,
            Self::CheckStop { .. }
            | Self::StopProcesses { .. }
            | Self::CheckRemoval { .. }
            | Self::RemoveSpace { .. }
            | Self::StartRemoval { .. }
            | Self::EndMovedSpace { .. }
            | Self::ReleaseHeldMove { .. } => Forwarding::Online,
            // The machine that keeps a record serves it when asked for it;
            // a lookup that fails here never names an absent space, so it
            // is answered here.
            Self::RemovedSpace { .. } => Forwarding::Online,
            // Raw bytes follow its answer, which only a duplex stream carries.
            Self::ReceiveMove { .. } | Self::ReadHeldPackage { .. } => Forwarding::Duplex,
            Self::StopTargets
            | Self::AbsorbSpace(_)
            | Self::Cleanup { .. }
            | Self::RemovedSpaces
            | Self::LocateRemovedSpace { .. }
            | Self::RestoreSpace { .. }
            | Self::DropRestoredSessions { .. }
            | Self::ExportRemovedSpace { .. }
            | Self::CopySpace { .. }
            | Self::ExportMovingSpace { .. }
            | Self::AbandonMove { .. }
            | Self::ImportPackage(_)
            | Self::DeliverMove { .. }
            | Self::DeliverCopy { .. }
            | Self::SettleMove { .. }
            | Self::TakenRecords { .. }
            | Self::DeliverRemovedSpace { .. }
            | Self::SettleRemovedMove { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::StopSpace { space, .. }
            | Self::CheckStop { space }
            | Self::StopProcesses { space, .. }
            | Self::CheckRemoval { space, .. }
            | Self::RemoveSpace { space, .. }
            | Self::StartRemoval { space, .. }
            | Self::CopySpace { space, .. }
            | Self::ExportMovingSpace { space, .. }
            | Self::EndMovedSpace { space }
            | Self::AbandonMove { space }
            | Self::DeliverMove { space }
            | Self::DeliverCopy { space, .. }
            | Self::ReleaseHeldMove { space }
            | Self::SettleMove { space } => Some(Target::Space(space)),
            // A removed space's record exports where it is kept.
            Self::ReadHeldPackage { space, .. } => Some(Target::Removable(space)),
            Self::AbsorbSpace(absorb) => Some(Target::Space(&absorb.space)),
            // A removed space's record is this machine's, whichever runs
            // the space now.
            Self::RemovedSpace { .. }
            | Self::RestoreSpace { .. }
            | Self::DropRestoredSessions { .. }
            | Self::ExportRemovedSpace { .. }
            | Self::TakenRecords { .. }
            | Self::DeliverRemovedSpace { .. }
            | Self::SettleRemovedMove { .. } => None,
            // It asks the machine that keeps the record itself.
            Self::LocateRemovedSpace { .. } => None,
            // The hub sends it to the machine a move goes to.
            Self::ReceiveMove { .. }
            | Self::StopTargets
            | Self::Cleanup { .. }
            | Self::RemovedSpaces
            | Self::ImportPackage(_) => None,
        }
    }
}
