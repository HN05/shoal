//! A space's assignment: marking it done or ready for review, and
//! forgetting the stopped work it leaves.
use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::client::space_names::SpaceSelector;
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

/// What `shoal done` records besides the HEAD it binds to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Done {
    /// Remove the space once nothing keeps it, even when `[cleanup.done]
    /// enabled` is false.
    pub cleanup: bool,
    /// Another branch has HEAD's work under other commits; only a
    /// workspace has such a branch.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub integrated: bool,
    /// Files to hand back by absolute path, which the daemon copies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<NewFile>,
}

impl Done {
    /// Refuse what one completion cannot record, before it has any effect.
    pub fn check(&self) -> Result<()> {
        crate::client::files::check_results(&self.results)
    }
}

/// Mark `space`'s assignment done as `done` describes. The chat
/// `sender_chat` names, when its agent completes, attaches its files.
pub async fn mark_done(
    daemon: &impl Daemon,
    space: SpaceSelector,
    done: Done,
    sender_chat: Option<String>,
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
pub async fn discard_stopped_work(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    execution: Option<String>,
) -> Result<Discarded> {
    let method = AssignmentMethod::DiscardStoppedWork {
        workspace,
        execution,
    };
    request(daemon, method).await
}
