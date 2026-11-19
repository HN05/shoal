//! Review gates: with `[review] require_for_pr`, `require_for_ready` or
//! `require_for_merge` holding the caller, it opens a new PR, marks work
//! ready or merges only once a finished review covers the commit it acts on
//! and every finding is resolved.
use shoal_ids::space_ids::SpaceId;
use shoal_refs::CommitId;
use std::fmt::Write as _;

use anyhow::{Result, bail};

use crate::{
    Reviews,
    config::{Effective, repo::Requester},
    daemon::Daemon,
    model::Workspace,
    reviews::{Review, ReviewId, ReviewState, ReviewTarget},
    store,
};
use shoal_daemon_base::callers::Caller;
use shoal_protocol::space_names::SpaceSelector;

/// What a gate guards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// `shoal pr open` of a new PR.
    Pr,
    /// `shoal ready`.
    Ready,
    /// `shoal merge`.
    Merge,
}

/// A gate's refusal for want of any review of the commit, with none queued
/// or running, before a review is queued for it: queueing takes the
/// registry gate, so a caller that holds a repository's Git gate releases
/// it first, then queues as [`Reviews::queue_unreviewed`] does.
#[derive(Debug)]
pub struct Unreviewed {
    refusal: String,
    workspace: SpaceId,
}

impl std::fmt::Display for Unreviewed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.refusal)
    }
}

impl std::error::Error for Unreviewed {}

impl Reviews {
    /// Refuse `caller`, where the workspace's setting for `gate` holds it,
    /// as [`Self::check_reviewed`] does, queueing the review it misses.
    pub async fn require(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        gate: Gate,
        head: &CommitId,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let checked = self.check(daemon, workspace, gate, head, caller).await;
        self.queue_unreviewed(daemon, checked).await
    }

    /// Refuse `caller`, where the setting for `gate` holds it, as
    /// [`Self::check_reviewed`] does.
    pub async fn check(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        gate: Gate,
        head: &CommitId,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let settings = gate_settings(daemon, workspace, gate).await?;
        if !gate.holds(&settings, caller) {
            return Ok(());
        }
        self.check_reviewed(workspace, gate, head).await
    }

    /// Refuse unless a finished review of the workspace covers `head`, the
    /// commit a command acts on, with every finding resolved, saying what is
    /// missing: as [`Unreviewed`] without a review of it and with none
    /// queued or running.
    pub async fn check_reviewed(
        &self,
        workspace: &Workspace,
        gate: Gate,
        head: &CommitId,
    ) -> Result<()> {
        let (id, covering) = (workspace.id.clone(), head.to_owned());
        let records = self.records.clone();
        let (reviewed, active) = self
            .store
            .read(move |tx| {
                Ok((
                    store::finished_at(tx, &*records, &id, &covering)?,
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
        match active {
            Some((id, state)) => bail!("{missing}{}", pending(id, state)),
            None if reviewed.is_none() => Err(Unreviewed {
                refusal: missing,
                workspace: workspace.id.clone(),
            }
            .into()),
            None => bail!(missing),
        }
    }

    /// `checked`, with a review of the workspace queued for an
    /// [`Unreviewed`] refusal, which then says how that went.
    pub async fn queue_unreviewed<T>(&self, daemon: &dyn Daemon, checked: Result<T>) -> Result<T> {
        let error = match checked {
            Ok(checked) => return Ok(checked),
            Err(error) => error,
        };
        let Some(unreviewed) = error.downcast_ref::<Unreviewed>() else {
            return Err(error);
        };
        let queued = self.queue_for_gate(daemon, &unreviewed.workspace).await;
        bail!("{unreviewed}{queued}")
    }

    /// Queue a review of the workspace `id` for a gate that found none,
    /// saying how it went.
    async fn queue_for_gate(&self, daemon: &dyn Daemon, id: &SpaceId) -> String {
        let selector = SpaceSelector::id(id);
        let queued = self.queue(
            daemon,
            &selector,
            ReviewTarget::Workspace,
            None,
            Default::default(),
        );
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
    pub fn holds(self, settings: &Effective, caller: Option<&Caller>) -> bool {
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

/// The settings that decide `gate` for `workspace`: its own, except for a
/// merge those of the repository it merges into, which no branch's own
/// config file turns off.
async fn gate_settings(
    daemon: &dyn Daemon,
    workspace: &Workspace,
    gate: Gate,
) -> Result<Effective> {
    match gate {
        Gate::Merge => daemon.repository_settings(workspace.repository()).await,
        Gate::Pr | Gate::Ready => daemon.settings(workspace).await,
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
