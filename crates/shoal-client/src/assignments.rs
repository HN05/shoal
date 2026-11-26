//! Requests about a space's assignment and the stopped agents it leaves.
use anyhow::Result;
use shoal_protocol::assignments::Done;

use crate::{
    connection::{Daemon, Prepared, request},
    forge::items::link::Selection,
};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    executions::DiscardedAgent,
    model::{Completion, ReadyOutcome, ReviewMark},
    protocol::AssignmentMethod,
};

pub fn mark_done(space: SpaceSelector, done: Done) -> Prepared<Completion> {
    Prepared::new(AssignmentMethod::MarkDone { space, done })
}

pub fn undo_done(space: SpaceSelector) -> Prepared<Completion> {
    Prepared::new(AssignmentMethod::UndoDone { space })
}

/// Mark the items of `space` that `selection` selects ready for review.
pub fn mark_ready(space: SpaceSelector, selection: Selection) -> Prepared<Vec<ReadyOutcome>> {
    Prepared::new(AssignmentMethod::MarkReady { space, selection })
}

/// Clear the ready-for-review marks of the items of `space` that
/// `selection` selects; `draft` overrides `[pr] draft_on_unready`.
pub fn clear_ready(
    space: SpaceSelector,
    selection: Selection,
    draft: Option<bool>,
) -> Prepared<Vec<ReviewMark>> {
    Prepared::new(AssignmentMethod::ClearReady {
        space,
        selection,
        draft,
    })
}

/// Discard `space`'s stopped agent, last sessions and stopped commands,
/// leaving an agent a wrapper waits to restore, and its failed launch; only
/// `execution`'s agent when given.
pub async fn discard_stopped_agent(
    daemon: &impl Daemon,
    space: SpaceSelector,
    execution: Option<ExecutionId>,
) -> Result<DiscardedAgent> {
    let method = AssignmentMethod::DiscardStoppedAgent { space, execution };
    request(daemon, method).await
}

/// Applies `change` to the permissions that each of the stopped `agents`,
/// the saved `chats` and the launches waiting for the failed setup of the
/// `launches` workspaces was given. The daemon changes none when it refuses
/// one.
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
