//! Review gates: with `[review] require_for_pr`, `require_for_ready` or
//! `require_for_merge` holding the caller, it opens a new PR, marks work
//! ready or merges only once a finished review covers the commit it acts on
//! and every finding is resolved.
use shoal_refs::CommitId;
use std::fmt::Write as _;

use anyhow::{Result, bail};

use super::store;
use crate::{
    config::{Effective, repo::Requester},
    daemon::{scope::Caller, workspace::Manager},
    model::Workspace,
    reviews::{Review, ReviewId, ReviewState, ReviewTarget},
};
use shoal_protocol::space_names::SpaceSelector;

/// What a gate guards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    /// `shoal pr open` of a new PR.
    Pr,
    /// `shoal ready`.
    Ready,
    /// `shoal merge`.
    Merge,
}

impl Manager {
    /// Refuse `caller`, where the workspace's setting for `gate` holds it,
    /// as [`Self::require_reviewed`] does.
    pub(crate) async fn require_review(
        &self,
        workspace: &Workspace,
        gate: Gate,
        head: &CommitId,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let settings = self.workspace_settings(workspace).await?;
        if !gate.holds(&settings, caller) {
            return Ok(());
        }
        self.require_reviewed(workspace, gate, head).await
    }

    /// Refuse unless a finished review of the workspace covers `head`, the
    /// commit a command acts on, with every finding resolved, saying what is
    /// missing; without a review of it and with none queued or running,
    /// queue one.
    pub(crate) async fn require_reviewed(
        &self,
        workspace: &Workspace,
        gate: Gate,
        head: &CommitId,
    ) -> Result<()> {
        let (id, covering) = (workspace.id.clone(), head.to_owned());
        let (reviewed, active) = self
            .store
            .read(move |tx| {
                Ok((
                    store::finished_at(tx, &id, &covering)?,
                    store::active(tx, &id)?,
                ))
            })
            .await?;
        let missing = match &reviewed {
            Some(review) if review.unresolved().next().is_none() => return Ok(()),
            Some(review) => unresolved(review, head),
            None => format!(
                "{} needs a finished review of {} first",
                gate.command(),
                head.short()
            ),
        };
        let next = match active {
            Some((id, state)) => pending(id, state),
            None if reviewed.is_none() => self.queue_gate_review(workspace).await,
            None => String::new(),
        };
        bail!("{missing}{next}")
    }

    /// Queue a review of the workspace for a gate that found none, saying
    /// how it went.
    async fn queue_gate_review(&self, workspace: &Workspace) -> String {
        let selector = SpaceSelector::id(&workspace.id);
        let queued =
            self.queue_review(&selector, ReviewTarget::Workspace, None, Default::default());
        match queued.await {
            Ok(review) => format!(
                "; queued review {}, whose findings arrive as a message",
                review.id
            ),
            Err(error) => format!("; no review could be queued: {error:#}"),
        }
    }
}

impl Gate {
    /// Whether `settings` hold `caller` to the gate.
    pub(crate) fn holds(self, settings: &Effective, caller: Option<&Caller>) -> bool {
        let required = match self {
            Self::Pr => settings.review.require_for_pr,
            Self::Ready => settings.review.require_for_ready,
            Self::Merge => settings.review.require_for_merge,
        };
        required.holds(requester(caller))
    }

    fn command(self) -> &'static str {
        match self {
            Self::Pr => "opening a PR",
            Self::Ready => "marking work ready",
            Self::Merge => "merging",
        }
    }
}

/// Who `caller` stands for: an agent when scoped, else the user.
fn requester(caller: Option<&Caller>) -> Requester {
    match caller {
        Some(_) => Requester::Agent,
        None => Requester::User,
    }
}

/// What a refusal adds for review `id`, queued or running in `state`.
fn pending(id: ReviewId, state: ReviewState) -> String {
    let state = match state {
        ReviewState::Queued => "queued",
        _ => "running",
    };
    format!("; review {id} is {state}, and shoal watch review {id} waits for it")
}

/// The refusal for a review of `head` whose findings are not all
/// resolved, listing them.
fn unresolved(review: &Review, head: &CommitId) -> String {
    let mut text = format!(
        "review {} of {} has unresolved findings; resolve each with shoal review resolve:",
        review.id,
        head.short()
    );
    for finding in review.unresolved() {
        let _ = write!(
            text,
            "\n  {} {} at {}:{}: {}",
            finding.id, finding.severity, finding.path, finding.line, finding.title
        );
    }
    text
}
