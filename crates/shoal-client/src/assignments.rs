//! A space's assignment: marking it done or ready for review, and
//! forgetting the stopped work it leaves.
use anyhow::Result;
use shoal_protocol::assignments::Done;

use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use crate::{
    connection::{Daemon, request},
    forge::items::link::Selection,
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    executions::Forgotten,
    model::{Completion, ReadyOutcome, ReviewMark},
    protocol::AssignmentMethod,
};

/// Mark `space`'s assignment done as `done` describes. The chat
/// `sender_chat` names, when its agent completes, attaches its files.
pub async fn mark_done(
    daemon: &impl Daemon,
    space: SpaceSelector,
    done: Done,
    sender_chat: Option<SpaceId>,
) -> Result<Completion> {
    let method = AssignmentMethod::MarkDone {
        space,
        done,
        sender_chat,
    };
    request(daemon, method).await
}

/// Withdraw `space`'s completion.
pub async fn undo_done(daemon: &impl Daemon, space: SpaceSelector) -> Result<Completion> {
    request(daemon, AssignmentMethod::UndoDone { space }).await
}

/// Mark the items of `space` that `selection` selects ready for review.
pub async fn mark_ready(
    daemon: &impl Daemon,
    space: SpaceSelector,
    selection: Selection,
) -> Result<Vec<ReadyOutcome>> {
    request(daemon, AssignmentMethod::MarkReady { space, selection }).await
}

/// Clear the ready-for-review marks of the items of `space` that
/// `selection` selects; `draft` overrides `[pr] draft_on_unready`.
pub async fn clear_ready(
    daemon: &impl Daemon,
    space: SpaceSelector,
    selection: Selection,
    draft: Option<bool>,
) -> Result<Vec<ReviewMark>> {
    let method = AssignmentMethod::ClearReady {
        space,
        selection,
        draft,
    };
    request(daemon, method).await
}

/// Forget `workspace`'s stopped work and last sessions, only `execution`'s
/// when given.
pub async fn forget_stopped_work(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    execution: Option<ExecutionId>,
) -> Result<Forgotten> {
    let method = AssignmentMethod::ForgetStoppedWork {
        workspace,
        execution,
    };
    request(daemon, method).await
}

/// Make `change` to the permissions each of the stopped `agents` and saved
/// `chats` was given, which its restore runs under, and the launch waiting
/// for the failed setup of each of the `launches` workspaces; the daemon
/// changes none when it refuses one.
pub async fn change_stopped_permissions(
    daemon: &impl Daemon,
    agents: Vec<shoal_protocol::protocol::SavedAgent>,
    launches: Vec<SpaceSelector>,
    change: shoal_protocol::config::permissions::PermissionChange,
) -> Result<()> {
    let method = AssignmentMethod::ChangeStoppedPermissions {
        agents,
        launches,
        change,
    };
    request(daemon, method).await
}
