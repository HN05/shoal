//! Human decisions authorize cooperative allocations; they never reserve capacity.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Params, Transaction, params};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

use crate::{
    config::repo::ConflictPolicy,
    daemon::{
        log,
        resources::{Definition, LockMode, Scope},
        store,
        workspace::Manager,
    },
    sim::Profile,
    state::{states, text_key},
};

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
});

/// What a decision authorizes; one active request per workspace, target and
/// name. Keyed as `resource/<scope>/<pool>`, `port/<name>`, `simulator`,
/// `secret/<name>`, `keep` or `land`.
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
    /// Keeping a workspace after an agent marks its assignment done.
    Keep,
    /// Landing a workspace branch into the default branch.
    Land,
}

/// The name of a workspace's single keep request.
pub const KEEP: &str = "done";
/// The name of a workspace's single land request.
pub const LAND: &str = "branch";

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resource { scope, pool } => write!(f, "resource/{scope}/{pool}"),
            Self::Port(name) => write!(f, "port/{name}"),
            Self::Simulator => f.write_str("simulator"),
            Self::Secret(name) => write!(f, "secret/{name}"),
            Self::Keep => f.write_str("keep"),
            Self::Land => f.write_str("land"),
        }
    }
}

impl FromStr for Target {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        match text {
            "simulator" => return Ok(Self::Simulator),
            "keep" => return Ok(Self::Keep),
            "land" => return Ok(Self::Land),
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

/// The effective allocation settings a decision binds; a later request with
/// different settings cannot reuse the grant. Each kind keeps the JSON shape
/// its allocator has always recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Specification {
    Resource(ResourceSpecification),
    Port(PortSpecification),
    Simulator(SimulatorSpecification),
    Keep(KeepSpecification),
    Secret(SecretSpecification),
    Land(LandSpecification),
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

/// A keep decision covers the completed HEAD; later commits need a new one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeepSpecification {
    pub head: String,
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

pub fn list(db: &Connection, owner: Option<&str>) -> Result<Vec<ApprovalRequest>> {
    db.prepare(&format!(
        "{COLUMNS} WHERE ?1 IS NULL OR workspace_id=?1 ORDER BY rowid"
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
            target,
            name: name.into(),
            specification,
            lifetime,
            reason: reason.unwrap_or_default().into(),
            status: DecisionStatus::Pending,
            created_at: crate::time::unix_seconds(),
            decided_at: None,
            active: true,
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

    /// What the user is asked to decide: the target and lease name, `keep`,
    /// or the branch and commit to land.
    pub fn subject(&self) -> String {
        match (&self.target, &self.specification) {
            (Target::Keep, _) => self.target.to_string(),
            (Target::Land, Specification::Land(land)) => {
                format!("land {} at {}", land.branch, land.head)
            }
            _ => format!("{} / {} ({})", self.target, self.name, self.lifetime),
        }
    }

    /// The agent message naming a decision and what the requesting agent does next.
    fn decision_message(&self) -> String {
        let next = match (&self.target, &self.status) {
            (Target::Keep, DecisionStatus::Approved) => "Shoal keeps this workspace",
            (Target::Keep, _) => "Shoal will clean up this workspace",
            (_, DecisionStatus::Approved) => "retry the command that requested it",
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

/// Keep a request for an earlier commit as outdated until its workspace is
/// removed: its number is never reused for a newer commit's request, and
/// deciding it is refused.
pub fn retire(tx: &Transaction<'_>, request: &ApprovalRequest) -> Result<()> {
    let mut request = request.clone();
    request.status = DecisionStatus::Outdated;
    tx.execute(
        "UPDATE access_requests SET record=?2,active=0 WHERE id=?1",
        params![request.key, serde_json::to_string(&request)?],
    )?;
    Ok(())
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

/// Replace the workspace's keep request with `request`, or cancel it when the
/// completion needs none. Returns the request still awaiting a decision; an
/// approval for the same HEAD is reused and a denial refused.
pub fn request_keep(
    tx: &Transaction<'_>,
    owner: &str,
    request: Option<ApprovalRequest>,
) -> Result<Option<ApprovalRequest>> {
    let existing = current(tx, owner, &Target::Keep, KEEP)?;
    let Some(mut request) = request else {
        release(tx, owner, &Target::Keep, KEEP)?;
        return Ok(None);
    };
    match existing {
        // A repeated request keeps the reason the user is deciding on.
        Some(existing) if existing.specification == request.specification => {
            request.reason = existing.reason;
        }
        Some(existing) => retire(tx, &existing)?,
        None => {}
    }
    let pending = check(tx, request)?;
    ensure!(
        pending
            .as_ref()
            .is_none_or(|request| request.status != DecisionStatus::Denied),
        "the user denied keeping this workspace; run shoal done without --keep"
    );
    Ok(pending)
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
        request.status != DecisionStatus::Outdated,
        "approval request {} is outdated: its commit is no longer current",
        request.id
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
    /// reusing its number is refused.
    pub async fn decide_approval(
        &self,
        id: i64,
        approve: bool,
        shown: Option<ApprovalRequest>,
    ) -> Result<ApprovalRequest> {
        let request = self
            .approval_request(id)
            .await?
            .with_context(|| format!("unknown approval request {id}"))?;
        ensure!(
            shown.is_none_or(|shown| request.is_same(&shown)),
            "approval request {id} changed since it was shown; choose again"
        );
        let decision = self.record_decision(request, approve).await?;
        if decision.new {
            self.tell_requester(&decision.request).await;
        }
        Ok(decision.request)
    }

    /// Decide the stored `request` the user chose; later requests reusing its
    /// number are not affected.
    async fn record_decision(&self, request: ApprovalRequest, approve: bool) -> Result<Decision> {
        if request.target == Target::Keep {
            return self.decide_keep(request, approve).await;
        }
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
        if let Err(error) = self
            .queue_agent_message(&request.workspace_id, request.decision_message())
            .await
        {
            log!("approval decision message not queued: {error:#}");
        }
    }

    /// A request's target never changes, so it can be read before taking the
    /// gate its decision needs.
    async fn approval_request(&self, id: i64) -> Result<Option<ApprovalRequest>> {
        self.store.run(move |db| by_number(db, id)).await
    }

    pub async fn notify_approval(&self, workspace: &str, request: &ApprovalRequest) {
        if request.status == DecisionStatus::Pending {
            self.notify(
                Some(workspace),
                crate::daemon::notifications::NotificationKind::ApprovalRequested,
                format!(
                    "approval {}: {}: {}; approve with shoal approve {}",
                    request.id,
                    request.subject(),
                    request.reason,
                    request.id
                ),
            )
            .await;
        }
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
            db.execute("INSERT INTO access_requests VALUES ('history','owner',?1,'default','invalid',0,100)", [&target])?;
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
            db.execute("INSERT INTO access_requests VALUES ('broken','other','simulator','default','invalid',0,3)", [])?;
            Ok(())
        }).await?;
        for (id, approve, status) in [
            (1, true, DecisionStatus::Approved),
            (2, false, DecisionStatus::Denied),
        ] {
            let decision = manager.decide_approval(id, approve, None).await?;
            assert_eq!(decision.status, status);
            let messages = manager.agent_messages("worker").await?;
            let ids: Vec<_> = messages.iter().map(|m| m.id).collect();
            assert_eq!(
                messages.into_iter().map(|m| m.message).collect::<Vec<_>>(),
                [decision.decision_message()]
            );
            manager.mark_agent_messages_delivered("worker", ids).await?;
            let retry = manager.decide_approval(id, approve, None).await?;
            assert_eq!(
                serde_json::to_value(retry)?,
                serde_json::to_value(&decision)?
            );
            // Repeating a decision does not tell the agent again.
            assert!(manager.agent_messages("worker").await?.is_empty());
            assert!(manager.decide_approval(id, !approve, None).await.is_err());
        }
        // A decision chosen from a shown request does not reach another
        // request holding its number.
        let shown = manager.approval_requests(Some("worker")).await?;
        let error = manager
            .decide_approval(1, true, Some(shown[1].clone()))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("changed since it was shown"),
            "{error:#}"
        );
        manager
            .decide_approval(1, true, Some(shown[0].clone()))
            .await?;
        for (id, message) in [
            (4, "unknown approval request 4"),
            (3, "approval request 3 has an invalid record"),
        ] {
            assert!(
                manager
                    .decide_approval(id, true, None)
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
        assert!(manager.decide_approval(1, true, None).await.is_err());
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
        request.target = Target::Keep;
        assert_eq!(
            request.decision_message(),
            "Approval 7 for keep was denied: Shoal will clean up this workspace."
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
            db.execute("INSERT INTO access_requests VALUES ('port','owner','port/web','default','invalid',1,100)", [])?;
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
            INSERT INTO access_requests
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
            (Target::Keep, "keep"),
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
            serde_json::json!({"head": "abc"}),
            serde_json::json!({"branch": "feature", "head": "abc"}),
        ];
        for json in recorded {
            let specification: Specification = serde_json::from_value(json.clone())?;
            assert_eq!(serde_json::to_value(&specification)?, json);
        }
        assert!(matches!(
            serde_json::from_value(serde_json::json!({"head": "abc"}))?,
            Specification::Keep(_)
        ));
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
