//! Human decisions authorize cooperative allocations; they never reserve capacity.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Params, Transaction, params};
use std::fmt;

use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        events::{self, EventKind},
        item_numbers::{self, ItemKind, ItemNumber},
        log,
        notifications::{self, About, NotificationKind},
        scope::Caller,
        store,
        workspace::Manager,
    },
    model::PresentSpace,
};

pub mod proposals;

pub(crate) use crate::client::approvals::{
    ApprovalRequest, DecisionStatus, HoldSpecification, Lifetime, MERGE, MergeSpecification,
    PortSpecification, PushSpecification, ResourceSpecification, SecretSpecification,
    SimulatorSpecification, Specification, Target, Undecided, UnmergeSpecification,
};

/// Operations on the client type [`Target`] that only the `shoal` command uses.
pub(crate) trait TargetExt {
    /// Whether the request proposes a forge write the user may edit before
    /// approving, rather than granting access.
    fn is_proposal(&self) -> bool;

    /// Whether a coordinator decides the request only while it holds a grant
    /// for the same thing. A hold grants nothing a coordinator could hold, so
    /// the nearest one decides it.
    fn needs_grant(&self) -> bool;
}

impl TargetExt for Target {
    fn is_proposal(&self) -> bool {
        matches!(self, Self::IssueOpen | Self::PrOpen | Self::Push)
    }

    fn needs_grant(&self) -> bool {
        !matches!(self, Self::Hold(_))
    }
}

/// The permissions that govern `request`, each of which a coordinator needs
/// as `direct` to approve it instead of a grant; none for a request a grant
/// governs. They are those whose value made the space ask instead of act: an
/// issue in another repository is governed by `issue_other_repositories`
/// alone, and what pushes by `push` too.
fn governing(request: &ApprovalRequest) -> &'static [&'static str] {
    use proposals::ProposalExt;
    match request.target {
        Target::IssueOpen
            if (request.proposed_issue()).is_some_and(|issue| issue.repository.is_some()) =>
        {
            &["issue_other_repositories"]
        }
        Target::IssueOpen => &["issue_open"],
        Target::PrOpen if request.pending_push().is_some() => &["pr_open", "push"],
        Target::PrOpen => &["pr_open"],
        Target::Push => &["push"],
        Target::Merge | Target::Unmerge => &["merge", "push"],
        _ => &[],
    }
}

/// A stored request's key, record, active flag, numbers, and whether its
/// space is gone, which only a proposed issue outlives.
macro_rules! select {
    ($($filter:literal)?) => {
        concat!(
            "SELECT id,record,active,space_number,number,
                workspace_id IS NULL AND NOT EXISTS (SELECT 1 FROM workspaces
                        WHERE workspaces.number=access_requests.space_number
                    UNION ALL SELECT 1 FROM chats WHERE chats.number=access_requests.space_number)
            FROM access_requests "$(, $filter)?
        )
    };
}

const COLUMNS: &str = select!();
const BY_KEY: &str = select!("WHERE id=?1");
const BY_NUMBER: &str = select!("WHERE space_number=?1 AND number=?2");
const CURRENT: &str = select!("WHERE workspace_id=?1 AND target_key=?2 AND name=?3 AND active=1");
const CANDIDATES: &str = select!("WHERE workspace_id=?1 AND target_key=?2 ORDER BY rowid");

/// Whether a request the repository owns, an issue proposal, was made by
/// the space `?1`; a malformed record of another space's history stays out
/// of the selection.
const REPOSITORY_OWNED: &str = "(workspace_id IS NULL
    AND CASE WHEN json_valid(record) THEN json_extract(record, '$.space_id') END=?1)";

/// A stored request's key, record, active flag, number and whether its
/// space is gone.
type RecordRow = (String, String, bool, ItemNumber, bool);

fn decode_record((key, record, active, number, removed): RecordRow) -> Result<ApprovalRequest> {
    let mut request: ApprovalRequest = serde_json::from_str(&record)
        .with_context(|| format!("approval request {number} has an invalid record"))?;
    request.key = key;
    request.number = number;
    request.active = active;
    request.space_removed = removed;
    Ok(request)
}

fn record_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RecordRow> {
    let number = ItemNumber {
        space: row.get(3)?,
        item: row.get(4)?,
    };
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, number, row.get(5)?))
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

/// The requests routed to the space `id`, in the order they were made; a
/// malformed record of another space's history stays out of the selection.
fn list_decided(db: &Connection, id: &str) -> Result<Vec<ApprovalRequest>> {
    db.prepare(&format!(
        "{COLUMNS} WHERE CASE WHEN json_valid(record) THEN json_extract(record, '$.decider') END=?1
            ORDER BY rowid"
    ))?
    .query_map([id], record_row)?
    .map(|row| decode_record(row?))
    .collect()
}

/// The issues the chat `id` proposed and the requests routed to it to
/// decide, in the order they were made. A chat proposes nothing else, and
/// its issue proposals belong to its repository.
fn list_of_chat(db: &Connection, id: &str) -> Result<Vec<ApprovalRequest>> {
    db.prepare(&format!(
        "{COLUMNS} WHERE {REPOSITORY_OWNED}
            OR CASE WHEN json_valid(record) THEN json_extract(record, '$.decider') END=?1
            ORDER BY rowid"
    ))?
    .query_map([id], record_row)?
    .map(|row| decode_record(row?))
    .collect()
}

/// How many requests of the space `id` wait for a decision: those a
/// workspace made, or those a `chat` made or decides.
pub(crate) fn pending_count(db: &Connection, id: &str, chat: bool) -> Result<usize> {
    let requests = match chat {
        true => list_of_chat(db, id)?,
        false => list(db, Some(id))?,
    };
    let pending = |request: &&ApprovalRequest| request.status == DecisionStatus::Pending;
    Ok(requests.iter().filter(pending).count())
}

/// Give the pending requests the space `id` decides back to the user, as
/// it ends, is saved or is removed, in the caller's transaction; the
/// requests given back.
pub(crate) fn release_decided_by(tx: &Transaction<'_>, id: &str) -> Result<Vec<ApprovalRequest>> {
    let mut released = list_decided(tx, id)?;
    released.retain(|request| request.status == DecisionStatus::Pending);
    for request in &mut released {
        request.decider = None;
        write_record(tx, request)?;
    }
    Ok(released)
}

/// Give the request stored under `key` back to the user while it is pending
/// and still `decider`'s, in the caller's transaction.
fn give_back(tx: &Transaction<'_>, key: &str, decider: Option<&str>) -> Result<()> {
    let Some(mut stored) = by_key(tx, key)? else {
        return Ok(());
    };
    if stored.status == DecisionStatus::Pending && stored.decider.as_deref() == decider {
        stored.decider = None;
        write_record(tx, &stored)?;
    }
    Ok(())
}

/// Store `request`'s record in place of the one under its key.
fn write_record(db: &Connection, request: &ApprovalRequest) -> Result<()> {
    db.execute(
        "UPDATE access_requests SET record=?2 WHERE id=?1",
        params![request.key, serde_json::to_string(request)?],
    )?;
    Ok(())
}

fn by_number(db: &Connection, number: ItemNumber) -> Result<Option<ApprovalRequest>> {
    lookup(db, BY_NUMBER, params![number.space, number.item])
}

/// Every request, or those `owner` made, including the issues it proposed
/// for its repository.
pub fn list(db: &Connection, owner: Option<&str>) -> Result<Vec<ApprovalRequest>> {
    db.prepare(&format!(
        "{COLUMNS} WHERE ?1 IS NULL OR workspace_id=?1
            OR {REPOSITORY_OWNED}
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
    store::require_ready(tx, &request.space_id)?;
    if let Some(existing) = current(tx, &request.space_id, &request.target, &request.name)? {
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
    request.space = tx.query_row(
        "SELECT name FROM workspaces WHERE id=?1",
        [&request.space_id],
        |r| r.get(0),
    )?;
    request.key = uuid::Uuid::new_v4().to_string();
    request.number = item_numbers::next(tx, &request.space_id)?;
    tx.execute("INSERT INTO access_requests(id,workspace_id,target_key,name,record,space_number,number) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![request.key,request.space_id,request.target,request.name,serde_json::to_string(&request)?,request.number.space,request.number.item])?;
    events::record_approval(tx, EventKind::ApprovalRequested, &request)?;
    Ok(Some(request))
}

fn reusable_grant(db: &Connection, request: &ApprovalRequest) -> Result<bool> {
    // Decode every candidate before matching: even a later malformed record must
    // fail the check. Released workspace grants remain candidates.
    let candidates = db
        .prepare(CANDIDATES)?
        .query_map(params![request.space_id, request.target], record_row)?
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

/// Operations on the client type [`ApprovalRequest`] that only the `shoal` command uses.
pub(crate) trait ApprovalRequestExt {
    fn new(
        owner: &str,
        target: Target,
        name: &str,
        specification: Specification,
        lifetime: Lifetime,
        reason: Option<&str>,
    ) -> Self;

    /// Whether `other` is this request as recorded, though its decision may
    /// differ: the same key, workspace, target, settings, reason and time.
    fn is_same(&self, other: &ApprovalRequest) -> bool;

    /// What the user is told about a request no coordinator decides,
    /// including how to approve it.
    fn notification(&self) -> String;

    /// What its decider is told about a withdrawn request whose
    /// notification was already shown.
    fn withdrawal_notification(&self) -> String;

    /// The agent message naming a decision and what the requesting agent does next.
    fn decision_message(&self) -> String;
}

impl ApprovalRequestExt for ApprovalRequest {
    fn new(
        owner: &str,
        target: Target,
        name: &str,
        specification: Specification,
        lifetime: Lifetime,
        reason: Option<&str>,
    ) -> Self {
        Self {
            number: ItemNumber::default(),
            key: String::new(),
            space_id: owner.into(),
            space: String::new(),
            space_removed: false,
            repository_id: None,
            target,
            name: name.into(),
            specification,
            lifetime,
            reason: reason.unwrap_or_default().into(),
            status: DecisionStatus::Pending,
            created_at: crate::time::UnixTime::now(),
            decided_at: None,
            active: true,
            routed: false,
            decider: None,
            url: None,
            warnings: Vec::new(),
            operation: None,
        }
    }

    fn is_same(&self, other: &ApprovalRequest) -> bool {
        self.number == other.number
            && self.key == other.key
            && self.space_id == other.space_id
            && self.target == other.target
            && self.name == other.name
            && self.specification == other.specification
            && self.lifetime == other.lifetime
            && self.reason == other.reason
            && self.created_at == other.created_at
    }

    fn notification(&self) -> String {
        format!(
            "approval {}: {}{}; approve with shoal approvals --approve {}",
            self.number,
            self.subject(),
            reason_suffix(self),
            self.number
        )
    }

    fn withdrawal_notification(&self) -> String {
        format!(
            "approval {}: {} was withdrawn; no decision needed",
            self.number,
            self.subject()
        )
    }

    fn decision_message(&self) -> String {
        let next = match self.status {
            DecisionStatus::Approved => "retry the command that requested it",
            _ => "do not retry it",
        };
        format!(
            "Approval {} for {} was {}: {next}.",
            self.number,
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

/// Whether the coordinating agent of `workspace` may approve `request` as
/// far as grants go: one that needs no grant, one its permissions decide
/// instead, or one it holds a grant for.
fn may_decide(db: &Connection, workspace: &str, request: &ApprovalRequest) -> Result<bool> {
    Ok(!request.target.needs_grant()
        || !governing(request).is_empty()
        || holds_grant(db, workspace, request)?)
}

/// Give the pending request stored under `key` to the first of `candidates`,
/// or to the user. Without `from` this is its first routing, which a routed
/// request keeps; with `from`, the space that decides it passes it on.
fn route(
    tx: &Transaction<'_>,
    key: &str,
    candidates: &[String],
    from: Option<&str>,
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
            request.decider.as_deref() == Some(from),
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

/// The space's request `number`, as the agent names it to withdraw.
fn own_request(
    db: &Connection,
    space: &PresentSpace,
    number: ItemNumber,
) -> Result<ApprovalRequest> {
    by_number(db, number)?
        .filter(|request| request.space_id == space.id())
        .with_context(|| {
            format!(
                "{} {} has no approval request {number}",
                space.kind(),
                space.name()
            )
        })
}

/// Close the pending request `selected`, which its agent no longer needs. A
/// proposed issue goes as a decided one does, since it would outlive the
/// workspace; any other request stays as withdrawn, like an outdated one.
/// The stored request is found by its key, so a later request that took the
/// number since it was selected is never withdrawn in its place.
fn withdraw(tx: &Transaction<'_>, selected: &ApprovalRequest) -> Result<ApprovalRequest> {
    let id = selected.number;
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

/// Replace a merge or unmerge request for another commit, another push or
/// another warning with `request`. Returns the request still awaiting or
/// refused a decision; an approval for the same merge lets it proceed. A
/// request for the same merge keeps the reason it was decided on, though the
/// commit count or open PRs it names changed since.
pub fn request_merge(
    tx: &Transaction<'_>,
    mut request: ApprovalRequest,
) -> Result<Option<ApprovalRequest>> {
    let owner = request.space_id.clone();
    if let Some(existing) = current(tx, &owner, &request.target, MERGE)? {
        if existing.specification == request.specification {
            request.reason = existing.reason;
        } else {
            retire(tx, &existing)?;
        }
    }
    check(tx, request)
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
    store::require_ready(tx, &request.space_id)?;
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
    /// [`check`] in its own transaction, for allocations that claim nothing else.
    pub async fn request_approval(
        &self,
        request: ApprovalRequest,
    ) -> Result<Option<ApprovalRequest>> {
        self.store.write(move |tx| check(tx, request)).await
    }

    /// [`request_merge`] in its own transaction.
    pub async fn request_merge_approval(
        &self,
        request: ApprovalRequest,
    ) -> Result<Option<ApprovalRequest>> {
        self.store.write(move |tx| request_merge(tx, request)).await
    }

    /// Whether a PR proposal of the workspace waits for the user's decision.
    pub async fn has_pending_pull_proposal(&self, workspace_id: &str) -> Result<bool> {
        let id = workspace_id.to_owned();
        self.store
            .run(move |db| proposals::pull_pending(db, &id))
            .await
    }

    /// Whether a push request of the workspace waits for the user's decision.
    pub async fn has_pending_push(&self, workspace_id: &str) -> Result<bool> {
        let id = workspace_id.to_owned();
        self.store
            .run(move |db| proposals::push_pending(db, &id))
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
            .write(move |tx| {
                store::require_ready(tx, &owner)?;
                let released = release(tx, &owner, &target, &name)?;
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
        self.store.write(move |tx| record(tx, request)).await
    }

    /// [`proposals::save`] in its own transaction.
    pub(crate) async fn save_proposal(&self, request: ApprovalRequest) -> Result<()> {
        self.store
            .write(move |tx| proposals::save(tx, &request))
            .await
    }

    /// [`proposals::remove`] in its own transaction, once `request` is
    /// decided, recording the decision in its space's log.
    pub(crate) async fn forget_proposal(&self, request: &ApprovalRequest) -> Result<()> {
        let request = request.clone();
        self.store
            .write(move |tx| {
                events::record_approval(tx, EventKind::ApprovalDecided, &request)?;
                proposals::remove(tx, &request.key)
            })
            .await
    }

    /// Withdraw the pending request `number` of the space `selector`
    /// names, which its agent no longer needs, and take back its
    /// notification while unread; once shown, its decider is told instead.
    pub async fn withdraw_approval(
        &self,
        selector: &SpaceSelector,
        number: ItemNumber,
    ) -> Result<ApprovalRequest> {
        let space = self.space(selector).await?;
        let space_id = space.id().to_owned();
        let selected = self
            .store
            .run(move |db| own_request(db, &space, number))
            .await?;
        // Deciding a proposal opens its item under the space's gate before
        // forgetting the request, which withdrawal must not split.
        let _decision = match selected.target.is_proposal() {
            true => Some(self.gates.lock_prs([space_id]).await),
            false => None,
        };
        if let Some(current) = self.approval_by_key(&selected.key).await? {
            self.ensure_not_pushing(&current)?;
        }
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
            .write(move |tx| {
                let request = withdraw(tx, &selected)?;
                let retracted = notifications::retract_about(
                    tx,
                    NotificationKind::ApprovalRequested,
                    &request.key,
                )?;
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
                .tell_agent(decider, format!("{}: {message}", request.space))
                .await
        {
            return;
        }
        self.notify(
            Some(&request.space),
            NotificationKind::ApprovalWithdrawn,
            message,
        )
        .await;
    }

    /// [`retire`] in its own transaction, such as for a stale request.
    pub async fn retire_approval(&self, request: ApprovalRequest) -> Result<()> {
        self.store.write(move |tx| retire(tx, &request)).await
    }

    /// Every request, or those of the workspace `selector` names, or those
    /// the chat it names made or decides.
    pub async fn approval_requests(
        &self,
        selector: Option<&SpaceSelector>,
    ) -> Result<Vec<ApprovalRequest>> {
        if let Some(selector) = selector
            && let PresentSpace::Chat(chat) = self.space(selector).await?
        {
            return self.store.run(move |db| list_of_chat(db, &chat.id)).await;
        }
        let owner = self.workspace_filter(selector).await?;
        self.store.run(move |db| list(db, owner.as_deref())).await
    }

    /// Decide request `number` as `caller`: a workspace process decides
    /// only what was routed to it, and denies a proposal with a reason at
    /// most.
    pub(super) async fn decide_approval_as(
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
        coordinator: &str,
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
        if approve {
            self.ensure_permitted(coordinator, &request).await?;
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

    /// Refuse `coordinator`'s approval of `request` unless its effective
    /// permission is `direct` for each permission that governs the request,
    /// resolved in the repository of the request's action.
    async fn ensure_permitted(&self, coordinator: &str, request: &ApprovalRequest) -> Result<()> {
        let keys = governing(request);
        if keys.is_empty() {
            return Ok(());
        }
        let repository = match &request.repository_id {
            Some(repository) => Some(repository.clone()),
            None => (self.space_by_id(&request.space_id).await?)
                .repository_id()
                .map(str::to_owned),
        };
        let permissions = self
            .space_permissions(coordinator, repository.as_deref())
            .await?;
        if let Some(withheld) = permissions.withheld(keys) {
            let id = request.number;
            bail!(
                "approval {id}: {withheld}; deny it or pass it up with shoal approvals --escalate {id}"
            );
        }
        Ok(())
    }

    /// Decide `request` for `coordinator` in one transaction with the checks
    /// that it decides it and, to approve one a grant governs, holds a grant
    /// for it.
    async fn record_routed_decision(
        &self,
        coordinator: &str,
        request: &ApprovalRequest,
        approve: bool,
    ) -> Result<Decision> {
        if request.target == Target::Merge && approve && request.status == DecisionStatus::Pending {
            self.ensure_merge_current(request).await?;
        }
        let request = request.clone();
        let (coordinator, key, id) = (coordinator.to_owned(), request.key.clone(), request.number);
        let _hold = self.hold_decision_guard(&request, approve).await?;
        let _guard = self.gates.lock_simulators().await;
        self.store
            .write(move |tx| {
                let stored = by_key(tx, &key)?.context("the approval request no longer exists")?;
                ensure!(
                    stored.decider.as_deref() == Some(coordinator.as_str()),
                    "approval request {id} is not routed to this workspace"
                );
                crate::refusal::ensure_unchanged(
                    stored.is_same(&request),
                    format_args!("approval request {id}"),
                )?;
                ensure!(
                    !approve || may_decide(tx, &coordinator, &stored)?,
                    "approval {id}: this workspace holds no grant for {}; deny it or pass it up with shoal approvals --escalate {id}",
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
        if request.target == Target::Merge && approve && request.status == DecisionStatus::Pending {
            self.ensure_merge_current(&request).await?;
        }
        let _hold = self.hold_decision_guard(&request, approve).await?;
        let key = request.key;
        // Simulator allocation checks approval and mutates devices in separate
        // transactions under this gate; a decision must not land between them.
        let _guard = self.gates.lock_simulators().await;
        self.store
            .write(move |tx| decide_and_place(tx, &key, approve))
            .await
    }

    /// Failure only logs; the decision stands.
    async fn tell_requester(&self, request: &ApprovalRequest) {
        self.tell_space(&request.space_id, request.decision_message())
            .await;
    }

    /// Queue `message` for the agent of the requesting space. Failure only
    /// logs; the decision stands.
    pub(crate) async fn tell_space(&self, id: &str, message: String) {
        let queued = self.queue_space_message(id, None, message, Vec::new());
        if let Err(error) = queued.await {
            log!("approval decision message not queued: {error:#}");
        }
    }

    /// The request stored under `key`, whatever number it has.
    pub(crate) async fn approval_by_key(&self, key: &str) -> Result<Option<ApprovalRequest>> {
        let key = key.to_owned();
        self.store.run(move |db| by_key(db, &key)).await
    }

    /// The request `number`, refusing a number that names none, or that
    /// names a question.
    pub async fn show_approval(&self, number: ItemNumber) -> Result<ApprovalRequest> {
        self.store
            .run(move |db| match by_number(db, number)? {
                Some(request) => Ok(request),
                None => Err(item_numbers::unknown(db, ItemKind::Approval, number)?),
            })
            .await
    }

    /// A request's target never changes, so it can be read before taking the
    /// gate its decision needs.
    pub(crate) async fn approval_request(
        &self,
        number: ItemNumber,
    ) -> Result<Option<ApprovalRequest>> {
        self.store.run(move |db| by_number(db, number)).await
    }

    /// Tell whoever decides a pending request about it: the space directly
    /// above its workspace, a parent workspace with an agent or the chat that
    /// opened it, or else the user. A request is routed once; repeating it
    /// tells its decider again.
    pub async fn notify_approval(&self, workspace: &str, request: &ApprovalRequest) {
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
            Ok(routed) => self.announce_approval(workspace, &routed).await,
            Err(error) => {
                log!("approval {} not routed: {error:#}", request.number);
                self.ask_user(workspace, request).await;
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
            request.decider.as_deref() == Some(space.id()),
            "approval request {number} is not {}'s to decide",
            space.name()
        );
        let _gate = self.gates.lock_announcements().await;
        let routed = self.route_approval(&request, Some(space.id())).await?;
        self.announce_approval(&request.space, &routed).await;
        // A decider that could not be told gave the request to the user. Its
        // key, unlike its number, never names another request.
        Ok(self.approval_by_key(&routed.key).await?.unwrap_or(routed))
    }

    /// Route `request` to the space above its workspace, or to the one above
    /// `above` when that space passes it on.
    async fn route_approval(
        &self,
        request: &ApprovalRequest,
        above: Option<&str>,
    ) -> Result<ApprovalRequest> {
        let agents = self.tracked_agents().await;
        let (key, workspace) = (request.key.clone(), request.space_id.clone());
        let above = above.map(str::to_owned);
        self.store
            .write(move |tx| {
                // Passed up, it goes on above its decider, whatever became
                // of the requester's agent since it was routed.
                let candidates = match &above {
                    Some(above) => crate::daemon::workspace::spaces_above(tx, above, &agents)?,
                    None => crate::daemon::workspace::deciders(tx, &workspace, &agents)?,
                };
                let routed = route(tx, &key, &candidates, above.as_deref())?;
                Ok(routed)
            })
            .await
    }

    /// Message the request's decider, or notify the user when no space
    /// decides it or its decider cannot be told.
    async fn announce_approval(&self, workspace: &str, request: &ApprovalRequest) {
        let about = format!(
            "{workspace}: approval {}: {}: {}",
            request.number,
            request.subject(),
            request.reason
        );
        let told = match &request.decider {
            Some(decider) => {
                self.tell_agent(
                    decider,
                    format!(
                        "{about}; decide with shoal approvals --approve {id} or shoal approvals --deny {id}, or pass it up with shoal approvals --escalate {id}",
                        id = request.number
                    ),
                )
                .await
            }
            None => false,
        };
        // The user decides what no space could be told about.
        if !told {
            if request.decider.is_some() {
                self.release_to_user(request).await;
            }
            self.ask_user(workspace, request).await;
        }
    }

    /// Tell the user about requests handed to them, such as those of a chat
    /// that ended, which [`release_decided_by`] gave back in the same
    /// transaction that ended it.
    pub(crate) async fn ask_user_about(&self, requests: &[ApprovalRequest]) {
        // Under the gate a withdrawal takes, with each request read again, so
        // one withdrawn meanwhile notifies nobody.
        let _gate = self.gates.lock_announcements().await;
        for request in requests {
            let stored = match self.approval_by_key(&request.key).await {
                Ok(stored) => stored,
                Err(error) => {
                    log!("approval {} not rechecked: {error:#}", request.number);
                    continue;
                }
            };
            if let Some(stored) = stored.filter(|stored| {
                stored.status == DecisionStatus::Pending && stored.decider.is_none()
            }) {
                self.ask_user(&stored.space, &stored).await;
            }
        }
    }

    /// Give the request back to the user when its decider cannot be told,
    /// so it is the user's to decide and their inbox lists it.
    async fn release_to_user(&self, request: &ApprovalRequest) {
        let (key, decider) = (request.key.clone(), request.decider.clone());
        let released = self
            .store
            .write(move |tx| give_back(tx, &key, decider.as_deref()))
            .await;
        if let Err(error) = released {
            log!("approval {} not given back: {error:#}", request.number);
        }
    }

    /// Queue `message` for the agent of the space `space`; false when it
    /// cannot be.
    async fn tell_agent(&self, space: &str, message: String) -> bool {
        let queued = (self.queue_space_message(space, None, message, Vec::new())).await;
        match queued {
            Ok(()) => true,
            Err(error) => {
                log!("approval message not queued: {error:#}");
                false
            }
        }
    }

    async fn ask_user(&self, workspace: &str, request: &ApprovalRequest) {
        let about = About::space(workspace, &request.space_id).asking(&request.key);
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
