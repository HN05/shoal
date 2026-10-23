//! Human decisions authorize cooperative allocations; they never reserve capacity.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Params, Transaction, params};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

use crate::{
    config::repo::ConflictPolicy,
    daemon::{
        log,
        notifications::{self, NotificationKind},
        resources::{Definition, LockMode, Scope},
        store,
        workspace::Manager,
    },
    model::Workspace,
    sim::Profile,
    state::{states, text_key},
};

pub mod proposals;

use proposals::{IssueSpecification, PullSpecification};

states!(
    #[derive(Default)]
    Lifetime {
        #[default]
        Lease => "lease",
        Workspace => "workspace",
    }
);

states!(DecisionStatus {
    Pending => "pending",
    Approved => "approved",
    Denied => "denied",
    Outdated => "outdated",
    /// The requesting agent no longer needed it.
    Withdrawn => "withdrawn",
});

/// What a decision authorizes; one active request per workspace, target and
/// name. Keyed as `resource/<scope>/<pool>`, `port/<name>`, `simulator`,
/// `secret/<name>`, `land`, `issue_open` or `pr_open`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    Resource {
        scope: Scope,
        pool: String,
    },
    Port(String),
    Simulator,
    /// Fetching a configured secret; the daemon records the grant, never the value.
    Secret(String),
    /// Landing a workspace branch into the default branch.
    Land,
    /// Opening an issue a scoped caller proposed; the repository owns the
    /// request, so it outlives the workspace.
    IssueOpen,
    /// Opening a PR for the workspace branch at one commit.
    PrOpen,
}

/// The name of a workspace's single land request.
pub const LAND: &str = "branch";

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resource { scope, pool } => write!(f, "resource/{scope}/{pool}"),
            Self::Port(name) => write!(f, "port/{name}"),
            Self::Simulator => f.write_str("simulator"),
            Self::Secret(name) => write!(f, "secret/{name}"),
            Self::Land => f.write_str("land"),
            Self::IssueOpen => f.write_str("issue_open"),
            Self::PrOpen => f.write_str("pr_open"),
        }
    }
}

impl FromStr for Target {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        match text {
            "simulator" => return Ok(Self::Simulator),
            "land" => return Ok(Self::Land),
            "issue_open" => return Ok(Self::IssueOpen),
            "pr_open" => return Ok(Self::PrOpen),
            _ => {}
        }
        if let Some(name) = text.strip_prefix("port/")
            && !name.is_empty()
        {
            return Ok(Self::Port(name.into()));
        }
        if let Some(name) = text.strip_prefix("secret/")
            && !name.is_empty()
        {
            return Ok(Self::Secret(name.into()));
        }
        // The scope may itself contain a slash; pool names cannot.
        if let Some((scope, pool)) = text
            .strip_prefix("resource/")
            .and_then(|rest| rest.rsplit_once('/'))
            && !pool.is_empty()
        {
            return Ok(Self::Resource {
                scope: scope.parse()?,
                pool: pool.into(),
            });
        }
        bail!("invalid approval target: {text}")
    }
}
text_key!(Target);

impl Target {
    /// Whether the request proposes a forge write the user may edit before
    /// approving, rather than granting access.
    pub fn is_proposal(&self) -> bool {
        matches!(self, Self::IssueOpen | Self::PrOpen)
    }
}

/// The effective allocation settings a decision binds; a later request with
/// different settings cannot reuse the grant. Each kind keeps the JSON shape
/// its allocator has always recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Specification {
    Resource(ResourceSpecification),
    Port(PortSpecification),
    Simulator(SimulatorSpecification),
    Secret(SecretSpecification),
    Land(LandSpecification),
    Issue(IssueSpecification),
    Pull(PullSpecification),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSpecification {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<crate::daemon::resources::RepositoryView>,
    pub definition: Definition,
    /// The pool member the decision selects.
    pub member: String,
    pub mode: LockMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortSpecification {
    pub env: String,
    pub on_conflict: ConflictPolicy,
    pub preferred: Option<u16>,
    /// Automatic range as `[start, end]`.
    pub range: [u16; 2],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimulatorSpecification {
    pub clean: bool,
    pub profile: Profile,
}

/// A land decision covers the branch at one commit; later commits need a new one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LandSpecification {
    pub branch: String,
    pub head: String,
}

/// A secret grant covers the configured source; changing the command or the
/// tools it serves needs a new decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretSpecification {
    pub command: Vec<String>,
    pub tools: Vec<String>,
}

impl fmt::Display for Specification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&serde_json::to_string(self).map_err(|_| fmt::Error)?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// The number the user decides it by; reused once the request is deleted.
    #[serde(default)]
    pub id: i64,
    /// The stored request's own key, which is never reused or shown.
    #[serde(skip)]
    pub key: String,
    pub workspace_id: String,
    #[serde(default)]
    pub workspace: String,
    /// The repository that owns a proposed issue, which outlives the
    /// workspace that proposed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<String>,
    pub target: Target,
    pub name: String,
    pub specification: Specification,
    pub lifetime: Lifetime,
    pub reason: String,
    pub status: DecisionStatus,
    pub created_at: u64,
    pub decided_at: Option<u64>,
    /// False for workspace grants retained after release.
    pub active: bool,
    /// Whether the request went up the workspace tree to find its decider.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub routed: bool,
    /// The workspace whose coordinating agent decides the request, since it
    /// holds a grant for the same thing; the user decides without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decider: Option<String>,
    /// The issue or PR an approved proposal opened, or the issue a denied
    /// one duplicates; decided proposals are no longer stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// What deciding a proposal could not finish, such as attaching the
    /// files of a duplicate's comment that was posted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
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
    /// differ: the same workspace, target, settings, reason and time.
    fn is_same(&self, other: &ApprovalRequest) -> bool {
        self.workspace_id == other.workspace_id
            && self.target == other.target
            && self.name == other.name
            && self.specification == other.specification
            && self.lifetime == other.lifetime
            && self.reason == other.reason
            && self.created_at == other.created_at
    }

    /// What the user is asked to decide: the target and lease name, the
    /// branch and commit to land, or the issue or PR to open.
    pub fn subject(&self) -> String {
        match (&self.target, &self.specification) {
            (Target::Land, Specification::Land(land)) => {
                format!("land {} at {}", land.branch, land.head)
            }
            (Target::IssueOpen, Specification::Issue(issue)) => {
                format!("open issue \"{}\"", issue.issue.title)
            }
            (Target::PrOpen, Specification::Pull(pull)) => format!(
                "open PR \"{}\" from {} at {}",
                pull.pull.title, pull.pull.head, pull.commit
            ),
            _ => format!("{} / {} ({})", self.target, self.name, self.lifetime),
        }
    }

    /// What the user is told about a request no coordinator decides,
    /// including how to approve it.
    fn notification(&self) -> String {
        format!(
            "approval {}: {}{}; approve with shoal approve {}",
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
        if holds_grant(tx, candidate, &request)? {
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
        let selected = self
            .store
            .run(move |db| own_request(db, &workspace, id))
            .await?;
        // Deciding a proposal opens its item under this gate before
        // forgetting the request, which withdrawal must not split.
        let _gate = match selected.target.is_proposal() {
            true => Some(self.pr_gate.lock().await),
            false => None,
        };
        let (request, retracted) = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let request = withdraw(&tx, &selected)?;
                let retracted = notifications::retract(
                    &tx,
                    &request.workspace_id,
                    NotificationKind::ApprovalRequested,
                    &request.notification(),
                )?;
                tx.commit()?;
                Ok((request, retracted))
            })
            .await?;
        if request.target.is_proposal() {
            self.prune_proposal_files().await;
        }
        if !retracted {
            self.announce_withdrawal(&request).await;
        }
        Ok(request)
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
        let request = self
            .approval_request(id)
            .await?
            .with_context(|| format!("unknown approval request {id}"))?;
        ensure!(
            shown.is_none_or(|shown| request.is_same(&shown)),
            "approval request {id} changed since it was shown; choose again"
        );
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
    ) -> Result<ApprovalRequest> {
        let request = self
            .approval_request(id)
            .await?
            .with_context(|| format!("unknown approval request {id}"))?;
        let decided = self
            .record_routed_decision(coordinator, &request, approve)
            .await?;
        if let Some(decision) = decided {
            if decision.new {
                self.tell_requester(&decision.request).await;
            }
            return Ok(decision.request);
        }
        let routed = self.route_approval(&request, Some(coordinator)).await?;
        self.announce_approval(&request.workspace, &routed).await;
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
        let (coordinator, key, id) = (coordinator.to_owned(), request.key.clone(), request.id);
        let _guard = self.simulator_gate.lock().await;
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let stored = by_key(&tx, &key)?.context("the approval request no longer exists")?;
                ensure!(
                    stored.decider.as_deref() == Some(coordinator.as_str()),
                    "approval request {id} is not routed to this workspace"
                );
                if !holds_grant(&tx, &coordinator, &stored)? {
                    return Ok(None);
                }
                let decision = decide(&tx, &key, approve)?;
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
        let key = request.key;
        // Simulator allocation checks approval and mutates devices in separate
        // transactions under this gate; a decision must not land between them.
        let _guard = self.simulator_gate.lock().await;
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let decision = decide(&tx, &key, approve)?;
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
        if request.status != DecisionStatus::Pending {
            return;
        }
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
        let request = self
            .approval_request(id)
            .await?
            .with_context(|| format!("unknown approval request {id}"))?;
        ensure!(
            shown.is_none_or(|shown| request.is_same(&shown)),
            "approval request {id} changed since it was shown; choose again"
        );
        ensure!(
            request.decider.as_deref() == Some(workspace.id.as_str()),
            "approval request {id} is not {}'s to decide",
            workspace.name
        );
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
                        "{about}; decide with shoal approve {id} or shoal deny {id}, or pass it up with shoal escalate {id}",
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
        self.notify(
            Some(workspace),
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
mod tests {
    use super::*;

    fn resource(mode: LockMode) -> Specification {
        let definition = Definition {
            capacity: 1,
            reason: None,
            resources: std::collections::BTreeMap::from([(
                "lock".to_owned(),
                crate::daemon::resources::ResourceConfig::default(),
            )]),
        };
        Specification::Resource(ResourceSpecification {
            repository: None,
            definition,
            member: "lock".into(),
            mode,
        })
    }

    fn owners(db: &Connection) -> Result<()> {
        db.execute_batch(
            "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
            INSERT INTO workspaces(id,repository_id,name,path,branch,state) VALUES
                ('owner','repo','worker','/work','worker','ready'),
                ('other','repo','other','/other','other','ready');",
        )?;
        Ok(())
    }

    /// Store `request` under its key, keeping the number a stored request
    /// with that key holds or taking a free one.
    fn save(db: &Connection, request: &ApprovalRequest) -> Result<()> {
        let number = match db
            .query_row(
                "SELECT number FROM access_requests WHERE id=?1",
                [&request.key],
                |r| r.get(0),
            )
            .optional()?
        {
            Some(number) => number,
            None => store::free_number(db, "access_requests")?,
        };
        db.execute(
            "INSERT OR REPLACE INTO access_requests(id,workspace_id,target_key,name,record,active,number)
            VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                request.key,
                request.workspace_id,
                request.target,
                request.name,
                serde_json::to_string(request)?,
                request.active,
                number
            ],
        )?;
        Ok(())
    }

    /// The number stored under `key`.
    fn number(db: &Connection, key: &str) -> Result<i64> {
        Ok(db.query_row(
            "SELECT number FROM access_requests WHERE id=?1",
            [key],
            |r| r.get(0),
        )?)
    }

    fn sample(owner: &str, target: Target) -> ApprovalRequest {
        ApprovalRequest::new(
            owner,
            target,
            "default",
            resource(LockMode::Read),
            Lifetime::Workspace,
            Some("read shared data"),
        )
    }

    #[tokio::test]
    async fn current_lookup_isolates_names_and_ignores_unrelated_history() -> Result<()> {
        let (_root, manager) = crate::test_support::manager().await;
        manager.store.run(|db| {
            owners(db)?;
            let target = Target::Resource { scope: Scope::Global, pool: "lock".into() };
            for (id, owner, target) in [
                ("wanted", "owner", target.clone()),
                ("other-owner", "other", target.clone()),
                ("other-target", "owner", Target::Resource { scope: Scope::Global, pool: "other".into() }),
            ] {
                let mut request = sample(owner, target.clone());
                request.key = id.into();
                save(db, &request)?;
                assert_eq!(current(db, owner, &target, "default")?.unwrap().key, id);
            }
            db.execute("INSERT INTO access_requests(id,workspace_id,target_key,name,record,active,number) VALUES ('history','owner',?1,'default','invalid',0,100)", [&target])?;
            assert!(list(db, Some("owner")).is_err());
            assert_eq!(current(db, "owner", &target, "default")?.unwrap().key, "wanted");
            assert!(current(db, "owner", &target, "absent")?.is_none());
            let tx = db.transaction()?;
            let request = sample("owner", target.clone());
            assert_eq!(check(&tx, request.clone())?.unwrap().key, "wanted");
            let mut changed = request.clone();
            changed.lifetime = Lifetime::Lease;
            assert!(check(&tx, changed).is_err());
            let mut changed = request;
            changed.reason = "different reason".into();
            assert!(check(&tx, changed).is_err());
            tx.execute("UPDATE access_requests SET record='invalid' WHERE id='wanted'", [])?;
            assert!(current(&tx, "owner", &target, "default").is_err());
            Ok(())
        }).await
    }

    #[tokio::test]
    async fn decisions_decode_only_the_selected_id_and_remain_idempotent() -> Result<()> {
        let (_root, manager) = crate::test_support::manager().await;
        manager.store.run(|db| {
            owners(db)?;
            for key in ["approve", "deny"] {
                let mut request = sample("owner", Target::Simulator);
                request.key = key.into();
                request.name = key.into();
                save(db, &request)?;
            }
            db.execute("INSERT INTO access_requests(id,workspace_id,target_key,name,record,active,number) VALUES ('broken','other','simulator','default','invalid',0,3)", [])?;
            Ok(())
        }).await?;
        for (id, approve, status) in [
            (1, true, DecisionStatus::Approved),
            (2, false, DecisionStatus::Denied),
        ] {
            let decision = manager
                .decide_approval(id, approve, None, Default::default())
                .await?;
            assert_eq!(decision.status, status);
            let messages = manager.agent_messages("worker").await?;
            let ids: Vec<_> = messages.iter().map(|m| m.id).collect();
            assert_eq!(
                messages.into_iter().map(|m| m.message).collect::<Vec<_>>(),
                [decision.decision_message()]
            );
            manager.mark_agent_messages_delivered("worker", ids).await?;
            let retry = manager
                .decide_approval(id, approve, None, Default::default())
                .await?;
            assert_eq!(
                serde_json::to_value(retry)?,
                serde_json::to_value(&decision)?
            );
            // Repeating a decision does not tell the agent again.
            assert!(manager.agent_messages("worker").await?.is_empty());
            assert!(
                manager
                    .decide_approval(id, !approve, None, Default::default())
                    .await
                    .is_err()
            );
        }
        // A decision chosen from a shown request does not reach another
        // request holding its number.
        let shown = manager.approval_requests(Some("worker")).await?;
        let error = manager
            .decide_approval(1, true, Some(shown[1].clone()), Default::default())
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("changed since it was shown"),
            "{error:#}"
        );
        manager
            .decide_approval(1, true, Some(shown[0].clone()), Default::default())
            .await?;
        for (id, message) in [
            (4, "unknown approval request 4"),
            (3, "approval request 3 has an invalid record"),
        ] {
            assert!(
                manager
                    .decide_approval(id, true, None, Default::default())
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains(message)
            );
        }
        manager
            .store
            .run(|db| {
                db.execute("UPDATE workspaces SET state='failed' WHERE id='owner'", [])?;
                Ok(())
            })
            .await?;
        assert!(
            manager
                .decide_approval(1, true, None, Default::default())
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn replaced_land_requests_keep_their_numbers_and_refuse_decisions() -> Result<()> {
        let (_root, manager) = crate::test_support::manager().await;
        manager
            .store
            .run(|db| {
                owners(db)?;
                let tx = db.transaction()?;
                let mut requests = Vec::new();
                for head in ["first", "second", "third"] {
                    let request = ApprovalRequest::new(
                        "owner",
                        Target::Land,
                        LAND,
                        Specification::Land(LandSpecification {
                            branch: "feature".into(),
                            head: head.into(),
                        }),
                        Lifetime::Lease,
                        Some("land it"),
                    );
                    requests.push(request_land(&tx, request)?.unwrap());
                }
                let numbers: Vec<_> = requests.iter().map(|request| request.id).collect();
                assert_eq!(numbers, [1, 2, 3]);
                for outdated in &requests[..2] {
                    let error = decide(&tx, &outdated.key, true).err().unwrap();
                    assert!(error.to_string().contains("is outdated"), "{error:#}");
                    let stored = by_key(&tx, &outdated.key)?.unwrap();
                    assert_eq!(stored.status, DecisionStatus::Outdated);
                    assert!(!stored.active);
                }
                assert!(decide(&tx, &requests[2].key, true)?.new);
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn withdrawn_requests_keep_their_numbers_and_take_back_their_notifications() {
        let (root, manager) = crate::test_support::manager().await;
        let path = crate::test_support::repository(root.path(), "repo");
        let repo = manager
            .register_repository(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "asker".into(), None, None, None)
            .await
            .unwrap();
        let other = manager
            .create_workspace(&repo.id, "other".into(), None, None, None)
            .await
            .unwrap();
        let request = async || {
            let request = manager
                .request_approval(sample(&workspace.id, Target::Port("web".into())))
                .await
                .unwrap()
                .unwrap();
            manager.notify_approval(&workspace.name, &request).await;
            request
        };
        let unseen = request().await;
        let error = manager
            .withdraw_approval(&other.name, unseen.id)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("has no approval request"),
            "{error:#}"
        );

        let withdrawn = manager
            .withdraw_approval(&workspace.name, unseen.id)
            .await
            .unwrap();
        assert_eq!(withdrawn.status, DecisionStatus::Withdrawn);
        assert!(!withdrawn.active);
        assert!(manager.notifications(true, 50).await.unwrap().is_empty());
        let default = proposals::ProposalDecision::default;
        for error in [
            manager
                .decide_approval(unseen.id, true, None, default())
                .await
                .unwrap_err(),
            manager
                .withdraw_approval(&workspace.name, unseen.id)
                .await
                .unwrap_err(),
        ] {
            assert!(error.to_string().contains("is withdrawn"), "{error:#}");
        }

        // Requesting again asks anew under another number; once the user saw
        // that one, withdrawing it tells them.
        let shown = request().await;
        assert_ne!(shown.id, unseen.id);
        let notifications = manager.notifications(true, 50).await.unwrap();
        manager
            .mark_notifications_read(notifications.iter().map(|n| n.id).collect())
            .await
            .unwrap();
        manager
            .withdraw_approval(&workspace.name, shown.id)
            .await
            .unwrap();
        let unread = manager.notifications(true, 50).await.unwrap();
        assert_eq!(unread.len(), 1);
        assert_eq!(unread[0].kind, NotificationKind::ApprovalWithdrawn);
        assert_eq!(
            unread[0].message,
            format!(
                "approval {}: port/web / default (workspace) was withdrawn; no decision needed",
                shown.id
            )
        );

        // Workspace processes withdraw only their own requests.
        let caller = crate::daemon::scope::Caller {
            workspace_id: workspace.id.clone(),
            execution: None,
        };
        manager.issue_scope("scope".into(), caller).await;
        for (target, allowed) in [(&workspace.name, true), (&other.name, false)] {
            let mut method = crate::protocol::Method::WithdrawApproval {
                workspace: target.clone(),
                id: shown.id,
            };
            let authorized = crate::daemon::scope::authorize(&manager, Some("scope"), &mut method)
                .await
                .is_ok();
            assert_eq!(authorized, allowed);
        }
    }

    #[tokio::test]
    async fn withdrawal_never_reaches_a_request_that_took_the_number_meanwhile() -> Result<()> {
        let (_root, manager) = crate::test_support::manager().await;
        manager
            .store
            .run(|db| {
                owners(db)?;
                let tx = db.transaction()?;
                let selected = check(&tx, sample("owner", Target::Port("web".into())))?.unwrap();
                // Decided and gone while the withdrawal waited for its gate.
                release(&tx, "owner", &Target::Port("web".into()), "default")?;
                let later = check(&tx, sample("owner", Target::Port("api".into())))?.unwrap();
                assert_eq!(later.id, selected.id);
                let error = withdraw(&tx, &selected).unwrap_err();
                assert!(error.to_string().contains("decided meanwhile"), "{error:#}");
                let stored = by_key(&tx, &later.key)?.unwrap();
                assert_eq!(stored.status, DecisionStatus::Pending);
                Ok(())
            })
            .await
    }

    #[test]
    fn decision_messages_name_the_request_target_and_next_step() {
        let mut request = sample("owner", Target::Port("web".into()));
        request.id = 7;
        request.status = DecisionStatus::Approved;
        assert_eq!(
            request.decision_message(),
            "Approval 7 for port/web / default (workspace) was approved: retry the command that requested it."
        );
        request.status = DecisionStatus::Denied;
        assert_eq!(
            request.decision_message(),
            "Approval 7 for port/web / default (workspace) was denied: do not retry it."
        );
        request.target = Target::Land;
        request.specification = Specification::Land(LandSpecification {
            branch: "feature".into(),
            head: "abc".into(),
        });
        assert_eq!(
            request.decision_message(),
            "Approval 7 for land feature at abc was denied: do not retry it."
        );
        crate::validate::message(&request.decision_message()).unwrap();
    }

    #[tokio::test]
    async fn resource_release_targets_both_scopes_and_rolls_back_invalid_candidates() -> Result<()>
    {
        let (root, manager) = crate::test_support::manager().await;
        let path = root.path().to_owned();
        manager.store.run(move |db| {
            owners(db)?;
            db.execute("UPDATE workspaces SET path=?1 WHERE id='owner'", [path.to_str().unwrap()])?;
            for (id, scope, status) in [
                ("global", Scope::Global, DecisionStatus::Denied),
                ("repo", Scope::Repo("repo".into()), DecisionStatus::Approved),
            ] {
                let mut request = sample("owner", Target::Resource { scope, pool: "lock".into() });
                request.key = id.into();
                request.status = status;
                save(db, &request)?;
            }
            let Specification::Resource(spec) = resource(LockMode::Read) else { unreachable!() };
            db.execute("INSERT INTO resource_pools VALUES ('global','lock',?1)", [serde_json::to_string(&spec.definition)?])?;
            db.execute("INSERT INTO resource_leases(id,workspace_id,scope,pool,name,resource,created_at,mode)
                VALUES ('lease','owner','global','lock','default','lock',1,'read')", [])?;
            // Unrelated port history cannot block release, even without configured definitions.
            db.execute("INSERT INTO access_requests(id,workspace_id,target_key,name,record,active,number) VALUES ('port','owner','port/web','default','invalid',1,100)", [])?;
            db.execute("UPDATE access_requests SET record=json_set(record,'$.status','unknown') WHERE id='repo'", [])?;
            Ok(())
        }).await?;
        assert!(
            manager
                .release_resource("owner", "lock".into(), "default".into())
                .await
                .is_err()
        );
        manager.store.run(|db| {
            assert!(by_key(db, "global")?.is_some());
            assert_eq!(db.query_row("SELECT COUNT(*) FROM resource_leases", [], |r| r.get::<_, i64>(0))?, 1);
            db.execute("UPDATE access_requests SET record=json_set(record,'$.status','approved') WHERE id='repo'", [])?;
            Ok(())
        }).await?;
        manager
            .release_resource("owner", "lock".into(), "default".into())
            .await?;
        manager
            .store
            .run(|db| {
                assert!(by_key(db, "global")?.is_none());
                assert!(!by_key(db, "repo")?.unwrap().active);
                assert_eq!(
                    db.query_row("SELECT COUNT(*) FROM resource_leases", [], |r| r
                        .get::<_, i64>(0))?,
                    0
                );
                assert!(list(db, Some("owner")).is_err());
                Ok(())
            })
            .await
    }

    fn unrelated_history(db: &Connection, count: i64) -> Result<()> {
        db.execute(
            "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<?1)
            INSERT INTO access_requests(id,workspace_id,target_key,name,record,active,number)
            SELECT 'history-'||x, CASE WHEN x%2=0 THEN 'owner' ELSE 'other' END,
                CASE WHEN x%2=0 THEN 'port/other' ELSE 'resource/global/lock' END,
                'default', 'invalid', 0, 100000+x FROM n",
            [count],
        )?;
        Ok(())
    }

    #[tokio::test]
    async fn reuse_validates_all_candidates_but_not_unrelated_history() -> Result<()> {
        let (_root, manager) = crate::test_support::manager().await;
        manager
            .store
            .run(|db| {
                owners(db)?;
                unrelated_history(db, 10_000)?;
                let tx = db.transaction()?;
                let target = Target::Resource {
                    scope: Scope::Global,
                    pool: "lock".into(),
                };
                let request = sample("owner", target.clone());
                let mut grant = check(&tx, request.clone())?.unwrap();
                grant.status = DecisionStatus::Approved;
                save(&tx, &grant)?;
                assert!(release(&tx, "owner", &target, "default")?);
                assert!(!by_key(&tx, &grant.key)?.unwrap().active);
                assert!(check(&tx, request.clone())?.is_none());
                let mut another = request.clone();
                another.name = "another".into();
                assert!(check(&tx, another.clone())?.is_none());
                another.specification = resource(LockMode::Write);
                assert!(check(&tx, another)?.is_some());

                // Neither a different workspace nor target can authorize this request.
                for (owner, target) in [
                    ("other", target.clone()),
                    ("owner", Target::Port("other".into())),
                ] {
                    let mut unrelated = grant.clone();
                    unrelated.key = format!("unrelated-{owner}");
                    unrelated.workspace_id = owner.into();
                    unrelated.target = target;
                    save(&tx, &unrelated)?;
                }
                grant.status = DecisionStatus::Denied;
                grant.active = false;
                save(&tx, &grant)?;
                assert!(check(&tx, request.clone())?.is_some());
                assert!(release(&tx, "owner", &target, "default")?);
                grant.status = DecisionStatus::Approved;
                save(&tx, &grant)?;

                let mut broken = grant.clone();
                broken.key = "broken".into();
                // Put corruption after a reusable grant: a match must not short-circuit decoding.
                for (field, value) in [
                    ("status", serde_json::json!("unknown")),
                    ("lifetime", serde_json::json!("forever")),
                    ("target", serde_json::json!("resource/lock")),
                    ("specification", serde_json::json!({"member": "lock"})),
                ] {
                    save(&tx, &broken)?;
                    let mut record = serde_json::to_value(&broken)?;
                    record[field] = value;
                    tx.execute(
                        "UPDATE access_requests SET record=?1 WHERE id='broken'",
                        [record.to_string()],
                    )?;
                    let invalid = format!(
                        "approval request {} has an invalid record",
                        number(&tx, "broken")?
                    );
                    assert!(
                        check(&tx, request.clone())
                            .unwrap_err()
                            .to_string()
                            .contains(&invalid)
                    );
                    assert!(by_key(&tx, "broken").is_err());
                }
                assert!(list(&tx, None).is_err());
                assert!(list(&tx, Some("owner")).is_err());
                assert!(list(&tx, Some("missing"))?.is_empty());
                Ok(())
            })
            .await
    }

    fn plan(db: &Connection, sql: &str, params: impl Params) -> Result<String> {
        Ok(db
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?
            .query_map(params, |row| row.get::<_, String>(3))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .join("; "))
    }

    fn query_work(db: &Connection, sql: &str, params: impl Params) -> Result<(usize, i32)> {
        let mut statement = db.prepare(sql)?;
        let records = statement
            .query_map(params, record_row)?
            .map(|row| decode_record(row?))
            .collect::<Result<Vec<_>>>()?;
        assert_eq!(
            statement.get_status(rusqlite::StatementStatus::FullscanStep),
            0
        );
        Ok((
            records.len(),
            statement.get_status(rusqlite::StatementStatus::VmStep),
        ))
    }

    #[tokio::test]
    async fn bundled_planner_bounds_lookups_and_limits_candidate_decoding() -> Result<()> {
        let (_root, manager) = crate::test_support::manager().await;
        manager.store.run(|db| {
            owners(db)?;
            let target = Target::Resource { scope: Scope::Global, pool: "lock".into() };
            let mut request = sample("owner", target.clone());
            request.key = "wanted".into();
            save(db, &request)?;
            request.key = "released".into();
            request.active = false;
            request.status = DecisionStatus::Approved;
            save(db, &request)?;
            let id_work = query_work(db, BY_KEY, ["wanted"])?;
            let current_work = query_work(db, CURRENT, params!["owner", target, "default"])?;
            let candidate_work = query_work(db, CANDIDATES, params!["owner", target])?;
            assert_eq!((id_work.0, current_work.0, candidate_work.0), (1, 1, 2));
            unrelated_history(db, 10_000)?;
            assert_eq!(query_work(db, BY_KEY, ["wanted"])?, id_work);
            assert_eq!(query_work(db, CURRENT, params!["owner", target, "default"])?, current_work);
            assert_eq!(query_work(db, CANDIDATES, params!["owner", target])?, candidate_work);
            let id_plan = plan(db, BY_KEY, ["wanted"])?;
            let current_plan = plan(db, CURRENT, params!["owner", target, "default"])?;
            let candidate_plan = plan(db, CANDIDATES, params!["owner", target])?;
            assert!(id_plan.contains("SEARCH access_requests USING INDEX sqlite_autoindex_access_requests_1 (id=?)"), "{id_plan}");
            assert!(current_plan.contains("SEARCH access_requests USING INDEX access_request_name (workspace_id=? AND target_key=? AND name=?)"), "{current_plan}");
            assert!(candidate_plan.contains("SEARCH access_requests USING INDEX access_request_target (workspace_id=? AND target_key=?)"), "{candidate_plan}");
            db.execute_batch("SAVEPOINT without_index; DROP INDEX access_request_target;")?;
            let old_plan = plan(db, CANDIDATES, params!["owner", target])?;
            assert!(old_plan.contains("SCAN access_requests"), "{old_plan}");
            db.execute_batch("ROLLBACK TO without_index; RELEASE without_index;")?;
            eprintln!("SQLite {}: ID: {id_plan}; current: {current_plan}; candidates: {candidate_plan}; without target index: {old_plan}", rusqlite::version());
            eprintln!("Before/after 10,000 unrelated records, (decoded rows, VM steps): ID {id_work:?}, current {current_work:?}, candidates {candidate_work:?}; no full-scan steps");
            Ok(())
        }).await
    }

    #[test]
    fn targets_keep_their_stored_spelling() -> Result<()> {
        let db = Connection::open_in_memory()?;
        for (target, text) in [
            (
                Target::Resource {
                    scope: Scope::Global,
                    pool: "lock".into(),
                },
                "resource/global/lock",
            ),
            (
                Target::Resource {
                    scope: Scope::Repo("repo-id".into()),
                    pool: "lock".into(),
                },
                "resource/repo/repo-id/lock",
            ),
            (Target::Port("web".into()), "port/web"),
            (Target::Simulator, "simulator"),
            (Target::Secret("TOKEN".into()), "secret/TOKEN"),
            (Target::Land, "land"),
        ] {
            assert_eq!(target.to_string(), text);
            assert_eq!(text.parse::<Target>()?, target);
            assert_eq!(serde_json::to_value(&target)?, text);
            assert_eq!(serde_json::from_value::<Target>(text.into())?, target);
            let stored: Target = db.query_row("SELECT ?1", [&target], |row| row.get(0))?;
            assert_eq!(stored, target);
        }
        for text in [
            "",
            "port/",
            "resource/global",
            "resource/global/",
            "resource/local/lock",
            "resource//lock",
            "simulators",
            "secret/",
            "keep",
        ] {
            assert!(text.parse::<Target>().is_err(), "{text}");
        }
        Ok(())
    }

    #[test]
    fn specifications_round_trip_recorded_json_and_reject_mixed_shapes() -> Result<()> {
        let recorded = [
            serde_json::json!({"definition": {"capacity": 1, "reason": null,
                "resources": {"lock": {"requires_approval": true, "approval_lifetime": "lease",
                    "kind": "semaphore", "capacity": 1, "reason": null}}},
                "member": "lock", "mode": "permit"}),
            serde_json::json!({"preferred": null, "env": "PORT_WEB", "on_conflict": "suggest",
                "range": [3000, 3100]}),
            serde_json::json!({"profile": {"requires_approval": true, "approval_lifetime": "workspace",
                "device": "iPhone", "runtime": "iOS"}, "clean": false}),
            serde_json::json!({"command": ["op", "read", "op://Agents/key"], "tools": ["agent-browser"]}),
            serde_json::json!({"branch": "feature", "head": "abc"}),
        ];
        for json in recorded {
            let specification: Specification = serde_json::from_value(json.clone())?;
            assert_eq!(serde_json::to_value(&specification)?, json);
        }
        assert!(matches!(
            serde_json::from_value(serde_json::json!({"branch": "feature", "head": "abc"}))?,
            Specification::Land(_)
        ));
        assert!(matches!(
            serde_json::from_value(serde_json::json!({"preferred": 8080, "env": "PORT_WEB",
                "on_conflict": "auto", "range": [1, 2]}))?,
            Specification::Port(PortSpecification {
                preferred: Some(8080),
                ..
            })
        ));
        for json in [
            serde_json::json!({}),
            serde_json::json!({"member": "lock", "mode": "permit"}),
            serde_json::json!({"profile": {"device": "iPhone", "runtime": "iOS"}, "clean": false,
                "member": "lock"}),
            serde_json::json!({"preferred": null, "env": "PORT_WEB", "on_conflict": "maybe",
                "range": [3000, 3100]}),
            serde_json::json!({"command": ["op"], "tools": [], "head": "abc"}),
            serde_json::json!({"branch": "feature"}),
        ] {
            assert!(
                serde_json::from_value::<Specification>(json.clone()).is_err(),
                "{json}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn invalid_records_are_reported_instead_of_ignored() {
        let root = tempfile::tempdir().unwrap();
        let store = store::Store::open(root.path().join("state.db"))
            .await
            .unwrap();
        store.run(|db| {
            db.execute_batch("INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
                INSERT INTO workspaces(id,repository_id,name,path,branch,state) VALUES ('owner','repo','worker','/work','worker','ready');")?;
            let request = ApprovalRequest::new("owner", Target::Simulator, "default",
                Specification::Simulator(SimulatorSpecification { clean: false, profile: Profile {
                    requires_approval: true, approval_lifetime: Lifetime::Lease,
                    device: "iPhone".into(), runtime: "iOS".into() } }), Lifetime::Lease, Some("test app"));
            let mut record = serde_json::to_value(&request)?;
            record["id"] = 1.into();
            let insert = |db: &Connection, key: &str, number: i64, record: &serde_json::Value| db.execute(
                "INSERT INTO access_requests(id,workspace_id,target_key,name,record,number) VALUES (?1,'owner','simulator',?1,?2,?3)",
                params![key, record.to_string(), number]);
            insert(db, "valid", 1, &record)?;
            assert_eq!(list(db, Some("owner"))?.len(), 1);
            let mut broken = record.clone();
            broken["specification"] = serde_json::json!({"member": "lock"});
            broken["id"] = 2.into();
            insert(db, "broken", 2, &broken)?;
            let error = list(db, Some("owner")).unwrap_err().to_string();
            assert!(error.contains("approval request 2 has an invalid record"), "{error}");
            db.execute("DELETE FROM access_requests WHERE id='broken'", [])?;
            broken["specification"] = record["specification"].clone();
            broken["target"] = "resource/lock".into();
            insert(db, "broken", 2, &broken)?;
            assert!(list(db, Some("owner")).is_err());
            Ok(())
        }).await.unwrap();
    }

    #[tokio::test]
    async fn decisions_bind_settings_and_release_obeys_configured_lifetime() {
        let root = tempfile::tempdir().unwrap();
        let store = store::Store::open(root.path().join("state.db"))
            .await
            .unwrap();
        store.run(|db| {
            db.execute_batch("INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
                INSERT INTO workspaces(id,repository_id,name,path,branch,state) VALUES ('owner','repo','worker','/work','worker','ready');")?;
            for lifetime in [Lifetime::Lease, Lifetime::Workspace] {
                assert_eq!(serde_json::to_value(lifetime)?, lifetime.to_string());
                let tx = db.transaction()?;
                let target = Target::Resource { scope: Scope::Global, pool: "lock".into() };
                let request = ApprovalRequest::new("owner", target, "default",
                    resource(LockMode::Read), lifetime, Some("read shared data"));
                let pending = check(&tx, request.clone())?.unwrap();
                assert_eq!(check(&tx, request.clone())?.unwrap().id, pending.id);
                assert_eq!(list(&tx, Some("owner"))?.len(), 1);
                let mut changed = request.clone();
                changed.specification = resource(LockMode::Write);
                assert!(check(&tx, changed).is_err());
                let mut decision = pending.clone();
                decision.status = DecisionStatus::Denied;
                tx.execute("UPDATE access_requests SET record=?2 WHERE id=?1", params![decision.key,serde_json::to_string(&decision)?])?;
                assert_eq!(check(&tx, request.clone())?.unwrap().status, DecisionStatus::Denied);
                decision.status = DecisionStatus::Approved;
                tx.execute("UPDATE access_requests SET record=?2 WHERE id=?1", params![decision.key,serde_json::to_string(&decision)?])?;
                assert!(check(&tx, request.clone())?.is_none());
                assert!(release(&tx, "owner", &request.target, "default")?);
                let result = check(&tx, request.clone())?;
                assert_eq!(result.is_some(), lifetime == Lifetime::Lease);
                if lifetime == Lifetime::Workspace {
                    let mut another = request.clone();
                    another.name = "another".into();
                    assert!(check(&tx, another.clone())?.is_none());
                    another.specification = resource(LockMode::Write);
                    assert!(check(&tx, another)?.is_some());
                }
                tx.rollback()?;
            }
            assert!(serde_json::from_str::<DecisionStatus>("\"unknown\"").is_err());
            assert!(serde_json::from_str::<Lifetime>("\"forever\"").is_err());
            Ok(())
        }).await.unwrap();
    }
}
