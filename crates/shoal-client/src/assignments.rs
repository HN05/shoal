//! A space's assignment: marking it done or ready for review, and
//! forgetting the stopped work it leaves.
use crate::execution_ids::ExecutionId;
use crate::space_ids::SpaceId;
use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::space_names::SpaceSelector;
use crate::{
    connection::{Daemon, request},
    executions::Discarded,
    files::NewFile,
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
    /// One line saying what the work found, which the completion notice
    /// ends with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl Done {
    /// Refuse what one completion cannot record, before it has any effect.
    pub fn check(&self) -> Result<()> {
        crate::files::check_results(&self.results)?;
        if let Some(summary) = &self.summary {
            crate::validate::message(summary).context("invalid summary")?;
        }
        Ok(())
    }
}

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
pub async fn discard_stopped_work(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    execution: Option<ExecutionId>,
) -> Result<Discarded> {
    let method = AssignmentMethod::DiscardStoppedWork {
        workspace,
        execution,
    };
    request(daemon, method).await
}

/// Give each of the stopped `agents` `narrowing` in place of the one it
/// recorded, which its restore runs under; the daemon replaces none when it
/// refuses one.
pub async fn replace_stopped_narrowing(
    daemon: &impl Daemon,
    agents: Vec<crate::protocol::SavedAgent>,
    narrowing: crate::config::permissions::PermissionsConfig,
) -> Result<()> {
    let method = AssignmentMethod::ReplaceStoppedNarrowing { agents, narrowing };
    request(daemon, method).await
}
