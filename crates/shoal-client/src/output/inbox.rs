//! What `shoal inbox` prints.
use serde::{Deserialize, Serialize};

use crate::output::outputs;
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::{
    approvals::ApprovalRequest,
    demos::Running,
    hub::directory::Listed,
    list_status::ListStatus,
    model::{Chat, CheckoutSync, QueueEntry, ReviewMark, UntrackedWorktree, Workspace},
    threads::Thread,
};

/// Something that needs the user.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
    /// A thread that waits for the user's reply.
    Thread {
        thread: Box<Thread>,
    },
    Approval {
        request: ApprovalRequest,
    },
    NeedsInput {
        workspace: Workspace,
    },
    Failed {
        workspace: Workspace,
    },
    /// A space whose agent's launch failed to start, kept as a failed
    /// entry of the agent queue.
    StartFailed {
        entry: QueueEntry,
        space: SpaceId,
        number: SpaceNumber,
    },
    /// A workspace whose progress records what blocks it.
    Blocked {
        workspace: Workspace,
    },
    /// A workspace whose agent Shoal stopped and brings back on its own.
    Asleep {
        workspace: Workspace,
    },
    Ready {
        workspace: Workspace,
        mark: ReviewMark,
    },
    /// A running chat whose agent needs input.
    ChatNeedsInput {
        chat: Chat,
    },
    /// A chat whose progress records what blocks it.
    ChatBlocked {
        chat: Chat,
    },
    /// A chat whose agent Shoal stopped and brings back on its own.
    ChatAsleep {
        chat: Chat,
    },
    /// A chat's mark on its linked work or on itself.
    ChatReady {
        chat: Chat,
        mark: ReviewMark,
    },
    /// A chat that recorded its work as finished.
    ChatDone {
        chat: Chat,
    },
    Demo {
        demo: Running,
    },
    /// A worktree with no workspace, so it belongs to no listed one.
    Untracked {
        worktree: UntrackedWorktree,
    },
    /// Automatic cleanup failing other than in a workspace, or stalled.
    Cleanup {
        error: String,
    },
    /// A repository whose default branch has not synced for a while.
    SyncFailed {
        sync: CheckoutSync,
    },
    /// Another joined machine's space that needs the user, as its machine
    /// last reported it.
    Elsewhere {
        space: Listed,
        need: Need,
    },
}

/// What another machine's space needs of the user.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(clippy::large_enum_variant, reason = "an inbox holds a few items")]
pub enum Need {
    Thread(Box<Thread>),
    Approval(ApprovalRequest),
    /// What it is doing, such as needing input or having failed.
    Status(ListStatus),
}

outputs!(Item);
