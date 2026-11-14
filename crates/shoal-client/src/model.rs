//! Records shared by the daemon, its SQLite store, and CLI output.
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
mod spaces;
mod status;
mod sync;
mod workspaces;

pub use agents::{AgentSession, AgentStatus, Asleep, PendingRestart, RunningAgent};
pub use chats::{Chat, ChatHome, RenamedChat, chat_mode};
pub use checkpoints::{Checkpoint, CheckpointCause, CheckpointNumber, RestoredCheckpoint};
pub use cleanup::CleanupReason;
pub use completion::{Completion, HandedBack};
pub use conflicts::{ChangedOnBothSides, ConfirmedWarning, ConflictCheck, DiffBase, MergeWarning};
pub use executions::{CopyIgnored, Execution, ExecutionPlan, ExecutionSpace, OwnedPlace};
pub use holds::{HoldPlacer, WorkspaceHold};
pub use links::{
    Assigner, IssueAssignee, LinkedItem, Relation, Selected, SelectedItem, SpaceLinks,
};
pub use merges::{
    MergeConfirmation, MergeOptions, MergePlan, MergePreview, MergePush, MergedBranch,
    PulledBranch, UnmergePlan, Unmerged,
};
pub use ports::{PortCatalog, PortOverview, PortRequest, PortReservation, PortSuggestion};
pub use queue::{
    Blocker, Dependency, HeldBy, InheritedRank, QueueAdmission, QueueEntry, QueueRank,
    QueueRequest, QueuedJob, QueuedPlan, SharedArea, StoppedSpace, Tier,
};
pub use ready::{ReadyMarks, ReadyOutcome, ReviewMark, SkippedPr};
pub use removal::{
    BranchTarget, KeptBranch, RemovedWorkspace, RestoredWorkspace, UntrackedWorktree,
};
pub use repositories::{
    LocalConfigEdit, MovedRepository, Repository, RepositoryMoves, RepositoryRemoval,
};
pub use spaces::{PresentSpace, SpaceKind, SpaceRef, SpaceTarget, Spaces};
pub use status::{
    Attention, DiffSummary, Inspection, IssueStatus, SpaceDetail, SpaceStatus, WorktreeStatus,
};
pub use sync::{CheckoutSync, SyncedRepository};
pub use workspaces::{
    Branch, CopiedFrom, MadeFor, OpenedWorkspace, RenamedWorkspace, StartingCommit, Workspace,
    WorkspaceRef, WorkspaceUsage, no_branch,
};
