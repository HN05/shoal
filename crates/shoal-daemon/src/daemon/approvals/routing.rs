//! Routing a request to the space that decides it, passing it up, and
//! recording that space's or the user's decision.
use crate::client::space_ids::SpaceId;
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, Transaction, params};

use super::{
    ApprovalRequest, ApprovalRequestExt, DecisionStatus, Target, proposals,
    store::{CANDIDATES, by_key, decode_record, record_row, write_record},
};
use crate::client::space_names::SpaceSelector;
use crate::{
    config::permissions::PermissionKey,
    daemon::{
        events::{self, EventKind},
        item_numbers::ItemNumber,
        log,
        scope::Caller,
        store::require_ready,
        workspace::Manager,
    },
};

/// The permissions that govern `request`, each of which a coordinator needs
/// as `direct` to approve it instead of a grant; none for a request a grant
/// governs. They are those whose value made the space ask instead of act: an
/// issue in another repository is governed by `issue_other_repositories`
/// alone, and what pushes by `push` too.
fn governing(request: &ApprovalRequest) -> &'static [PermissionKey] {
    use PermissionKey::{IssueOpen, IssueOtherRepositories, Merge, PrOpen, Push};
    match request.target {
        Target::IssueOpen
            if (request.proposed_issue()).is_some_and(|issue| issue.repository.is_some()) =>
        {
            &[IssueOtherRepositories]
        }
        Target::IssueOpen => &[IssueOpen],
        Target::PrOpen if request.pending_push().is_some() => &[PrOpen, Push],
        Target::PrOpen => &[PrOpen],
        Target::Push => &[Push],
        Target::Merge | Target::Unmerge => &[Merge, Push],
        _ => &[],
    }
}

/// The permissions a coordinator needs as `direct` to decide `request` so:
/// to approve it, those that govern it; to deny an issue proposal for
/// another repository as a duplicate of an issue there, which comments on
/// that issue, `issue_other_repositories`.
fn needed(
    request: &ApprovalRequest,
    approve: bool,
    details: &proposals::ProposalDecision,
) -> &'static [PermissionKey] {
    if approve {
        return governing(request);
    }
    let elsewhere = (request.proposed_issue()).is_some_and(|issue| issue.repository.is_some());
    match &details.duplicate_of {
        Some(proposals::Duplicate::Issue(_)) if elsewhere => {
            &[PermissionKey::IssueOtherRepositories]
        }
        _ => &[],
    }
}

/// Whether the workspace holds an approved grant for the same target and
/// settings as `request`, which lets its agent decide the request for the
/// workspaces below it.
fn holds_grant(db: &Connection, workspace: &SpaceId, request: &ApprovalRequest) -> Result<bool> {
    let candidates = db
        .prepare(CANDIDATES)?
        .query_map(params![workspace, request.target], record_row)?
        .map(|row| decode_record(row?))
        .collect::<Result<Vec<_>>>()?;
    Ok(candidates.iter().any(|grant| {
        grant.status == DecisionStatus::Approved
            && grant.specification == request.specification
            && grant.lifetime == request.lifetime
    }))
}

/// Whether the coordinating agent of `workspace` may approve `request` as
/// far as grants go: one that needs no grant, one its permissions decide
/// instead, or one it holds a grant for.
fn may_decide(db: &Connection, workspace: &SpaceId, request: &ApprovalRequest) -> Result<bool> {
    Ok(!request.target.needs_grant()
        || !governing(request).is_empty()
        || holds_grant(db, workspace, request)?)
}

/// Give the pending request stored under `key` to the first of `candidates`,
/// or to the user. Without `from` this is its first routing, which a routed
/// request keeps; with `from`, the space that decides it passes it on.
pub(super) fn route(
    tx: &Transaction<'_>,
    key: &str,
    candidates: &[SpaceId],
    from: Option<&SpaceId>,
) -> Result<ApprovalRequest> {
    let mut request = by_key(tx, key)?.context("the approval request no longer exists")?;
    ensure!(
        request.status == DecisionStatus::Pending,
        "approval request {} is already decided",
        request.number
    );
    match from {
        None if request.routed => return Ok(request),
        None => {}
        Some(from) => ensure!(
            request.decider.as_ref() == Some(from),
            "approval request {} is no longer this space's to decide",
            request.number
        ),
    }
    // Without a decider above, the request stays the user's, as recorded.
    if candidates.is_empty() && !request.routed {
        return Ok(request);
    }
    request.decider = candidates.first().cloned();
    request.routed = true;
    write_record(tx, &request)?;
    Ok(request)
}

/// A decided request; `new` is false when the decision repeats the recorded one.
pub struct Decision {
    pub request: ApprovalRequest,
    pub new: bool,
}

/// Record a decision on the pending request stored under `key`; repeating the
/// same decision is a no-op.
pub fn decide(tx: &Transaction<'_>, key: &str, approve: bool) -> Result<Decision> {
    let mut request = by_key(tx, key)?.context("the approval request no longer exists")?;
    require_ready(tx, &request.space_id)?;
    ensure!(
        !matches!(
            request.status,
            DecisionStatus::Outdated | DecisionStatus::Withdrawn
        ),
        "approval request {} is {}: it no longer applies",
        request.number,
        request.status
    );
    let status = if approve {
        DecisionStatus::Approved
    } else {
        DecisionStatus::Denied
    };
    ensure!(
        request.status == DecisionStatus::Pending || request.status == status,
        "approval request already decided"
    );
    let new = request.status == DecisionStatus::Pending;
    if new {
        request.status = status;
        request.decided_at = Some(crate::time::UnixTime::now());
        write_record(tx, &request)?;
        events::record_approval(tx, EventKind::ApprovalDecided, &request)?;
    }
    Ok(Decision { request, new })
}

/// [`decide`], placing the hold an approved hold request asks for in the
/// same transaction.
fn decide_and_place(tx: &Transaction<'_>, key: &str, approve: bool) -> Result<Decision> {
    let decision = decide(tx, key, approve)?;
    if decision.new && decision.request.status == DecisionStatus::Approved {
        crate::daemon::workspace::place_approved_hold(tx, &decision.request)?;
    }
    Ok(decision)
}

impl Manager {
    /// Decide request `number` as `caller`: a scoped process decides only
    /// what was routed to its space, as [`Self::decide_routed_approval`]
    /// allows.
    pub(in crate::daemon) async fn decide_approval_as(
        &self,
        caller: Option<&Caller>,
        number: ItemNumber,
        approve: bool,
        shown: Option<ApprovalRequest>,
        details: proposals::ProposalDecision,
        env: Option<std::collections::BTreeMap<String, String>>,
    ) -> Result<ApprovalRequest> {
        match caller {
            Some(caller) => {
                self.decide_routed_approval(&caller.space_id, number, approve, shown, details)
                    .await
            }
            None => {
                self.decide_approval(number, approve, shown, details, env)
                    .await
            }
        }
    }

    /// Record the decision and tell the requesting workspace's agents about a
    /// new one. With `shown`, the request the user was shown, a later request
    /// reusing its number is refused. Only proposals take `details`; an
    /// approved push runs in `env`.
    pub async fn decide_approval(
        &self,
        number: ItemNumber,
        approve: bool,
        shown: Option<ApprovalRequest>,
        details: proposals::ProposalDecision,
        env: Option<std::collections::BTreeMap<String, String>>,
    ) -> Result<ApprovalRequest> {
        let request = self.show_approval(number).await?;
        crate::refusal::ensure_unchanged(
            shown.is_none_or(|shown| request.is_same(&shown)),
            format_args!("approval request {number}"),
        )?;
        if request.target.is_proposal() {
            return self
                .decide_proposal(request, approve, details, env, None)
                .await;
        }
        ensure!(
            details == proposals::ProposalDecision::default(),
            "only issue and PR proposals take a title, body, reason or duplicate"
        );
        let decision = self.record_decision(request, approve).await?;
        if decision.new {
            self.tell_requester(&decision.request).await;
        }
        Ok(decision.request)
    }

    /// Record the decision of the coordinating agent of `coordinator` on a
    /// request routed to it, rechecking with the decision that the request is
    /// still its to decide. It approves what a permission governs where its
    /// own permission is `direct`, with a proposal's edits, and anything else
    /// only while it holds a grant for it; otherwise it denies the request,
    /// with a reason or as a duplicate, or passes it up.
    pub async fn decide_routed_approval(
        &self,
        coordinator: &SpaceId,
        number: ItemNumber,
        approve: bool,
        shown: Option<ApprovalRequest>,
        details: proposals::ProposalDecision,
    ) -> Result<ApprovalRequest> {
        let request = self.show_approval(number).await?;
        crate::refusal::ensure_unchanged(
            shown.is_none_or(|shown| request.is_same(&shown)),
            format_args!("approval request {number}"),
        )?;
        self.ensure_permitted(coordinator, &request, needed(&request, approve, &details))
            .await?;
        if let Some(proposals::Duplicate::Approval(original)) = &details.duplicate_of {
            self.ensure_reaches_proposal(coordinator, *original).await?;
        }
        if request.target.is_proposal() {
            return self
                .decide_proposal(request, approve, details, None, Some(coordinator))
                .await;
        }
        ensure!(
            details == proposals::ProposalDecision::default(),
            "only issue and PR proposals take a title, body, reason or duplicate"
        );
        let decision = self
            .record_routed_decision(coordinator, &request, approve)
            .await?;
        if decision.new {
            self.tell_requester(&decision.request).await;
        }
        Ok(decision.request)
    }

    /// Refuse `coordinator`'s decision on `request` unless its effective
    /// permission is `direct` for each of `keys`, resolved in the repository
    /// of the request's action.
    async fn ensure_permitted(
        &self,
        coordinator: &SpaceId,
        request: &ApprovalRequest,
        keys: &[PermissionKey],
    ) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let repository = match &request.repository_id {
            Some(repository) => Some(repository.clone()),
            None => (self.space_by_id(&request.space_id).await?)
                .repository_id()
                .cloned(),
        };
        let permissions = self
            .space_permissions(coordinator, repository.as_ref())
            .await?;
        if let Some(withheld) = permissions.withheld(keys) {
            let id = request.number;
            bail!(
                "approval {id}: {withheld}; deny it or pass it up with shoal approvals --escalate {id}"
            );
        }
        Ok(())
    }

    /// Refuse `coordinator`'s marking a proposal a duplicate of proposal
    /// `number` unless its scope reaches the space that made it, since the
    /// finding changes that proposal.
    async fn ensure_reaches_proposal(
        &self,
        coordinator: &SpaceId,
        number: ItemNumber,
    ) -> Result<()> {
        let original = self.show_approval(number).await?;
        let caller = Caller {
            space_id: coordinator.clone(),
            execution: None,
        };
        ensure!(
            self.reaches(&caller, &original.space_id).await?,
            "approval request {number} is not this space's or below it; deny the duplicate with --reason or pass it up"
        );
        Ok(())
    }

    /// Decide `request` for `coordinator` in one transaction with the checks
    /// that it decides it and, to approve one a grant governs, holds a grant
    /// for it.
    async fn record_routed_decision(
        &self,
        coordinator: &SpaceId,
        request: &ApprovalRequest,
        approve: bool,
    ) -> Result<Decision> {
        self.ensure_merge_current(request, approve).await?;
        let request = request.clone();
        let (coordinator, key, id) = (coordinator.to_owned(), request.key.clone(), request.number);
        let _hold = self.hold_decision_guard(&request, approve).await?;
        let _guard = self.gates.lock_simulators().await;
        self.store
            .write(move |tx| {
                let stored = by_key(tx, &key)?.context("the approval request no longer exists")?;
                ensure!(
                    stored.decider.as_ref() == Some(&coordinator),
                    "approval request {id} is not routed to this space"
                );
                crate::refusal::ensure_unchanged(
                    stored.is_same(&request),
                    format_args!("approval request {id}"),
                )?;
                ensure!(
                    !approve || may_decide(tx, &coordinator, &stored)?,
                    "approval {id}: this space holds no grant for {}; deny it or pass it up with shoal approvals --escalate {id}",
                    stored.subject()
                );
                let decision = decide_and_place(tx, &key, approve)?;
                Ok(decision)
            })
            .await
    }

    /// Decide the stored `request` the user chose; later requests reusing its
    /// number are not affected.
    async fn record_decision(&self, request: ApprovalRequest, approve: bool) -> Result<Decision> {
        self.ensure_merge_current(&request, approve).await?;
        let _hold = self.hold_decision_guard(&request, approve).await?;
        let key = request.key;
        // Simulator allocation checks approval and mutates devices in separate
        // transactions under this gate; a decision must not land between them.
        let _guard = self.gates.lock_simulators().await;
        self.store
            .write(move |tx| decide_and_place(tx, &key, approve))
            .await
    }

    /// Tell whoever decides a pending request about it: the space directly
    /// above its workspace, a parent workspace with an agent or the chat that
    /// opened it, or else the user. A request is routed once; repeating it
    /// tells its decider again.
    pub async fn notify_approval(&self, request: &ApprovalRequest) {
        let _gate = self.gates.lock_announcements().await;
        // It may have been withdrawn or decided since it was requested.
        let request = match self.approval_by_key(&request.key).await {
            Ok(Some(stored)) if stored.status == DecisionStatus::Pending => stored,
            Ok(_) => return,
            Err(error) => {
                log!("approval {} not rechecked: {error:#}", request.number);
                request.clone()
            }
        };
        let request = &request;
        let routed = match request.routed {
            true => Ok(request.clone()),
            false => self.route_approval(request, None).await,
        };
        match routed {
            Ok(routed) => self.announce_approval(&routed).await,
            Err(error) => {
                log!("approval {} not routed: {error:#}", request.number);
                self.ask_user(request).await;
            }
        }
    }

    /// Pass the request the space `space` decides on to the space above it,
    /// or to the user. With `shown`, a later request reusing its number is
    /// refused.
    pub async fn escalate_approval(
        &self,
        space: &SpaceSelector,
        number: ItemNumber,
        shown: Option<ApprovalRequest>,
    ) -> Result<ApprovalRequest> {
        let space = self.space(space).await?;
        let request = self.show_approval(number).await?;
        crate::refusal::ensure_unchanged(
            shown.is_none_or(|shown| request.is_same(&shown)),
            format_args!("approval request {number}"),
        )?;
        ensure!(
            request.decider.as_ref() == Some(space.id()),
            "approval request {number} is not {}'s to decide",
            space.name()
        );
        let _gate = self.gates.lock_announcements().await;
        let routed = self.route_approval(&request, Some(space.id())).await?;
        self.announce_approval(&routed).await;
        // A decider that could not be told gave the request to the user. Its
        // key, unlike its number, never names another request.
        Ok(self.approval_by_key(&routed.key).await?.unwrap_or(routed))
    }

    /// Route `request` to the space above its workspace, or to the one above
    /// `above` when that space passes it on.
    async fn route_approval(
        &self,
        request: &ApprovalRequest,
        above: Option<&SpaceId>,
    ) -> Result<ApprovalRequest> {
        let agents = self.tracked_agents().await;
        let (key, workspace) = (request.key.clone(), request.space_id.clone());
        let above = above.cloned();
        self.store
            .write(move |tx| {
                // Passed up, it goes on above its decider, whatever became
                // of the requester's agent since it was routed.
                let candidates = match &above {
                    Some(above) => crate::daemon::workspace::spaces_above(tx, above, &agents)?,
                    None => crate::daemon::workspace::deciders(tx, &workspace, &agents)?,
                };
                let routed = route(tx, &key, &candidates, above.as_ref())?;
                Ok(routed)
            })
            .await
    }
}
