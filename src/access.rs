//! Human decisions authorize cooperative allocations; they never reserve capacity.
use anyhow::{Result, ensure};
use rusqlite::{Connection, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{state::states, store, workspace::Manager};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessRequest {
    pub id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub workspace: String,
    pub target: String,
    pub name: String,
    pub specification: Value,
    pub lifetime: Lifetime,
    pub reason: String,
    pub status: DecisionStatus,
    pub created_at: u64,
    pub decided_at: Option<u64>,
    /// False for workspace grants retained after release.
    pub active: bool,
}

pub fn list(db: &Connection, owner: Option<&str>) -> Result<Vec<AccessRequest>> {
    let records = db.prepare("SELECT record,active FROM access_requests WHERE ?1 IS NULL OR workspace_id=?1 ORDER BY rowid")?
        .query_map([owner], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    records
        .into_iter()
        .map(|(record, active)| {
            let mut request: AccessRequest = serde_json::from_str(&record)?;
            request.active = active;
            Ok(request)
        })
        .collect()
}

pub fn current(
    db: &Connection,
    owner: &str,
    target: &str,
    name: &str,
) -> Result<Option<AccessRequest>> {
    Ok(list(db, Some(owner))?
        .into_iter()
        .find(|r| r.active && r.target == target && r.name == name))
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
        target: String,
        name: &str,
        specification: Value,
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
pub fn release(tx: &Transaction<'_>, owner: &str, target: &str, name: &str) -> Result<bool> {
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
                crate::notifications::NotificationKind::AccessRequested,
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
                let request = AccessRequest::new("owner", "resource/global/lock".into(), "default",
                    serde_json::json!({"mode":"read"}), lifetime, Some("read shared data"));
                let pending = check(&tx, request.clone())?.unwrap();
                assert_eq!(check(&tx, request.clone())?.unwrap().id, pending.id);
                assert_eq!(list(&tx, Some("owner"))?.len(), 1);
                let mut changed = request.clone();
                changed.specification["mode"] = serde_json::json!("write");
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
                    another.specification["mode"] = serde_json::json!("write");
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
