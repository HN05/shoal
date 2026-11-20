//! A space's assignment: marking it done or ready for review, and
//! discarding the stopped agent it leaves.
use anyhow::Result;
use shoal_protocol::assignments::Done;

use crate::{
    connection::{Daemon, request},
    forge::items::link::Selection,
};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    executions::DiscardedAgent,
    model::{Completion, ReadyOutcome, ReviewMark},
    protocol::AssignmentMethod,
};

/// Mark `space`'s assignment done as `done` describes.
pub async fn mark_done(
    daemon: &impl Daemon,
    space: SpaceSelector,
    done: Done,
) -> Result<Completion> {
    let method = AssignmentMethod::MarkDone { space, done };
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

/// Discard `space`'s stopped agent, last sessions and stopped commands,
/// only `execution`'s when given.
pub async fn discard_stopped_agent(
    daemon: &impl Daemon,
    space: SpaceSelector,
    execution: Option<ExecutionId>,
) -> Result<DiscardedAgent> {
    let method = AssignmentMethod::DiscardStoppedAgent { space, execution };
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
