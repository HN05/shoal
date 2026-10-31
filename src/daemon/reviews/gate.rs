//! Review gates: with `[review] require_for_pr` or `require_for_ready`, a
//! workspace process opens a new PR or marks work ready only once a
//! finished review covers HEAD and every finding is resolved.
use std::fmt::Write as _;

use anyhow::{Result, bail};

use super::store;
use crate::{
    daemon::workspace::Manager,
    model::Workspace,
    reviews::{Review, ReviewState, ReviewTarget},
};

/// What a gate guards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    /// `shoal pr open` of a new PR.
    Pr,
    /// `shoal ready`.
    Ready,
}

impl Manager {
    /// Refuse unless a finished review covers the workspace's HEAD with
    /// every finding resolved, saying what is missing; without a review of
    /// HEAD and with none queued or running, queue one. Passes when `gate`
    /// is not required.
    pub(crate) async fn require_review(&self, workspace: &Workspace, gate: Gate) -> Result<()> {
        let settings = self.workspace_settings(workspace).await?;
        let required = match gate {
            Gate::Pr => settings.review.require_for_pr,
            Gate::Ready => settings.review.require_for_ready,
        };
        if !required {
            return Ok(());
        }
        let head = crate::daemon::links::pr::current_head(workspace).await?;
        let (id, covering) = (workspace.id.clone(), head.clone());
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
                &head[..head.len().min(12)]
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
                    .queue_review(&workspace.id, ReviewTarget::Workspace, None)
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
    fn command(self) -> &'static str {
        match self {
            Self::Pr => "opening a PR",
            Self::Ready => "marking work ready",
        }
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
