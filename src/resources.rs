//! Cooperative semaphores and reader/writer locks; only Shoal bookkeeping is enforced.
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

use crate::{repo_config, workspace::Manager};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    #[default]
    Semaphore,
    Rwlock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum LockMode {
    Permit,
    Read,
    Write,
}
impl LockMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Permit => "permit",
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}
impl std::fmt::Display for LockMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
impl rusqlite::types::FromSql for LockMode {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        match value.as_str()? {
            "permit" => Ok(Self::Permit),
            "read" => Ok(Self::Read),
            "write" => Ok(Self::Write),
            other => Err(rusqlite::types::FromSqlError::Other(Box::new(
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid resource lease mode: {other}"),
                ),
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourceConfig {
    pub kind: ResourceKind,
    pub capacity: u32,
    pub reason: Option<String>,
}
impl Default for ResourceConfig {
    fn default() -> Self {
        Self {
            kind: ResourceKind::Semaphore,
            capacity: 1,
            reason: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PoolConfig {
    pub capacity: Option<u32>,
    pub reason: Option<String>,
    pub resources: BTreeMap<String, ResourceConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Definition {
    pub capacity: u32,
    pub reason: Option<String>,
    pub resources: BTreeMap<String, ResourceConfig>,
}

pub fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 64
            && name.as_bytes()[0].is_ascii_lowercase()
            && name
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-'),
        "resource names must start with a lowercase letter and contain only lowercase letters, digits, _ or - (max 64)"
    );
    Ok(())
}
fn validate_reason(reason: Option<&str>) -> Result<()> {
    ensure!(
        reason.is_none_or(|s| !s.trim().is_empty() && s.len() <= 256 && !s.contains(['\n', '\r'])),
        "resource reason must be a nonempty single line (max 256 bytes)"
    );
    Ok(())
}

pub fn definitions(
    resources: &BTreeMap<String, ResourceConfig>,
    pools: &BTreeMap<String, PoolConfig>,
) -> Result<BTreeMap<String, Definition>> {
    let mut result = BTreeMap::new();
    for (name, resource) in resources {
        ensure!(
            !pools.contains_key(name),
            "resource and pool share the name {name}"
        );
        result.insert(
            name.clone(),
            Definition {
                capacity: resource.capacity,
                reason: resource.reason.clone(),
                resources: BTreeMap::from([(name.clone(), resource.clone())]),
            },
        );
    }
    for (name, pool) in pools {
        let total: u64 = pool.resources.values().map(|r| u64::from(r.capacity)).sum();
        let capacity = pool.capacity.map(u64::from).unwrap_or(total);
        ensure!(
            (1..=65535).contains(&capacity),
            "pool {name} capacity must be between 1 and 65535"
        );
        result.insert(
            name.clone(),
            Definition {
                capacity: capacity as u32,
                reason: pool.reason.clone(),
                resources: pool.resources.clone(),
            },
        );
    }
    for (name, definition) in &result {
        validate_name(name)?;
        validate_reason(definition.reason.as_deref())?;
        ensure!(
            !definition.resources.is_empty(),
            "resource pool {name} must contain named resources"
        );
        ensure!(
            (1..=65535).contains(&definition.capacity),
            "capacity must be between 1 and 65535"
        );
        for (name, resource) in &definition.resources {
            validate_name(name)?;
            validate_reason(resource.reason.as_deref())?;
            ensure!(
                resource.kind != ResourceKind::Rwlock || resource.capacity == 1,
                "rwlock resource {name} must use capacity 1; readers share its one slot"
            );
            ensure!(
                (1..=65535).contains(&resource.capacity),
                "resource {name} capacity must be between 1 and 65535"
            );
        }
    }
    Ok(result)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ResourceLease {
    pub mode: LockMode,
    pub id: String,
    pub workspace_id: String,
    pub scope: String,
    pub pool: String,
    pub name: String,
    pub resource: String,
    pub reason: Option<String>,
    pub created_at: i64,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AcquireRequest {
    #[serde(default)]
    pub mode: Option<LockMode>,
    pub pool: String,
    pub name: String,
    pub resource: Option<String>,
    pub reason: Option<String>,
}
pub enum Acquisition {
    Acquired(ResourceLease),
    Busy(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResourceStatus {
    pub kind: ResourceKind,
    pub readers: u32,
    pub writers: u32,
    pub read_available: bool,
    pub write_available: bool,
    pub name: String,
    pub capacity: u32,
    pub used: u32,
    pub available: u32,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct PoolStatus {
    pub name: String,
    pub scope: String,
    pub capacity: u32,
    pub used: u32,
    pub available: u32,
    pub configuration_matches: bool,
    pub resources: Vec<ResourceStatus>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Overview {
    pub pools: Vec<PoolStatus>,
    pub leases: Vec<ResourceLease>,
}

fn row_lease(row: &rusqlite::Row<'_>) -> rusqlite::Result<ResourceLease> {
    Ok(ResourceLease {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        scope: row.get(2)?,
        pool: row.get(3)?,
        name: row.get(4)?,
        resource: row.get(5)?,
        reason: row.get(6)?,
        created_at: row.get(7)?,
        mode: row.get(8)?,
    })
}
pub fn leases(db: &Connection, owner: Option<&str>) -> Result<Vec<ResourceLease>> {
    Ok(db.prepare("SELECT id,workspace_id,scope,pool,name,resource,reason,created_at,mode FROM resource_leases WHERE ?1 IS NULL OR workspace_id=?1 ORDER BY scope,pool,resource,name,id")?
        .query_map([owner], row_lease)?.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[derive(Default, Clone, Copy)]
struct Usage {
    permits: u32,
    readers: u32,
    writers: u32,
}
impl Usage {
    fn slots(self) -> u32 {
        self.permits + u32::from(self.readers > 0 || self.writers > 0)
    }
}
fn usage(leases: &[&ResourceLease], resource: &str) -> Usage {
    let mut used = Usage::default();
    for lease in leases.iter().filter(|l| l.resource == resource) {
        match lease.mode {
            LockMode::Permit => used.permits += 1,
            LockMode::Read => used.readers += 1,
            LockMode::Write => used.writers += 1,
        }
    }
    used
}
fn pool_used(leases: &[&ResourceLease]) -> u32 {
    let permits = leases.iter().filter(|l| l.mode == LockMode::Permit).count();
    let locks: BTreeSet<_> = leases
        .iter()
        .filter(|l| l.mode != LockMode::Permit)
        .map(|l| &l.resource)
        .collect();
    (permits + locks.len()) as u32
}
impl ResourceConfig {
    fn mode(&self, requested: Option<LockMode>) -> Option<LockMode> {
        match (self.kind, requested) {
            (ResourceKind::Semaphore, None | Some(LockMode::Permit)) => Some(LockMode::Permit),
            (ResourceKind::Rwlock, None | Some(LockMode::Write)) => Some(LockMode::Write),
            (ResourceKind::Rwlock, Some(LockMode::Read)) => Some(LockMode::Read),
            _ => None,
        }
    }
    fn can_acquire(&self, mode: LockMode, used: Usage, pool_available: u32) -> bool {
        match (self.kind, mode) {
            (ResourceKind::Semaphore, LockMode::Permit) => {
                used.slots() < self.capacity && pool_available > 0
            }
            (ResourceKind::Rwlock, LockMode::Read) => {
                used.writers == 0 && (used.readers > 0 || pool_available > 0)
            }
            (ResourceKind::Rwlock, LockMode::Write) => used.slots() == 0 && pool_available > 0,
            _ => false,
        }
    }
}

impl Manager {
    fn resource_definitions(
        &self,
        workspace: &crate::model::Workspace,
    ) -> Result<BTreeMap<String, (String, Definition)>> {
        let repo = repo_config::load(&workspace.path)?;
        let global = definitions(&self.config.resources, &self.config.resource_pools)?;
        let local = definitions(&repo.resources, &repo.resource_pools)?;
        let mut result: BTreeMap<_, _> = global
            .into_iter()
            .map(|(name, definition)| (name, ("global".into(), definition)))
            .collect();
        for (name, definition) in local {
            if let Some((_, machine)) = result.get(&name) {
                ensure!(
                    machine == &definition,
                    "repo definition for {name} conflicts with the global resource; rename it or use the global definition"
                );
            } else {
                result.insert(
                    name,
                    (format!("repo/{}", workspace.repository_id), definition),
                );
            }
        }
        Ok(result)
    }

    pub async fn acquire_resource(
        &self,
        selector: String,
        request: AcquireRequest,
    ) -> Result<Acquisition> {
        validate_name(&request.pool)?;
        validate_name(&request.name)?;
        if let Some(resource) = &request.resource {
            validate_name(resource)?;
        }
        validate_reason(request.reason.as_deref())?;
        let workspace = self.get(selector).await?;
        self.touch(&workspace.id).await;
        let mut definitions = self.resource_definitions(&workspace)?;
        let (scope, definition) = definitions
            .remove(&request.pool)
            .ok_or_else(|| anyhow::anyhow!("unknown resource or pool: {}", request.pool))?;
        self.store.run(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let ready: bool = tx.query_row("SELECT state='ready' FROM workspaces WHERE id=?1", [&workspace.id], |r| r.get(0))?;
            ensure!(ready, "workspace is not ready");
            let all = leases(&tx, None)?;
            if let Some(existing) = all.iter().find(|l| l.workspace_id == workspace.id && l.pool == request.pool && l.name == request.name) {
                ensure!(existing.scope == scope && request.mode.is_none_or(|m| m == existing.mode) && request.resource.as_ref().is_none_or(|r| r == &existing.resource), "lease name already acquired with different settings; release it first");
                let mut lease = existing.clone();
                if let Some(reason) = request.reason { tx.execute("UPDATE resource_leases SET reason=?2 WHERE id=?1", params![lease.id, reason])?; lease.reason = Some(reason); }
                tx.commit()?;
                return Ok(Acquisition::Acquired(lease));
            }
            if let Some(resource) = &request.resource { ensure!(definition.resources.contains_key(resource), "unknown resource {resource} in pool {}", request.pool); }
            let active: Vec<_> = all.iter().filter(|l| l.scope == scope && l.pool == request.pool).collect();
            let stored: Option<String> = tx.query_row("SELECT definition FROM resource_pools WHERE scope=?1 AND name=?2", params![scope, request.pool], |r| r.get(0)).optional()?;
            if let Some(stored) = stored {
                ensure!(active.is_empty() || serde_json::from_str::<Definition>(&stored)? == definition,
                    "resource definition changed while leases are active; align repo configs or release all leases before changing kind/capacity/members");
            }
            tx.execute("INSERT INTO resource_pools(scope,name,definition) VALUES (?1,?2,?3) ON CONFLICT(scope,name) DO UPDATE SET definition=excluded.definition",
                params![scope,request.pool,serde_json::to_string(&definition)?])?;
            let pool_available = definition.capacity.saturating_sub(pool_used(&active));
            let eligible: Vec<_> = definition.resources.iter().filter(|(name, _)|
                request.resource.as_ref().is_none_or(|wanted| wanted == *name)
            ).filter_map(|(name, settings)| {
                let mode = settings.mode(request.mode)?;
                Some((name, settings, mode, usage(&active, name)))
            }).collect();
            ensure!(!eligible.is_empty(), "requested mode is incompatible with the selected resource(s); read/write require kind = rwlock, permit requires semaphore");
            let selected = eligible.into_iter().filter(|(_, settings, mode, used)|
                settings.can_acquire(*mode, *used, pool_available)
            ).min_by(|a,b| (u64::from(a.3.slots())*u64::from(b.1.capacity)).cmp(&(u64::from(b.3.slots())*u64::from(a.1.capacity))).then_with(|| a.0.cmp(b.0)));
            let Some((resource, settings, mode, _)) = selected else { return Ok(Acquisition::Busy(format!("no compatible capacity for {} in pool {}", request.resource.as_deref().unwrap_or("any resource"), request.pool))); };
            let lease = ResourceLease {
                mode, id: Uuid::new_v4().to_string(), workspace_id: workspace.id, scope, pool: request.pool, name: request.name,
                resource: resource.clone(), reason: request.reason.or_else(|| settings.reason.clone()).or(definition.reason.clone()),
                created_at: i64::try_from(crate::simulators::now())?,
            };
            tx.execute("INSERT INTO resource_leases(id,workspace_id,scope,pool,name,resource,reason,created_at,mode) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![lease.id,lease.workspace_id,lease.scope,lease.pool,lease.name,lease.resource,lease.reason,lease.created_at,lease.mode.as_str()])?;
            tx.commit()?;
            Ok(Acquisition::Acquired(lease))
        }).await
    }

    pub async fn list_resources(&self, selector: Option<String>) -> Result<Vec<ResourceLease>> {
        let owner = match selector {
            Some(selector) => Some(self.get(selector).await?.id),
            None => None,
        };
        self.store.run(move |db| leases(db, owner.as_deref())).await
    }

    pub async fn release_resource(
        &self,
        selector: String,
        pool: String,
        name: String,
    ) -> Result<()> {
        let workspace = self.get(selector).await?;
        self.touch(&workspace.id).await;
        // Release must work even after a definition is edited or deleted.
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let ready: bool = tx.query_row(
                    "SELECT state='ready' FROM workspaces WHERE id=?1",
                    [&workspace.id],
                    |r| r.get(0),
                )?;
                ensure!(ready, "workspace is not ready");
                ensure!(
                    tx.execute(
                        "DELETE FROM resource_leases WHERE workspace_id=?1 AND pool=?2 AND name=?3",
                        params![workspace.id, pool, name]
                    )? == 1,
                    "unknown resource lease"
                );
                tx.commit()?;
                Ok(())
            })
            .await
    }

    pub async fn resource_overview(&self, selector: String) -> Result<Overview> {
        let workspace = self.get(selector).await?;
        let definitions = self.resource_definitions(&workspace)?;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                let all = leases(&tx, None)?;
                let mut pools = Vec::new();
                for (name, (scope, definition)) in definitions {
                    let active: Vec<_> = all
                        .iter()
                        .filter(|l| l.scope == scope && l.pool == name)
                        .collect();
                    let stored: Option<String> = tx
                        .query_row(
                            "SELECT definition FROM resource_pools WHERE scope=?1 AND name=?2",
                            params![scope, name],
                            |r| r.get(0),
                        )
                        .optional()?;
                    let matches = active.is_empty()
                        || stored
                            .map(|s| serde_json::from_str::<Definition>(&s))
                            .transpose()?
                            .as_ref()
                            == Some(&definition);
                    let used = pool_used(&active);
                    let pool_available = if matches {
                        definition.capacity.saturating_sub(used)
                    } else {
                        0
                    };
                    let resources: Vec<_> = definition
                        .resources
                        .iter()
                        .map(|(name, r)| {
                            let occupancy = usage(&active, name);
                            let used = occupancy.slots();
                            ResourceStatus {
                                kind: r.kind,
                                readers: occupancy.readers,
                                writers: occupancy.writers,
                                read_available: matches
                                    && r.can_acquire(LockMode::Read, occupancy, pool_available),
                                write_available: matches
                                    && r.can_acquire(LockMode::Write, occupancy, pool_available),
                                name: name.clone(),
                                capacity: r.capacity,
                                used,
                                available: r.capacity.saturating_sub(used).min(pool_available),
                            }
                        })
                        .collect();
                    let available = pool_available.min(resources.iter().map(|r| r.available).sum());
                    pools.push(PoolStatus {
                        name,
                        scope,
                        capacity: definition.capacity,
                        used,
                        available,
                        configuration_matches: matches,
                        resources,
                    });
                }
                Ok(Overview {
                    pools,
                    leases: all
                        .into_iter()
                        .filter(|l| l.workspace_id == workspace.id)
                        .collect(),
                })
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_sum_member_capacities_and_reject_invalid_definitions() {
        let config: crate::repo_config::RepoConfig = toml::from_str("[resources.lock]\n[resource_pools.workers.resources.a]\n[resource_pools.workers.resources.b]\ncapacity=2\n").unwrap();
        let normalized = definitions(&config.resources, &config.resource_pools).unwrap();
        assert_eq!(normalized["lock"].capacity, 1);
        assert_eq!(normalized["workers"].capacity, 3);
        for text in [
            "[resources.lock]\ncapacity=0",
            "[resource_pools.empty]\ncapacity=2",
            "[resources.lock]\n[resource_pools.lock.resources.a]",
            "[resources.'invalid/name']",
            "[resource_pools.p.resources.a]\ncapacity=65536",
            "[resources.x]\nreason='  '",
            "[resources.cache]\nkind='rwlock'\ncapacity=2",
        ] {
            let config: crate::repo_config::RepoConfig = toml::from_str(text).unwrap();
            assert!(
                definitions(&config.resources, &config.resource_pools).is_err(),
                "{text}"
            );
        }
        assert!(
            toml::from_str::<crate::repo_config::RepoConfig>("[resources.x]\ncapcity=2").is_err()
        );
        assert!(
            toml::from_str::<crate::repo_config::RepoConfig>("[resources.x]\nkind='unknown'")
                .is_err()
        );
    }
}
