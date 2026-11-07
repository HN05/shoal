//! What `shoal inbox` prints.
use serde::{Deserialize, Serialize};

use crate::{
    approvals::ApprovalRequest,
    demos::Running,
    model::{Chat, CheckoutSync, ReviewMark, UntrackedWorktree, Workspace},
    output::outputs,
    questions::Question,
};

/// Something that needs the user.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
    Question {
        question: Question,
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
    Ready {
        workspace: Workspace,
        mark: ReviewMark,
    },
    /// A running chat whose agent needs input.
    ChatNeedsInput {
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
}

outputs!(Item);
