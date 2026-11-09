//! The request store: recording, reading, withdrawing, releasing and retiring
//! approval requests.
use crate::client::space_ids::SpaceId;
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Params, Transaction, params};
use std::fmt;

use super::{ApprovalRequest, DecisionStatus, Lifetime, MERGE, Target, TargetExt, proposals};
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        events::{self, EventKind},
        item_numbers::{self, ItemKind, ItemNumber},
        notifications::{self, NotificationKind},
        store,
        workspace::Manager,
    },
    model::PresentSpace,
};

/// A stored request's key, record, active flag, numbers, and whether its
/// space is gone, which only a proposed issue outlives.
macro_rules! select {
    ($($filter:literal)?) => {
        concat!(
            "SELECT id,record,active,space_number,number,
                workspace_id IS NULL AND NOT EXISTS (SELECT 1 FROM spaces
                    WHERE spaces.number=access_requests.space_number)
            FROM access_requests "$(, $filter)?
        )
    };
}

pub(super) const COLUMNS: &str = select!();
pub(super) const BY_KEY: &str = select!("WHERE id=?1");
const BY_NUMBER: &str = select!("WHERE space_number=?1 AND number=?2");
pub(super) const CURRENT: &str =
    select!("WHERE workspace_id=?1 AND target_key=?2 AND name=?3 AND active=1");
pub(super) const CANDIDATES: &str =
    select!("WHERE workspace_id=?1 AND target_key=?2 ORDER BY rowid");

/// Whether a request the repository owns, an issue proposal, was made by
/// the space `?1`; a malformed record of another space's history stays out
/// of the selection.
const REPOSITORY_OWNED: &str = "(workspace_id IS NULL
    AND CASE WHEN json_valid(record) THEN json_extract(record, '$.space_id') END=?1)";

/// A stored request's key, record, active flag, number and whether its
/// space is gone.
pub(super) type RecordRow = (String, String, bool, ItemNumber, bool);

pub(super) fn decode_record(
    (key, record, active, number, removed): RecordRow,
) -> Result<ApprovalRequest> {
    let mut request: ApprovalRequest = serde_json::from_str(&record)
        .with_context(|| format!("approval request {number} has an invalid record"))?;
    request.key = key;
    request.number = number;
    request.active = active;
    request.space_removed = removed;
    Ok(request)
}

pub(super) fn record_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RecordRow> {
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

pub(super) fn by_key(db: &Connection, key: &str) -> Result<Option<ApprovalRequest>> {
    lookup(db, BY_KEY, [key])
}

/// The requests routed to the space `id`, in the order they were made; a
/// malformed record of another space's history stays out of the selection.
fn list_decided(db: &Connection, id: &SpaceId) -> Result<Vec<ApprovalRequest>> {
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
pub(super) fn list_of_chat(db: &Connection, id: &SpaceId) -> Result<Vec<ApprovalRequest>> {
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
pub(crate) fn pending_count(db: &Connection, id: &SpaceId, chat: bool) -> Result<usize> {
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
pub(crate) fn release_decided_by(
    tx: &Transaction<'_>,
    id: &SpaceId,
) -> Result<Vec<ApprovalRequest>> {
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
pub(super) fn give_back(tx: &Transaction<'_>, key: &str, decider: Option<&SpaceId>) -> Result<()> {
    let Some(mut stored) = by_key(tx, key)? else {
        return Ok(());
    };
    if stored.status == DecisionStatus::Pending && stored.decider.as_ref() == decider {
        stored.decider = None;
        write_record(tx, &stored)?;
    }
    Ok(())
}

/// Store `request`'s record in place of the one under its key.
pub(super) fn write_record(db: &Connection, request: &ApprovalRequest) -> Result<()> {
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
pub fn list(db: &Connection, owner: Option<&SpaceId>) -> Result<Vec<ApprovalRequest>> {
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
    owner: &SpaceId,
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
    request.space = Some(tx.query_row(
        "SELECT name FROM workspaces WHERE id=?1",
        [&request.space_id],
        |r| r.get(0),
    )?);
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
        .filter(|request| request.space_id == *space.id())
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
pub(super) fn withdraw(
    tx: &Transaction<'_>,
    selected: &ApprovalRequest,
) -> Result<ApprovalRequest> {
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
pub fn release(tx: &Transaction<'_>, owner: &SpaceId, target: &Target, name: &str) -> Result<bool> {
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
    pub async fn has_pending_pull_proposal(&self, workspace_id: &SpaceId) -> Result<bool> {
        let id = workspace_id.to_owned();
        self.store
            .run(move |db| proposals::pull_pending(db, &id))
            .await
    }

    /// Whether a push request of the workspace waits for the user's decision.
    pub async fn has_pending_push(&self, workspace_id: &SpaceId) -> Result<bool> {
        let id = workspace_id.to_owned();
        self.store
            .run(move |db| proposals::push_pending(db, &id))
            .await
    }

    /// [`release`] in its own transaction, once the workspace is ready.
    pub async fn release_approval(
        &self,
        owner: SpaceId,
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
        self.store.run(move |db| list(db, owner.as_ref())).await
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
}
