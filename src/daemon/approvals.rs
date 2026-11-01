//! Human decisions authorize cooperative allocations; they never reserve capacity.
use crate::daemon::refusal::{Reason, Refusal};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Params, Transaction, params};
use std::fmt;

use crate::{
    daemon::{
        log,
        notifications::{self, About, NotificationKind},
        scope::Caller,
        store,
        workspace::Manager,
    },
    model::Workspace,
};

pub mod proposals;

pub(crate) use crate::client::approvals::{
    ApprovalRequest, DecisionStatus, HoldSpecification, LAND, LandSpecification, Lifetime,
    PortSpecification, ResourceSpecification, SecretSpecification, SimulatorSpecification,
    Specification, Target,
};

impl Target {
    /// Whether the request proposes a forge write the user may edit before
    /// approving, rather than granting access.
    pub fn is_proposal(&self) -> bool {
        matches!(self, Self::IssueOpen | Self::PrOpen)
    }

    /// Whether a coordinator decides the request only while it holds a grant
    /// for the same thing. A hold grants nothing a coordinator could hold, so
    /// the nearest one decides it.
    fn needs_grant(&self) -> bool {
        !matches!(self, Self::Hold(_))
    }
}

const COLUMNS: &str = "SELECT id,record,active,number FROM access_requests";
const BY_KEY: &str = "SELECT id,record,active,number FROM access_requests WHERE id=?1";
const BY_NUMBER: &str = "SELECT id,record,active,number FROM access_requests WHERE number=?1";
const CURRENT: &str = "SELECT id,record,active,number FROM access_requests
    WHERE workspace_id=?1 AND target_key=?2 AND name=?3 AND active=1";

const CANDIDATES: &str = "SELECT id,record,active,number FROM access_requests
    WHERE workspace_id=?1 AND target_key=?2 ORDER BY rowid";

/// A stored request's key, record, active flag and number.
type RecordRow = (String, String, bool, i64);

fn decode_record((key, record, active, number): RecordRow) -> Result<ApprovalRequest> {
    let mut request: ApprovalRequest = serde_json::from_str(&record)
        .with_context(|| format!("approval request {number} has an invalid record"))?;
    request.key = key;
    request.id = number;
    request.active = active;
    Ok(request)
}

fn record_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RecordRow> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn lookup(db: &Connection, sql: &str, params: impl Params) -> Result<Option<ApprovalRequest>> {
    db.query_row(sql, params, record_row)
        .optional()?
        .map(decode_record)
        .transpose()
}

fn by_key(db: &Connection, key: &str) -> Result<Option<ApprovalRequest>> {
    lookup(db, BY_KEY, [key])
}

fn by_number(db: &Connection, number: i64) -> Result<Option<ApprovalRequest>> {
    lookup(db, BY_NUMBER, [number])
}

/// Every request, or those `owner` made, including the issues it proposed
/// for its repository.
pub fn list(db: &Connection, owner: Option<&str>) -> Result<Vec<ApprovalRequest>> {
    db.prepare(&format!(
        "{COLUMNS} WHERE ?1 IS NULL OR workspace_id=?1
            OR (workspace_id IS NULL AND json_extract(record, '$.workspace_id')=?1)
        ORDER BY rowid"
    ))?
    .query_map([owner], record_row)?
    .map(|row| decode_record(row?))
    .collect()
}

pub fn current(
    db: &Connection,
    owner: &str,
    target: &Target,
    name: &str,
) -> Result<Option<ApprovalRequest>> {
    lookup(db, CURRENT, params![owner, target, name])
}

/// A new request names no reason; callers may say how to give one.
#[derive(Debug)]
pub struct ReasonRequired;

impl fmt::Display for ReasonRequired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("approval requires --reason explaining the request")
    }
}

impl std::error::Error for ReasonRequired {}

/// The caller holds the allocation transaction (or simulator gate). The exact
/// specification includes effective policy, so a changed policy cannot reuse a grant.
pub fn check(
    tx: &Transaction<'_>,
    mut request: ApprovalRequest,
) -> Result<Option<ApprovalRequest>> {
    store::require_ready(tx, &request.workspace_id)?;
    if let Some(existing) = current(tx, &request.workspace_id, &request.target, &request.name)? {
        ensure!(
            existing.specification == request.specification
                && existing.lifetime == request.lifetime,
            "approval request settings changed; release the resource name before retrying"
        );
        ensure!(
            request.reason.is_empty() || request.reason == existing.reason,
            "approval request reason changed; release the resource name before retrying"
        );
        return Ok((existing.status != DecisionStatus::Approved).then_some(existing));
    }
    if reusable_grant(tx, &request)? {
        return Ok(None);
    }
    ensure!(!request.reason.trim().is_empty(), ReasonRequired);
    crate::validate::reason("approval", Some(&request.reason))?;
    request.workspace = tx.query_row(
        "SELECT name FROM workspaces WHERE id=?1",
        [&request.workspace_id],
        |r| r.get(0),
    )?;
    request.key = uuid::Uuid::new_v4().to_string();
    request.id = store::free_number(tx, "access_requests")?;
    tx.execute("INSERT INTO access_requests(id,workspace_id,target_key,name,record,number) VALUES (?1,?2,?3,?4,?5,?6)",
        params![request.key,request.workspace_id,request.target,request.name,serde_json::to_string(&request)?,request.id])?;
    Ok(Some(request))
}

fn reusable_grant(db: &Connection, request: &ApprovalRequest) -> Result<bool> {
    // Decode every candidate before matching: even a later malformed record must
    // fail the check. Released workspace grants remain candidates.
    let candidates = db
        .prepare(CANDIDATES)?
        .query_map(params![request.workspace_id, request.target], record_row)?
        .map(|row| decode_record(row?))
        .collect::<Result<Vec<_>>>()?;
    Ok(candidates.iter().any(|r| {
        r.status == DecisionStatus::Approved
            && r.lifetime == Lifetime::Workspace
            && r.target == request.target
            && r.specification == request.specification
            && r.lifetime == request.lifetime
    }))
}

impl ApprovalRequest {
    pub fn new(
        owner: &str,
        target: Target,
        name: &str,
        specification: Specification,
        lifetime: Lifetime,
        reason: Option<&str>,
    ) -> Self {
        Self {
            id: 0,
            key: String::new(),
            workspace_id: owner.into(),
            workspace: String::new(),
            repository_id: None,
            target,
            name: name.into(),
            specification,
            lifetime,
            reason: reason.unwrap_or_default().into(),
            status: DecisionStatus::Pending,
            created_at: crate::time::unix_seconds(),
            decided_at: None,
            active: true,
            routed: false,
            decider: None,
            url: None,
            warnings: Vec::new(),
        }
    }

    /// Whether `other` is this request as recorded, though its decision may
    /// differ: the same key, workspace, target, settings, reason and time.
    fn is_same(&self, other: &ApprovalRequest) -> bool {
        self.id == other.id
            && self.key == other.key
            && self.workspace_id == other.workspace_id
            && self.target == other.target
            && self.name == other.name
            && self.specification == other.specification
            && self.lifetime == other.lifetime
            && self.reason == other.reason
            && self.created_at == other.created_at
    }

    /// What the user is told about a request no coordinator decides,
    /// including how to approve it.
    fn notification(&self) -> String {
        format!(
            "approval {}: {}{}; approve with shoal approvals --approve {}",
            self.id,
            self.subject(),
            reason_suffix(self),
            self.id
        )
    }

    /// What its decider is told about a withdrawn request whose
    /// notification was already shown.
    fn withdrawal_notification(&self) -> String {
        format!(
            "approval {}: {} was withdrawn; no decision needed",
            self.id,
            self.subject()
        )
    }

    /// The agent message naming a decision and what the requesting agent does next.
    fn decision_message(&self) -> String {
        let next = match self.status {
            DecisionStatus::Approved => "retry the command that requested it",
            _ => "do not retry it",
        };
        format!(
            "Approval {} for {} was {}: {next}.",
            self.id,
            self.subject(),
            self.status
        )
    }
}

/// Whether the workspace holds an approved grant for the same target and
/// settings as `request`, which lets its agent decide the request for the
/// workspaces below it.
fn holds_grant(db: &Connection, workspace: &str, request: &ApprovalRequest) -> Result<bool> {
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

/// Whether the coordinating agent of `workspace` may decide `request`: one
/// that needs no grant, or one it holds a grant for.
fn may_decide(db: &Connection, workspace: &str, request: &ApprovalRequest) -> Result<bool> {
    Ok(!request.target.needs_grant() || holds_grant(db, workspace, request)?)
}

/// A pending request and the coordinators routing passed over, which hold no
/// grant for it.
pub struct Routed {
    pub request: ApprovalRequest,
    pub passed: Vec<String>,
}

/// Give the pending request stored under `key` to the first of `candidates`
/// that holds a grant for it, or to the user. Without `from` this is its
/// first routing, which a routed request keeps; with `from`, the coordinator
/// that decides it passes it on.
fn route(
    tx: &Transaction<'_>,
    key: &str,
    candidates: &[String],
    from: Option<&str>,
) -> Result<Routed> {
    let mut request = by_key(tx, key)?.context("the approval request no longer exists")?;
    ensure!(
        request.status == DecisionStatus::Pending,
        "approval request {} is already decided",
        request.id
    );
    match from {
        None if request.routed => {
            return Ok(Routed {
                request,
                passed: Vec::new(),
            });
        }
        None => {}
        Some(from) => ensure!(
            request.decider.as_deref() == Some(from),
            "approval request {} is no longer this workspace's to decide",
            request.id
        ),
    }
    // Without coordinators the request stays the user's, as recorded.
    if candidates.is_empty() && !request.routed {
        return Ok(Routed {
            request,
            passed: Vec::new(),
        });
    }
    let mut passed = Vec::new();
    request.decider = None;
    for candidate in candidates {
        if may_decide(tx, candidate, &request)? {
            request.decider = Some(candidate.clone());
            break;
        }
        passed.push(candidate.clone());
    }
    request.routed = true;
    tx.execute(
        "UPDATE access_requests SET record=?2 WHERE id=?1",
        params![key, serde_json::to_string(&request)?],
    )?;
    Ok(Routed { request, passed })
}

/// Keep a request bound to a commit that no longer applies, because it was
/// replaced, cancelled or found stale, as outdated until its workspace is
/// removed: its number is never reused for a newer request, and deciding it
/// is refused.
pub fn retire(tx: &Transaction<'_>, request: &ApprovalRequest) -> Result<()> {
    close(tx, request.clone(), DecisionStatus::Outdated)?;
    Ok(())
}

/// Keep `request` undecidable with `status` until its workspace is removed.
fn close(
    tx: &Transaction<'_>,
    mut request: ApprovalRequest,
    status: DecisionStatus,
) -> Result<ApprovalRequest> {
    request.status = status;
    request.active = false;
    tx.execute(
        "UPDATE access_requests SET record=?2,active=0 WHERE id=?1",
        params![request.key, serde_json::to_string(&request)?],
    )?;
    Ok(request)
}

/// The workspace's request `id`, as the agent names it to withdraw.
fn own_request(db: &Connection, workspace: &Workspace, id: i64) -> Result<ApprovalRequest> {
    by_number(db, id)?
        .filter(|request| request.workspace_id == workspace.id)
        .with_context(|| format!("workspace {} has no approval request {id}", workspace.name))
}

/// Close the pending request `selected`, which its agent no longer needs. A
/// proposed issue goes as a decided one does, since it would outlive the
/// workspace; any other request stays as withdrawn, like an outdated one.
/// The stored request is found by its key, so a later request that took the
/// number since it was selected is never withdrawn in its place.
fn withdraw(tx: &Transaction<'_>, selected: &ApprovalRequest) -> Result<ApprovalRequest> {
    let id = selected.id;
    let request = by_key(tx, &selected.key)?
        .with_context(|| format!("approval request {id} was decided meanwhile"))?;
    ensure!(
        request.status == DecisionStatus::Pending,
        "approval request {id} is {}; only pending requests can be withdrawn",
        request.status
    );
    if let Some(url) = &request.url {
        bail!("approval request {id} already opened {url}");
    }
    if request.target != Target::IssueOpen {
        return close(tx, request, DecisionStatus::Withdrawn);
    }
    proposals::remove(tx, &request.key)?;
    Ok(ApprovalRequest {
        status: DecisionStatus::Withdrawn,
        active: false,
        ..request
    })
}

/// Cancel requests or end lease grants; retain approved workspace grants.
pub fn release(tx: &Transaction<'_>, owner: &str, target: &Target, name: &str) -> Result<bool> {
    let Some(request) = current(tx, owner, target, name)? else {
        return Ok(false);
    };
    if request.status == DecisionStatus::Approved && request.lifetime == Lifetime::Workspace {
        tx.execute(
            "UPDATE access_requests SET active=0 WHERE id=?1",
            [&request.key],
        )?;
    } else {
        tx.execute("DELETE FROM access_requests WHERE id=?1", [&request.key])?;
    }
    Ok(true)
}

/// Replace a land request for an earlier commit with `request`. Returns the
/// request still awaiting or refused a decision; an approval for the same
/// commit lets the landing proceed.
pub fn request_land(
    tx: &Transaction<'_>,
    request: ApprovalRequest,
) -> Result<Option<ApprovalRequest>> {
    let owner = request.workspace_id.clone();
    if let Some(existing) = current(tx, &owner, &Target::Land, LAND)?
        .filter(|existing| existing.specification != request.specification)
    {
        retire(tx, &existing)?;
    }
    check(tx, request)
}

/// An operation waiting for, or refused by, the user's decision on a request.
#[derive(Debug)]
pub struct Undecided(pub Box<ApprovalRequest>);

impl fmt::Display for Undecided {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "approval {} for {} is {}",
            self.0.id,
            self.0.subject(),
            self.0.status
        )
    }
}

impl std::error::Error for Undecided {}

/// A decided request; `new` is false when the decision repeats the recorded one.
pub struct Decision {
    pub request: ApprovalRequest,
    pub new: bool,
}

/// Record a decision on the pending request stored under `key`; repeating the
/// same decision is a no-op.
pub fn decide(tx: &Transaction<'_>, key: &str, approve: bool) -> Result<Decision> {
    let mut request = by_key(tx, key)?.context("the approval request no longer exists")?;
    store::require_ready(tx, &request.workspace_id)?;
    ensure!(
        !matches!(
            request.status,
            DecisionStatus::Outdated | DecisionStatus::Withdrawn
        ),
        "approval request {} is {}: it no longer applies",
        request.id,
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
        request.decided_at = Some(crate::time::unix_seconds());
        tx.execute(
            "UPDATE access_requests SET record=?2 WHERE id=?1",
            params![key, serde_json::to_string(&request)?],
        )?;
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
    /// [`check`] in its own transaction, for allocations that claim nothing else.
    pub async fn request_approval(
        &self,
        request: ApprovalRequest,
    ) -> Result<Option<ApprovalRequest>> {
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let pending = check(&tx, request)?;
                tx.commit()?;
                Ok(pending)
            })
            .await
    }

    /// [`request_land`] in its own transaction.
    pub async fn request_land_approval(
        &self,
        request: ApprovalRequest,
    ) -> Result<Option<ApprovalRequest>> {
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let undecided = request_land(&tx, request)?;
                tx.commit()?;
                Ok(undecided)
            })
            .await
    }

    /// Whether a PR proposal of the workspace waits for the user's decision.
    pub async fn has_pending_pull_proposal(&self, workspace_id: &str) -> Result<bool> {
        let id = workspace_id.to_owned();
        self.store
            .run(move |db| proposals::pull_pending(db, &id))
            .await
    }

    /// [`release`] in its own transaction, once the workspace is ready.
    pub async fn release_approval(
        &self,
        owner: String,
        target: Target,
        name: String,
    ) -> Result<bool> {
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                store::require_ready(&tx, &owner)?;
                let released = release(&tx, &owner, &target, &name)?;
                tx.commit()?;
                Ok(released)
            })
            .await
    }

    /// Record a proposal with `record` in its own transaction.
    pub(crate) async fn store_proposal(
        &self,
        request: ApprovalRequest,
        record: fn(&Transaction<'_>, ApprovalRequest) -> Result<proposals::Recorded>,
    ) -> Result<proposals::Recorded> {
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let recorded = record(&tx, request)?;
                tx.commit()?;
                Ok(recorded)
            })
            .await
    }

    /// [`proposals::save`] in its own transaction.
    pub(crate) async fn save_proposal(&self, request: ApprovalRequest) -> Result<()> {
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                proposals::save(&tx, &request)?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// [`proposals::remove`] in its own transaction, once it is decided.
    pub(crate) async fn forget_proposal(&self, key: String) -> Result<()> {
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                proposals::remove(&tx, &key)?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// Withdraw the workspace's pending request `id`, which its agent no
    /// longer needs, and take back its notification while unread; once
    /// shown, its decider is told instead.
    pub async fn withdraw_approval(&self, selector: &str, id: i64) -> Result<ApprovalRequest> {
        let workspace = self.workspace(selector).await?;
        let workspace_id = workspace.id.clone();
        let selected = self
            .store
            .run(move |db| own_request(db, &workspace, id))
            .await?;
        // Deciding a proposal opens its item under the workspace's gate
        // before forgetting the request, which withdrawal must not split.
        let _decision = match selected.target.is_proposal() {
            true => Some(self.gates.lock_prs([workspace_id]).await),
            false => None,
        };
        let _announcement = self.gates.lock_announcements().await;
        let (request, retracted) = self.record_withdrawal(selected).await?;
        if request.target.is_proposal() {
            self.prune_proposal_files().await;
        }
        if !retracted {
            self.announce_withdrawal(&request).await;
        }
        Ok(request)
    }

    /// Atomically withdraw the selected request and retract its unread notification.
    async fn record_withdrawal(
        &self,
        selected: ApprovalRequest,
    ) -> Result<(ApprovalRequest, bool)> {
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let request = withdraw(&tx, &selected)?;
                let retracted = notifications::retract_about(
                    &tx,
                    NotificationKind::ApprovalRequested,
                    &request.key,
                )?;
                tx.commit()?;
                Ok((request, retracted))
            })
            .await
    }

    /// Tell the decider of a withdrawn request that it needs no decision:
    /// the coordinating agent it was routed to, or else the user.
    async fn announce_withdrawal(&self, request: &ApprovalRequest) {
        let message = request.withdrawal_notification();
        if let Some(decider) = &request.decider
            && self
                .tell_agent(decider, format!("{}: {message}", request.workspace))
                .await
        {
            return;
        }
        self.notify(
            Some(&request.workspace),
            NotificationKind::ApprovalWithdrawn,
            message,
        )
        .await;
    }

    /// [`retire`] in its own transaction, such as for a stale request.
    pub async fn retire_approval(&self, request: ApprovalRequest) -> Result<()> {
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                retire(&tx, &request)?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    pub async fn approval_requests(&self, selector: Option<&str>) -> Result<Vec<ApprovalRequest>> {
        let owner = self.workspace_filter(selector).await?;
        self.store.run(move |db| list(db, owner.as_deref())).await
    }

    /// Decide request `id` as `caller`: a workspace process decides only what
    /// was routed to it, and only the user decides with `details`.
    pub(super) async fn decide_approval_as(
        &self,
        caller: Option<&Caller>,
        id: i64,
        approve: bool,
        shown: Option<ApprovalRequest>,
        details: proposals::ProposalDecision,
    ) -> Result<ApprovalRequest> {
        match caller {
            Some(caller) => {
                self.decide_routed_approval(&caller.workspace_id, id, approve, shown)
                    .await
            }
            None => self.decide_approval(id, approve, shown, details).await,
        }
    }

    /// Record the decision and tell the requesting workspace's agents about a
    /// new one. With `shown`, the request the user was shown, a later request
    /// reusing its number is refused. Only proposals take `details`.
    pub async fn decide_approval(
        &self,
        id: i64,
        approve: bool,
        shown: Option<ApprovalRequest>,
        details: proposals::ProposalDecision,
    ) -> Result<ApprovalRequest> {
        let request = self.approval_request(id).await?.ok_or_else(|| {
            Refusal::new(Reason::NotFound, format!("unknown approval request {id}"))
        })?;
        crate::daemon::refusal::ensure_unchanged(
            shown.is_none_or(|shown| request.is_same(&shown)),
            format_args!("approval request {id}"),
        )?;
        if request.target.is_proposal() {
            return self.decide_proposal(request, approve, details).await;
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
    /// still its to decide and that it still holds a grant for the same thing.
    /// Once the grant ended, the request passes up instead.
    pub async fn decide_routed_approval(
        &self,
        coordinator: &str,
        id: i64,
        approve: bool,
        shown: Option<ApprovalRequest>,
    ) -> Result<ApprovalRequest> {
        let request = self.approval_request(id).await?.ok_or_else(|| {
            Refusal::new(Reason::NotFound, format!("unknown approval request {id}"))
        })?;
        crate::daemon::refusal::ensure_unchanged(
            shown.is_none_or(|shown| request.is_same(&shown)),
            format_args!("approval request {id}"),
        )?;
        let decided = self
            .record_routed_decision(coordinator, &request, approve)
            .await?;
        if let Some(decision) = decided {
            if decision.new {
                self.tell_requester(&decision.request).await;
            }
            return Ok(decision.request);
        }
        let gate = self.gates.lock_announcements().await;
        let routed = self.route_approval(&request, Some(coordinator)).await?;
        self.announce_approval(&request.workspace, &routed).await;
        drop(gate);
        bail!(
            "approval {id}: the grant for {} has ended, so the request passed up",
            request.subject()
        )
    }

    /// Decide `request` for `coordinator` in one transaction with the checks
    /// that it decides it; `None` when its grant has ended.
    async fn record_routed_decision(
        &self,
        coordinator: &str,
        request: &ApprovalRequest,
        approve: bool,
    ) -> Result<Option<Decision>> {
        let request = request.clone();
        let (coordinator, key, id) = (coordinator.to_owned(), request.key.clone(), request.id);
        let _hold = self.hold_decision_guard(&request, approve).await?;
        let _guard = self.gates.lock_simulators().await;
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let stored = by_key(&tx, &key)?.context("the approval request no longer exists")?;
                ensure!(
                    stored.decider.as_deref() == Some(coordinator.as_str()),
                    "approval request {id} is not routed to this workspace"
                );
                crate::daemon::refusal::ensure_unchanged(
                    stored.is_same(&request),
                    format_args!("approval request {id}"),
                )?;
                if !may_decide(&tx, &coordinator, &stored)? {
                    return Ok(None);
                }
                let decision = decide_and_place(&tx, &key, approve)?;
                tx.commit()?;
                Ok(Some(decision))
            })
            .await
    }

    /// Decide the stored `request` the user chose; later requests reusing its
    /// number are not affected.
    async fn record_decision(&self, request: ApprovalRequest, approve: bool) -> Result<Decision> {
        if request.target == Target::Land && approve && request.status == DecisionStatus::Pending {
            self.ensure_land_current(&request).await?;
        }
        let _hold = self.hold_decision_guard(&request, approve).await?;
        let key = request.key;
        // Simulator allocation checks approval and mutates devices in separate
        // transactions under this gate; a decision must not land between them.
        let _guard = self.gates.lock_simulators().await;
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let decision = decide_and_place(&tx, &key, approve)?;
                tx.commit()?;
                Ok(decision)
            })
            .await
    }

    /// Failure only logs; the decision stands.
    async fn tell_requester(&self, request: &ApprovalRequest) {
        self.tell_workspace(&request.workspace_id, request.decision_message())
            .await;
    }

    /// Queue `message` for the requesting workspace's agents. Failure only
    /// logs; the decision stands.
    pub(crate) async fn tell_workspace(&self, workspace_id: &str, message: String) {
        if let Err(error) = self.queue_agent_message(workspace_id, message).await {
            log!("approval decision message not queued: {error:#}");
        }
    }

    pub async fn show_approval(&self, id: i64) -> Result<ApprovalRequest> {
        self.approval_request(id).await?.ok_or_else(|| {
            Refusal::new(Reason::NotFound, format!("unknown approval request {id}")).into()
        })
    }

    /// A request's target never changes, so it can be read before taking the
    /// gate its decision needs.
    pub(crate) async fn approval_request(&self, id: i64) -> Result<Option<ApprovalRequest>> {
        self.store.run(move |db| by_number(db, id)).await
    }

    /// Tell whoever decides a pending request about it: the first coordinating
    /// agent above its workspace that holds a grant for the same thing, the
    /// coordinators passed over on the way, or else the user. A request is
    /// routed once; repeating it tells its decider again.
    pub async fn notify_approval(&self, workspace: &str, request: &ApprovalRequest) {
        let _gate = self.gates.lock_announcements().await;
        // It may have been withdrawn or decided since it was requested.
        let key = request.key.clone();
        let request = match self.store.run(move |db| by_key(db, &key)).await {
            Ok(Some(stored)) if stored.status == DecisionStatus::Pending => stored,
            Ok(_) => return,
            Err(error) => {
                log!("approval {} not rechecked: {error:#}", request.id);
                request.clone()
            }
        };
        let request = &request;
        // Publishing to a forge others see is the user's review, so proposals
        // never go to a coordinator.
        if request.target.is_proposal() {
            self.ask_user(workspace, request).await;
            return;
        }
        let routed = match request.routed {
            true => Ok(Routed {
                request: request.clone(),
                passed: Vec::new(),
            }),
            false => self.route_approval(request, None).await,
        };
        match routed {
            Ok(routed) => self.announce_approval(workspace, &routed).await,
            Err(error) => {
                log!("approval {} not routed: {error:#}", request.id);
                self.ask_user(workspace, request).await;
            }
        }
    }

    /// Pass the request the coordinator of `workspace` decides on to the next
    /// coordinating agent above it that holds a grant, or to the user. With
    /// `shown`, a later request reusing its number is refused.
    pub async fn escalate_approval(
        &self,
        workspace: &str,
        id: i64,
        shown: Option<ApprovalRequest>,
    ) -> Result<ApprovalRequest> {
        let workspace = self.workspace(workspace).await?;
        let request = self.approval_request(id).await?.ok_or_else(|| {
            Refusal::new(Reason::NotFound, format!("unknown approval request {id}"))
        })?;
        crate::daemon::refusal::ensure_unchanged(
            shown.is_none_or(|shown| request.is_same(&shown)),
            format_args!("approval request {id}"),
        )?;
        ensure!(
            request.decider.as_deref() == Some(workspace.id.as_str()),
            "approval request {id} is not {}'s to decide",
            workspace.name
        );
        let _gate = self.gates.lock_announcements().await;
        let routed = self.route_approval(&request, Some(&workspace.id)).await?;
        self.announce_approval(&request.workspace, &routed).await;
        Ok(routed.request)
    }

    /// Route `request` through the coordinators of its workspace, or through
    /// those above `above` when that coordinator passes it on.
    async fn route_approval(
        &self,
        request: &ApprovalRequest,
        above: Option<&str>,
    ) -> Result<Routed> {
        let agents = self.tracked_agents().await;
        let (key, workspace) = (request.key.clone(), request.workspace_id.clone());
        let above = above.map(str::to_owned);
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let candidates = match &above {
                    Some(above) => {
                        crate::daemon::workspace::ancestors_with_agents(&tx, above, &agents)?
                    }
                    None => crate::daemon::workspace::coordinators(&tx, &workspace, &agents)?,
                };
                let routed = route(&tx, &key, &candidates, above.as_deref())?;
                tx.commit()?;
                Ok(routed)
            })
            .await
    }

    /// Message the request's decider and the coordinators it passed over, or
    /// notify the user when no coordinator decides it.
    async fn announce_approval(&self, workspace: &str, routed: &Routed) {
        let request = &routed.request;
        let about = format!(
            "{workspace}: approval {}: {}: {}",
            request.id,
            request.subject(),
            request.reason
        );
        for passed in &routed.passed {
            self.tell_agent(
                passed,
                format!("{about}; passed up, since you hold no grant for it"),
            )
            .await;
        }
        let told = match &request.decider {
            Some(decider) => {
                self.tell_agent(
                    decider,
                    format!(
                        "{about}; decide with shoal approvals --approve {id} or shoal approvals --deny {id}, or pass it up with shoal approvals --escalate {id}",
                        id = request.id
                    ),
                )
                .await
            }
            None => false,
        };
        // The user decides what no coordinator could be told about.
        if !told {
            self.ask_user(workspace, request).await;
        }
    }

    /// Queue `message` for the workspace's agent; false when it cannot be.
    async fn tell_agent(&self, workspace: &str, message: String) -> bool {
        match self.queue_agent_message(workspace, message).await {
            Ok(()) => true,
            Err(error) => {
                log!("approval message not queued: {error:#}");
                false
            }
        }
    }

    async fn ask_user(&self, workspace: &str, request: &ApprovalRequest) {
        let about = About::workspace(workspace, &request.workspace_id).asking(&request.key);
        self.notify_about(
            about,
            NotificationKind::ApprovalRequested,
            request.notification(),
        )
        .await;
    }
}

/// `: <reason>` for a request that gives one; proposals give none.
fn reason_suffix(request: &ApprovalRequest) -> String {
    match request.reason.as_str() {
        "" => String::new(),
        reason => format!(": {reason}"),
    }
}

#[cfg(test)]
mod tests;
