//! Human decisions authorize cooperative allocations; they never reserve capacity.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, Transaction, params};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

use crate::{
    config::repo::ConflictPolicy,
    daemon::{
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
});

/// What a decision authorizes; one active request per workspace, target and
/// name. Keyed as `resource/<scope>/<pool>`, `port/<name>` or `simulator`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Resource { scope: Scope, pool: String },
    Port(String),
    Simulator,
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resource { scope, pool } => write!(f, "resource/{scope}/{pool}"),
            Self::Port(name) => write!(f, "port/{name}"),
            Self::Simulator => f.write_str("simulator"),
        }
    }
}

impl FromStr for Target {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        if text == "simulator" {
            return Ok(Self::Simulator);
        }
        if let Some(name) = text.strip_prefix("port/")
            && !name.is_empty()
        {
            return Ok(Self::Port(name.into()));
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
        bail!("invalid access target: {text}")
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSpecification {
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

impl fmt::Display for Specification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&serde_json::to_string(self).map_err(|_| fmt::Error)?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessRequest {
    pub id: String,
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

pub fn list(db: &Connection, owner: Option<&str>) -> Result<Vec<AccessRequest>> {
    let records = db.prepare("SELECT id,record,active FROM access_requests WHERE ?1 IS NULL OR workspace_id=?1 ORDER BY rowid")?
        .query_map([owner], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, bool>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    records
        .into_iter()
        .map(|(id, record, active)| {
            let mut request: AccessRequest = serde_json::from_str(&record)
                .with_context(|| format!("access request {id} has an invalid record"))?;
            request.active = active;
            Ok(request)
        })
        .collect()
}

pub fn current(
    db: &Connection,
    owner: &str,
    target: &Target,
    name: &str,
) -> Result<Option<AccessRequest>> {
    Ok(list(db, Some(owner))?
        .into_iter()
        .find(|r| r.active && r.target == *target && r.name == name))
}

/// The caller holds the allocation transaction (or simulator gate). The exact
/// specification includes effective policy, so a changed policy cannot reuse a grant.
pub fn check(tx: &Transaction<'_>, mut request: AccessRequest) -> Result<Option<AccessRequest>> {
    store::require_ready(tx, &request.workspace_id)?;
    let records = list(tx, Some(&request.workspace_id))?;
    if let Some(existing) = records
        .iter()
        .find(|r| r.active && r.target == request.target && r.name == request.name)
    {
        ensure!(
            existing.specification == request.specification
                && existing.lifetime == request.lifetime,
            "access request settings changed; release the resource name before retrying"
        );
        ensure!(
            request.reason.is_empty() || request.reason == existing.reason,
            "access request reason changed; release the resource name before retrying"
        );
        return Ok((existing.status != DecisionStatus::Approved).then(|| existing.clone()));
    }
    if records.iter().any(|r| {
        r.status == DecisionStatus::Approved
            && r.lifetime == Lifetime::Workspace
            && r.target == request.target
            && r.specification == request.specification
            && r.lifetime == request.lifetime
    }) {
        return Ok(None);
    }
    ensure!(
        !request.reason.trim().is_empty(),
        "approval requires --reason explaining the requested access"
    );
    crate::validate::reason("access", Some(&request.reason))?;
    request.workspace = tx.query_row(
        "SELECT name FROM workspaces WHERE id=?1",
        [&request.workspace_id],
        |r| r.get(0),
    )?;
    request.id = uuid::Uuid::new_v4().to_string();
    tx.execute("INSERT INTO access_requests(id,workspace_id,target_key,name,record) VALUES (?1,?2,?3,?4,?5)",
        params![request.id,request.workspace_id,request.target,request.name,serde_json::to_string(&request)?])?;
    Ok(Some(request))
}

impl AccessRequest {
    pub fn new(
        owner: &str,
        target: Target,
        name: &str,
        specification: Specification,
        lifetime: Lifetime,
        reason: Option<&str>,
    ) -> Self {
        Self {
            id: String::new(),
            workspace_id: owner.into(),
            workspace: String::new(),
            target,
            name: name.into(),
            specification,
            lifetime,
            reason: reason.unwrap_or_default().into(),
            status: DecisionStatus::Pending,
            created_at: crate::sim::now(),
            decided_at: None,
            active: true,
        }
    }
}

/// Cancel requests or end lease grants; retain approved workspace grants.
pub fn release(tx: &Transaction<'_>, owner: &str, target: &Target, name: &str) -> Result<bool> {
    let Some(request) = current(tx, owner, target, name)? else {
        return Ok(false);
    };
    if request.status == DecisionStatus::Approved && request.lifetime == Lifetime::Workspace {
        tx.execute(
            "UPDATE access_requests SET active=0 WHERE id=?1",
            [&request.id],
        )?;
    } else {
        tx.execute("DELETE FROM access_requests WHERE id=?1", [&request.id])?;
    }
    Ok(true)
}

impl Manager {
    pub async fn access_requests(&self, selector: Option<&str>) -> Result<Vec<AccessRequest>> {
        let owner = self.workspace_filter(selector).await?;
        self.store.run(move |db| list(db, owner.as_deref())).await
    }

    pub async fn decide_access(&self, id: String, approve: bool) -> Result<AccessRequest> {
        // Simulator allocation checks approval and mutates devices in separate
        // transactions under this gate; a decision must not land between them.
        let _guard = self.simulator_gate.lock().await;
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let mut request = list(&tx, None)?
                    .into_iter()
                    .find(|r| r.id == id)
                    .ok_or_else(|| anyhow::anyhow!("unknown access request"))?;
                store::require_ready(&tx, &request.workspace_id)?;
                let status = if approve {
                    DecisionStatus::Approved
                } else {
                    DecisionStatus::Denied
                };
                ensure!(
                    request.status == DecisionStatus::Pending || request.status == status,
                    "access request already decided"
                );
                if request.status == DecisionStatus::Pending {
                    request.status = status;
                    request.decided_at = Some(crate::sim::now());
                    tx.execute(
                        "UPDATE access_requests SET record=?2 WHERE id=?1",
                        params![id, serde_json::to_string(&request)?],
                    )?;
                }
                tx.commit()?;
                Ok(request)
            })
            .await
    }

    pub async fn notify_access(&self, workspace: &str, request: &AccessRequest) {
        if request.status == DecisionStatus::Pending {
            self.notify(
                Some(workspace),
                crate::daemon::notifications::NotificationKind::AccessRequested,
                format!(
                    "access {}: {} / {}: {}; approve with shoal access approve {}",
                    request.id, request.target, request.name, request.reason, request.id
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
            definition,
            member: "lock".into(),
            mode,
        })
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
        ];
        for json in recorded {
            let specification: Specification = serde_json::from_value(json.clone())?;
            assert_eq!(serde_json::to_value(&specification)?, json);
        }
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
            let request = AccessRequest::new("owner", Target::Simulator, "default",
                Specification::Simulator(SimulatorSpecification { clean: false, profile: Profile {
                    requires_approval: true, approval_lifetime: Lifetime::Lease,
                    device: "iPhone".into(), runtime: "iOS".into() } }), Lifetime::Lease, Some("test app"));
            let mut record = serde_json::to_value(&request)?;
            record["id"] = "valid".into();
            let insert = |db: &Connection, id: &str, record: &serde_json::Value| db.execute(
                "INSERT INTO access_requests(id,workspace_id,target_key,name,record) VALUES (?1,'owner','simulator',?1,?2)",
                params![id, record.to_string()]);
            insert(db, "valid", &record)?;
            assert_eq!(list(db, Some("owner"))?.len(), 1);
            let mut broken = record.clone();
            broken["specification"] = serde_json::json!({"member": "lock"});
            broken["id"] = "broken".into();
            insert(db, "broken", &broken)?;
            let error = list(db, Some("owner")).unwrap_err().to_string();
            assert!(error.contains("access request broken has an invalid record"), "{error}");
            db.execute("DELETE FROM access_requests WHERE id='broken'", [])?;
            broken["specification"] = record["specification"].clone();
            broken["target"] = "resource/lock".into();
            insert(db, "broken", &broken)?;
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
                let request = AccessRequest::new("owner", target, "default",
                    resource(LockMode::Read), lifetime, Some("read shared data"));
                let pending = check(&tx, request.clone())?.unwrap();
                assert_eq!(check(&tx, request.clone())?.unwrap().id, pending.id);
                assert_eq!(list(&tx, Some("owner"))?.len(), 1);
                let mut changed = request.clone();
                changed.specification = resource(LockMode::Write);
                assert!(check(&tx, changed).is_err());
                let mut decision = pending.clone();
                decision.status = DecisionStatus::Denied;
                tx.execute("UPDATE access_requests SET record=?2 WHERE id=?1", params![decision.id,serde_json::to_string(&decision)?])?;
                assert_eq!(check(&tx, request.clone())?.unwrap().status, DecisionStatus::Denied);
                decision.status = DecisionStatus::Approved;
                tx.execute("UPDATE access_requests SET record=?2 WHERE id=?1", params![decision.id,serde_json::to_string(&decision)?])?;
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
