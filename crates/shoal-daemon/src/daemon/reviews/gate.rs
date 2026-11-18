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
    reviews::{Review, ReviewState, ReviewTarget},
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
    /// unless a finished review covers `head`, the commit it acts on, with
    /// every finding resolved, saying what is missing; without a review of
    /// it and with none queued or running, queue one.
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
        let (id, covering) = (workspace.id.clone(), head.to_owned());
        let (reviewed, active) = self
            .store
            .run(move |db| {
                Ok((
                    store::finished_at(db, &id, &covering)?,
                    store::active(db, &id)?,
                ))
            })
            .await?;
        let mut refusal = match &reviewed {
            Some(review) if review.unresolved().next().is_none() => return Ok(()),
            Some(review) => unresolved(review),
            None => format!(
                "{} needs a finished review of HEAD {} first",
                gate.command(),
                head.short()
            ),
        };
        match active {
            Some((id, state)) => {
                let state = if state == ReviewState::Queued {
                    "queued"
                } else {
                    "running"
                };
                let _ = write!(
                    refusal,
                    "; review {id} is {state}, and shoal watch review {id} waits for it"
                );
            }
            None if reviewed.is_none() => {
                match self
                    .queue_review(
                        &SpaceSelector::id(&workspace.id),
                        ReviewTarget::Workspace,
                        None,
                        Default::default(),
                    )
                    .await
                {
                    Ok(review) => {
                        let _ = write!(
                            refusal,
                            "; queued review {}, whose findings arrive as a message",
                            review.id
                        );
                    }
                    Err(error) => {
                        let _ = write!(refusal, "; no review could be queued: {error:#}");
                    }
                }
            }
            None => {}
        }
        bail!(refusal)
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

/// The refusal for a review of HEAD whose findings are not all resolved,
/// listing them.
fn unresolved(review: &Review) -> String {
    let mut text = format!(
        "review {} of HEAD has unresolved findings; resolve each with shoal review resolve:",
        review.id
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
