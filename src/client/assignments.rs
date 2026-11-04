//! A space's assignment: marking it done or ready for review, and
//! forgetting the stopped work it leaves.
use anyhow::Result;

use crate::{
    client::{
        connection::{Daemon, request},
        executions::Discarded,
        files::NewFile,
    },
    forge::items::link::Selection,
    model::{Completion, ReadyOutcome, ReviewMark},
    protocol::AssignmentMethod,
};

/// Mark `space`'s assignment done, with `cleanup` removing it once nothing
/// keeps it; `integrated` says another workspace took its commits, and
/// `results` are the files it hands back. The chat `sender_chat` names, when
/// its agent completes, attaches its files.
pub async fn mark_done(
    daemon: &impl Daemon,
    space: String,
    cleanup: bool,
    integrated: bool,
    results: Vec<NewFile>,
    sender_chat: Option<String>,
) -> Result<Completion> {
    let method = AssignmentMethod::MarkDone {
        space,
        cleanup,
        integrated,
        results,
        sender_chat,
    };
    request(daemon, method).await
}

/// Withdraw `space`'s completion.
pub async fn undo_done(daemon: &impl Daemon, space: String) -> Result<Completion> {
    request(daemon, AssignmentMethod::UndoDone { space }).await
}

/// Mark the items of `space` that `selection` selects ready for review.
pub async fn mark_ready(
    daemon: &impl Daemon,
    space: String,
    selection: Selection,
) -> Result<Vec<ReadyOutcome>> {
    request(daemon, AssignmentMethod::MarkReady { space, selection }).await
}

/// Clear the ready-for-review marks of the items of `space` that
/// `selection` selects; `draft` overrides `[pr] draft_on_unready`.
pub async fn clear_ready(
    daemon: &impl Daemon,
    space: String,
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
pub async fn discard_stopped_work(
    daemon: &impl Daemon,
    workspace: String,
    execution: Option<String>,
) -> Result<Discarded> {
    let method = AssignmentMethod::DiscardStoppedWork {
        workspace,
        execution,
    };
    request(daemon, method).await
}
