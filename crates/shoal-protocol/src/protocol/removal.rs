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
    /// The spaces `stop --all` stops.
    StopTargets,
    /// Stop the agents and commands of `space`, saving them for
    /// `shoal resume`.
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
    /// Refuse now what stopping `space` would refuse, so that a batch
    /// checks every target before stopping any.
    CheckStop {
        space: SpaceSelector,
    },
    /// Stop the untracked processes of `space` that `selection` takes,
    /// other than process `requester` and the processes that started it.
    StopProcesses {
        space: SpaceSelector,
        selection: crate::processes::Selection,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requester: Option<u32>,
    },
    CheckRemoval {
        space: SpaceSelector,
        #[serde(default)]
        include_changes: bool,
    },
    /// Remove `space` after the spaces below it, deleting only the
    /// `unsaved` work the caller saw.
    RemoveSpace {
        space: SpaceSelector,
        unsaved: UnsavedWork,
        /// Leave the spaces the user holds, as when the removal of a space
        /// above it on another machine came from a space's process.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        keep_user_holds: bool,
        /// The cause of the removal of the space above it on another
        /// machine, which records this removal as its child.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        above: Option<crate::model::EventCause>,
    },
    /// Remove `space` as `RemoveSpace` does, answering once removal has
    /// reserved it and passed its checks. The rest goes on in the
    /// background.
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
    /// The removed spaces the caller may restore.
    RemovedSpaces,
    /// The removed space `space` names, once the caller may restore it.
    RemovedSpace {
        space: SpaceSelector,
    },
    /// The removed space `space` names by ID, with the machine that keeps
    /// its record. Refused where the caller may not restore it.
    LocateRemovedSpace {
        space: SpaceSelector,
    },
    RestoreSpace {
        space: SpaceSelector,
    },
    /// Delete the agents' session files the restore that opened `space`
    /// returned, once the caller has put them in place.
    DropRestoredSessions {
        space: SpaceSelector,
    },
    /// Write the removal record of `space`, with those of the spaces its
    /// removal took that `carrying` carries, to the new file `file`. The
    /// record stays here.
    ExportRemovedSpace {
        space: SpaceSelector,
        file: std::path::PathBuf,
        #[serde(default)]
        carrying: crate::transfer::Carrying,
    },
    /// Write a copy of a space, with copies of the spaces below it that
    /// `carrying` carries, to the new file `file`, leaving them running. A
    /// workspace is copied at its checkpoint `checkpoint`, or at a new one.
    CopySpace {
        space: SpaceSelector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkpoint: Option<shoal_ids::checkpoint_numbers::CheckpointNumber>,
        file: std::path::PathBuf,
        #[serde(default)]
        carrying: crate::transfer::Carrying,
    },
    /// Record the move of `space` and the spaces below it that `carrying`
    /// carries to `destination`, then stop their agents and write their
    /// package. The move may delete the `unsaved` work the caller saw. The
    /// spaces stay here until [`Self::EndMovedSpace`].
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
    /// End `space` here with the spaces below it that its move carries,
    /// once the destination has their package. The move ends either way.
    EndMovedSpace {
        space: SpaceSelector,
    },
    /// Forget the failed move of `space`, answering with the executions
    /// whose work its spaces saved when the move stopped their agents.
    AbandonMove {
        space: SpaceSelector,
    },
    /// Import a package another Shoal exported. Once the whole package
    /// checks out, its spaces are recorded as removed here and its own space
    /// opens again.
    ImportPackage(Box<crate::transfer::PackageImport>),
    /// Deliver the package of the move of `space` to the machine it goes
    /// to, through the hub. Refused when it is unknown whether that machine
    /// claimed the space.
    DeliverMove {
        space: SpaceSelector,
    },
    /// Deliver the package of a copy of `space` to the machine `destination`
    /// names, through the hub.
    DeliverCopy {
        space: SpaceSelector,
        destination: crate::transfer::MachineDestination,
    },
    /// The package of `space` held here for the machine that started its
    /// move, or with `copy` a copy's, which goes once read. Answered with
    /// its size, then its bytes on the request's connection.
    ReadHeldPackage {
        space: SpaceSelector,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        copy: bool,
    },
    /// Give back the move of `space` whose package the machine that started
    /// it could not take. `space` stays here, its agents' restores queued.
    ReleaseHeldMove {
        space: SpaceSelector,
    },
    /// Ask the hub whether the machine the move of `space` goes to claimed
    /// the space. When it did not, it no longer can.
    SettleMove {
        space: SpaceSelector,
    },
    /// The removal records of the spaces the removal of `space` took that
    /// `carrying` carries, which its record carries on a move.
    TakenRecords {
        space: SpaceSelector,
        #[serde(default)]
        carrying: crate::transfer::Carrying,
    },
    /// Deliver the removal record of `space`, with those of the spaces its
    /// removal took that `carrying` carries, to the machine `destination`
    /// names. Their claims at the hub carry `ticket`. This machine forgets
    /// the records that machine recorded.
    DeliverRemovedSpace {
        space: SpaceSelector,
        destination: crate::transfer::MachineDestination,
        #[serde(default)]
        carrying: crate::transfer::Carrying,
        ticket: shoal_ids::move_tickets::MoveTicket,
    },
    /// Settle at the hub the move `ticket` of the removal records of
    /// `space` and `carried` to `machine`, whose arrival it could not
    /// report. This machine forgets each record that machine claimed.
    SettleRemovedMove {
        space: SpaceSelector,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        carried: Vec<shoal_ids::space_ids::SpaceId>,
        machine: String,
        ticket: shoal_ids::move_tickets::MoveTicket,
    },
    /// Take the package of a move that another machine sends on the
    /// request's connection once the first reply asks for it, and continue
    /// its spaces here as `shoal restore --from -` does. Their claims at the
    /// hub carry `ticket`, which a copy lacks. With `removed`, the package
    /// holds removal records, which are only recorded here.
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
    pub(super) fn reads_only(&self) -> bool {
        match self {
            Self::CheckRemoval { .. }
            | Self::CheckStop { .. }
            | Self::RemovedSpaces
            | Self::RemovedSpace { .. }
            | Self::LocateRemovedSpace { .. } => true,
            Self::StopTargets
            | Self::StopSpace { .. }
            | Self::StopProcesses { .. }
            | Self::RemoveSpace { .. }
            | Self::StartRemoval { .. }
            | Self::AbsorbSpace(_)
            | Self::Cleanup { .. }
            | Self::RestoreSpace { .. }
            | Self::DropRestoredSessions { .. }
            | Self::ExportRemovedSpace { .. }
            | Self::CopySpace { .. }
            | Self::ExportMovingSpace { .. }
            | Self::EndMovedSpace { .. }
            | Self::AbandonMove { .. }
            | Self::ImportPackage(_)
            | Self::DeliverMove { .. }
            | Self::DeliverCopy { .. }
            | Self::ReadHeldPackage { .. }
            | Self::ReleaseHeldMove { .. }
            | Self::SettleMove { .. }
            | Self::TakenRecords { .. }
            | Self::DeliverRemovedSpace { .. }
            | Self::SettleRemovedMove { .. }
            | Self::ReceiveMove { .. } => false,
        }
    }
}

impl RemovalMethod {
    /// A request about a running space goes to its machine, and a stop
    /// waits for one that is offline. A removed space's record is read
    /// where it is kept. An absorb needs both worktrees, so it runs here, as
    /// do a restore, an export, cleanup and the other steps of a move.
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
