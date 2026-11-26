//! The records the daemon keeps and the protocol carries.
mod agents;
mod chats;
mod checkpoints;
mod cleanup;
mod completion;
mod conflicts;
mod executions;
mod holds;
mod links;
mod merges;
mod ports;
mod queue;
mod ready;
mod removal;
mod repositories;
mod restarts;
mod resumes;
mod spaces;
mod sync;
mod workspaces;

pub use agents::{
    AgentSession, AgentStatus, Asleep, KeptKind, KeptLease, Protection, RunningAgent,
};
pub use chats::{Chat, ChatHome, ChatPart, chat_mode};
pub use checkpoints::{Checkpoint, CheckpointCause, RestoredCheckpoint};
pub use cleanup::CleanupReason;
pub use completion::{Completion, HandedBack};
pub use conflicts::{ChangedOnBothSides, ConfirmedWarning, ConflictCheck, DiffBase, MergeWarning};
pub use executions::{
    CopyIgnored, Execution, ExecutionPlan, ExecutionSpace, OwnedPlace, SpacePlaces,
};
pub use holds::{HoldPlacer, SpaceHold};
pub use links::{
    Assigner, IssueAssignee, LinkedItem, Relation, Selected, SelectedItem, SpaceLinks,
};
pub use merges::{
    MergeConfirmation, MergeOptions, MergePath, MergePlan, MergePreview, MergePush, MergedBranch,
    PulledBranch, UnmergePlan, Unmerged, message_subject,
};
pub use ports::{PortCatalog, PortOverview, PortRequest, PortReservation, PortSuggestion};
pub use queue::{
    HeldBy, QueueAdmission, QueueEntry, QueueRequest, QueuedJob, QueuedPlan, StoppedSpace,
};
pub use ready::{ReadyMarks, ReadyOutcome, ReviewMark, SkippedPr};
pub use removal::{
    BranchTarget, EventCause, KeptBranch, KeptChat, KeptWorkspace, LocatedRemoved, RemovedKind,
    RemovedSpace, RestoredChat, RestoredSpace, RestoredWorkspace, UntrackedWorktree,
};
pub use repositories::{
    LocalConfigEdit, MovedRepository, Repository, RepositoryMoves, RepositoryRemoval,
    RepositoryRemovalPreview, SpaceWork, WorktreeWork,
};
pub use restarts::{AgentRestart, RestartOutcome, RestartWait};
pub use resumes::{AgentResume, ResumeOutcome};
pub use spaces::{
    ByKind, Everywhere, EverywherePart, Located, Offer, Part, RefPart, RenamedSpace, Space,
    SpaceKind, SpaceMode, SpaceOffer, SpacePart, SpaceRef, SpaceTarget, Spaces, Workdir,
};
pub use sync::{CheckoutSync, SyncedRepository};
pub use workspaces::{
    Base, Branch, Checkout, CopiedFrom, Head, MadeFor, OpenedWorkspace, PreparationWait, Refreshed,
    StartingCommit, Workspace, WorkspaceRef, WorkspaceUsage, Worktree, no_branch,
};
