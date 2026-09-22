//! Cooperative semaphores and reader/writer locks; only Shoal bookkeeping is enforced.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

use crate::{notifications::NotificationKind, state::states, store, validate, workspace::Manager};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    #[default]
    Semaphore,
    Rwlock,
}

states!(LockMode {
    Permit => "permit",
    Read => "read",
    Write => "write",
});

impl clap::ValueEnum for LockMode {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Permit, Self::Read, Self::Write]
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(clap::builder::PossibleValue::new(self.as_str()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourceConfig {
    pub requires_approval: bool,
    pub approval_lifetime: crate::access::Lifetime,
    pub kind: ResourceKind,
    pub capacity: u32,
    pub reason: Option<String>,
}

impl Default for ResourceConfig {
    fn default() -> Self {
        Self {
            requires_approval: false,
            approval_lifetime: crate::access::Lifetime::Lease,
            kind: ResourceKind::Semaphore,
            capacity: 1,
            reason: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PoolConfig {
    pub capacity: Option<u32>,
    pub reason: Option<String>,
    pub resources: BTreeMap<String, ResourceConfig>,
}

/// A normalized pool: standalone resources become one-member pools.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Definition {
    pub capacity: u32,
    pub reason: Option<String>,
    pub resources: BTreeMap<String, ResourceConfig>,
}

const MAX_CAPACITY: u64 = 65535;

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
            (1..=MAX_CAPACITY).contains(&capacity),
            "pool {name} capacity must be between 1 and {MAX_CAPACITY}"
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
        validate::lowercase_name("resource", name)?;
        validate::reason("resource", definition.reason.as_deref())?;
        ensure!(
            !definition.resources.is_empty(),
            "resource pool {name} must contain named resources"
        );
        ensure!(
            (1..=MAX_CAPACITY).contains(&u64::from(definition.capacity)),
            "capacity must be between 1 and {MAX_CAPACITY}"
        );
        for (name, resource) in &definition.resources {
            validate::lowercase_name("resource", name)?;
            validate::reason("resource", resource.reason.as_deref())?;
            ensure!(
                resource.kind != ResourceKind::Rwlock || resource.capacity == 1,
                "rwlock resource {name} must use capacity 1; readers share its one slot"
            );
            ensure!(
                (1..=MAX_CAPACITY).contains(&u64::from(resource.capacity)),
                "resource {name} capacity must be between 1 and {MAX_CAPACITY}"
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
    /// `global` or `repo/<repository id>`.
    pub scope: String,
    pub pool: String,
    /// Caller-chosen lease name; one workspace may hold several per pool.
    pub name: String,
    /// The pool member actually granted.
    pub resource: String,
    pub reason: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ResourceRequest {
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
    Approval(Box<crate::access::AccessRequest>),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResourceStatus {
    pub requires_approval: bool,
    pub approval_lifetime: crate::access::Lifetime,
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
    /// The stored definition matches the current configuration; leases must
    /// drain before a changed definition takes effect.
    pub configuration_matches: bool,
    pub resources: Vec<ResourceStatus>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Overview {
    pub pools: Vec<PoolStatus>,
    pub leases: Vec<ResourceLease>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceOverview {
    pub workspace: crate::model::Workspace,
    #[serde(flatten)]
    pub overview: Overview,
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

/// `; held by a, b` naming the workspaces with leases in the pool, for the
/// busy message and the notification the user sees.
fn holders(db: &Connection, active: &[&ResourceLease]) -> Result<String> {
    let ids: BTreeSet<&str> = active.iter().map(|l| l.workspace_id.as_str()).collect();
    let mut names = Vec::with_capacity(ids.len());
    for id in ids {
        let name: Option<String> = db
            .query_row("SELECT name FROM workspaces WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        names.push(name.unwrap_or_else(|| id.to_owned()));
    }
    Ok(if names.is_empty() {
        String::new()
    } else {
        format!("; held by {}", names.join(", "))
    })
}

pub fn leases(db: &Connection, owner: Option<&str>) -> Result<Vec<ResourceLease>> {
    Ok(db
        .prepare(
            "SELECT id,workspace_id,scope,pool,name,resource,reason,created_at,mode FROM resource_leases WHERE ?1 IS NULL OR workspace_id=?1 ORDER BY scope,pool,resource,name,id",
        )?
        .query_map([owner], row_lease)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The definition recorded when a pool's leases were granted, if any.
fn stored_definition(tx: &Transaction<'_>, scope: &str, pool: &str) -> Result<Option<Definition>> {
    let stored: Option<String> = tx
        .query_row(
            "SELECT definition FROM resource_pools WHERE scope=?1 AND name=?2",
            params![scope, pool],
            |r| r.get(0),
        )
        .optional()?;
    stored
        .map(|json| serde_json::from_str(&json))
        .transpose()
        .map_err(Into::into)
}

/// Occupancy of one pool member.
#[derive(Default, Clone, Copy)]
struct Usage {
    permits: u32,
    readers: u32,
    writers: u32,
}

impl Usage {
    /// Pool slots consumed: one per permit, one shared by all lock holders.
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
    /// The lock mode this member grants for `requested`, or `None` when the
    /// mode does not apply to its kind.
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

/// Pick the least-loaded compatible member with capacity, by relative load
/// then by name.
fn select_member<'a>(
    definition: &'a Definition,
    request: &ResourceRequest,
    active: &[&ResourceLease],
    pool_available: u32,
) -> Result<Option<(&'a String, &'a ResourceConfig, LockMode)>> {
    let eligible: Vec<_> = definition
        .resources
        .iter()
        .filter(|(name, _)| {
            request
                .resource
                .as_ref()
                .is_none_or(|wanted| wanted == *name)
        })
        .filter_map(|(name, settings)| {
            let mode = settings.mode(request.mode)?;
            Some((name, settings, mode, usage(active, name)))
        })
        .collect();
    ensure!(
        !eligible.is_empty(),
        "requested mode is incompatible with the selected resource(s); read/write require kind = rwlock, permit requires semaphore"
    );
    let load = |slots: u32, capacity: u32| u64::from(slots) * u64::from(capacity);
    Ok(eligible
        .into_iter()
        .filter(|(_, settings, mode, used)| settings.can_acquire(*mode, *used, pool_available))
        .min_by(|a, b| {
            load(a.3.slots(), b.1.capacity)
                .cmp(&load(b.3.slots(), a.1.capacity))
                .then_with(|| a.0.cmp(b.0))
        })
        .map(|(name, settings, mode, _)| (name, settings, mode)))
}

impl Manager {
    /// Global definitions plus the workspace repository's, keyed by pool name
    /// with the scope each belongs to.
    async fn resource_definitions(
        &self,
        workspace: &crate::model::Workspace,
    ) -> Result<BTreeMap<String, (String, Definition)>> {
        let repo = self.workspace_config(workspace).await?;
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
        selector: &str,
        mut request: ResourceRequest,
        scoped: bool,
    ) -> Result<Acquisition> {
        validate::lowercase_name("resource", &request.pool)?;
        validate::lowercase_name("resource", &request.name)?;
        if let Some(resource) = &request.resource {
            validate::lowercase_name("resource", resource)?;
        }
        validate::reason("resource", request.reason.as_deref())?;
        let workspace = self.workspace(selector).await?;
        self.touch(&workspace.id).await;
        let mut definitions = self.resource_definitions(&workspace).await?;
        let (scope, definition) = definitions
            .remove(&request.pool)
            .ok_or_else(|| anyhow::anyhow!("unknown resource or pool: {}", request.pool))?;
        let hook_workspace = workspace.clone();
        let hook = self.resource_hook(&workspace, true).await?;
        let _resources = self.resource_guard(&workspace.id, hook.is_some()).await?;
        if hook.is_some() {
            self.verify_worktree(&workspace).await?;
        }
        let workspace_name = workspace.name.clone();
        let acquisition = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                store::require_ready(&tx, &workspace.id)?;
                let all = leases(&tx, None)?;
                if let Some(existing) = all.iter().find(|l| {
                    l.workspace_id == workspace.id
                        && l.pool == request.pool
                        && l.name == request.name
                }) {
                    return renew_lease(tx, existing, &scope, request);
                }
                if let Some(resource) = &request.resource {
                    ensure!(
                        definition.resources.contains_key(resource),
                        "unknown resource {resource} in pool {}",
                        request.pool
                    );
                }
                let active: Vec<_> = all
                    .iter()
                    .filter(|l| l.scope == scope && l.pool == request.pool)
                    .collect();
                if let Some(stored) = stored_definition(&tx, &scope, &request.pool)? {
                    ensure!(
                        active.is_empty() || stored == definition,
                        "resource definition changed while leases are active; align repo configs or release all leases before changing kind/capacity/members"
                    );
                }
                tx.execute(
                    "INSERT INTO resource_pools(scope,name,definition) VALUES (?1,?2,?3) ON CONFLICT(scope,name) DO UPDATE SET definition=excluded.definition",
                    params![scope, request.pool, serde_json::to_string(&definition)?],
                )?;
                let target = format!("resource/{scope}/{}", request.pool);
                if scoped && let Some(pending) = crate::access::current(&tx, &workspace.id, &target, &request.name)? {
                    let member = pending.specification["member"].as_str().unwrap_or_default();
                    ensure!(request.resource.as_deref().is_none_or(|r| r == member), "access request member changed; release it first");
                    request.resource = Some(member.into());
                }
                let pool_available = definition.capacity.saturating_sub(pool_used(&active));
                let Some((resource, settings, mode)) =
                    select_member(&definition, &request, &active, pool_available)?
                        .or(select_member(&definition, &request, &[], definition.capacity)?)
                else {
                    return Ok(Acquisition::Busy(format!(
                        "no compatible capacity for {} in pool {}{}",
                        request.resource.as_deref().unwrap_or("any resource"),
                        request.pool,
                        holders(&tx, &active)?
                    )));
                };
                if scoped && settings.requires_approval {
                    let approval = crate::access::AccessRequest::new(&workspace.id, target, &request.name,
                        serde_json::json!({"definition": definition, "member": resource, "mode": mode}),
                        settings.approval_lifetime, request.reason.as_deref());
                    if let Some(approval) = crate::access::check(&tx, approval)? {
                        tx.commit()?;
                        return Ok(Acquisition::Approval(Box::new(approval)));
                    }
                }
                if !settings.can_acquire(mode, usage(&active, resource), pool_available) {
                    return Ok(Acquisition::Busy(format!("no compatible capacity in pool {}{}", request.pool, holders(&tx, &active)?)));
                }
                let lease = ResourceLease {
                    mode,
                    id: Uuid::new_v4().to_string(),
                    workspace_id: workspace.id,
                    scope,
                    pool: request.pool,
                    name: request.name,
                    resource: resource.clone(),
                    reason: request
                        .reason
                        .or_else(|| settings.reason.clone())
                        .or(definition.reason.clone()),
                    created_at: i64::try_from(crate::simulators::now())?,
                };
                tx.execute(
                    "INSERT INTO resource_leases(id,workspace_id,scope,pool,name,resource,reason,created_at,mode) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    params![
                        lease.id,
                        lease.workspace_id,
                        lease.scope,
                        lease.pool,
                        lease.name,
                        lease.resource,
                        lease.reason,
                        lease.created_at,
                        lease.mode
                    ],
                )?;
                tx.commit()?;
                Ok(Acquisition::Acquired(lease))
            })
            .await?;
        if let Acquisition::Acquired(lease) = &acquisition
            && let Some(command) = hook
        {
            crate::hooks::run_detached(
                crate::hooks::Hook::PostResourceAcquire(lease),
                &hook_workspace,
                &command,
                &self.paths,
            )
            .await
            .context("resource lease retained; repeat acquire to retry the hook, or release it")?;
        }
        if let Acquisition::Busy(message) = &acquisition {
            self.notify(
                Some(&workspace_name),
                NotificationKind::ResourceBusy,
                message.clone(),
            )
            .await;
        }
        if let Acquisition::Approval(request) = &acquisition {
            self.notify_access(&workspace_name, request).await;
        }
        Ok(acquisition)
    }

    pub async fn list_resources(&self, selector: Option<&str>) -> Result<Vec<ResourceLease>> {
        let owner = self.workspace_filter(selector).await?;
        self.store.run(move |db| leases(db, owner.as_deref())).await
    }

    pub async fn release_resource(&self, selector: &str, pool: String, name: String) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        self.touch(&workspace.id).await;
        let hook = self.resource_hook(&workspace, false).await?;
        let _resources = self.resource_guard(&workspace.id, hook.is_some()).await?;
        let id = workspace.id.clone();
        let owned = self
            .store
            .run(move |db| {
                store::require_ready(db, &id)?;
                leases(db, Some(&id))
            })
            .await?;
        if let Some(lease) = owned
            .iter()
            .find(|lease| lease.pool == pool && lease.name == name)
        {
            self.run_resource_release_hook(&workspace, lease, hook.as_deref())
                .await?;
        }
        // Release must work even after a definition is edited or deleted.
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                store::require_ready(&tx, &workspace.id)?;
                let approval = crate::access::list(&tx, Some(&workspace.id))?
                    .into_iter()
                    .find(|r| {
                        r.active
                            && r.name == name
                            && r.target.ends_with(&format!("/{pool}"))
                            && r.target.starts_with("resource/")
                    });
                let released = if let Some(approval) = approval {
                    crate::access::release(&tx, &workspace.id, &approval.target, &name)?
                } else {
                    false
                };
                ensure!(
                    tx.execute(
                        "DELETE FROM resource_leases WHERE workspace_id=?1 AND pool=?2 AND name=?3",
                        params![workspace.id, pool, name]
                    )? == 1
                        || released,
                    "unknown resource lease or access request"
                );
                tx.commit()?;
                Ok(())
            })
            .await
    }

    pub(crate) async fn resource_hook(
        &self,
        workspace: &crate::model::Workspace,
        acquire: bool,
    ) -> Result<Option<std::path::PathBuf>> {
        let config = self.workspace_config(workspace).await?;
        let command = if acquire {
            config
                .post_resource_acquire_cmd
                .as_ref()
                .or(self.config.post_resource_acquire_cmd.as_ref())
        } else {
            config
                .pre_resource_release_cmd
                .as_ref()
                .or(self.config.pre_resource_release_cmd.as_ref())
        };
        Ok(command.map(|command| workspace.path.join(command)))
    }

    pub(crate) async fn run_resource_release_hook(
        &self,
        workspace: &crate::model::Workspace,
        lease: &ResourceLease,
        command: Option<&std::path::Path>,
    ) -> Result<()> {
        if let Some(command) = command {
            self.verify_worktree(workspace).await?;
            crate::hooks::run_detached(
                crate::hooks::Hook::PreResourceRelease(lease),
                workspace,
                command,
                &self.paths,
            )
            .await
            .context("resource lease retained; retry release after fixing its hook")?;
        }
        Ok(())
    }

    pub async fn resource_overview(&self, selector: &str) -> Result<Overview> {
        let workspace = self.workspace(selector).await?;
        let definitions = self.resource_definitions(&workspace).await?;
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
                    let matches = active.is_empty()
                        || stored_definition(&tx, &scope, &name)?.as_ref() == Some(&definition);
                    pools.push(pool_status(name, scope, &definition, &active, matches));
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

/// An existing lease with the same name is returned again, refreshing its
/// reason, as long as the request does not contradict it.
fn renew_lease(
    tx: Transaction<'_>,
    existing: &ResourceLease,
    scope: &str,
    request: ResourceRequest,
) -> Result<Acquisition> {
    ensure!(
        existing.scope == scope
            && request.mode.is_none_or(|m| m == existing.mode)
            && request
                .resource
                .as_ref()
                .is_none_or(|r| r == &existing.resource),
        "lease name already acquired with different settings; release it first"
    );
    let mut lease = existing.clone();
    if let Some(reason) = request.reason {
        tx.execute(
            "UPDATE resource_leases SET reason=?2 WHERE id=?1",
            params![lease.id, reason],
        )?;
        lease.reason = Some(reason);
    }
    tx.commit()?;
    Ok(Acquisition::Acquired(lease))
}

fn pool_status(
    name: String,
    scope: String,
    definition: &Definition,
    active: &[&ResourceLease],
    matches: bool,
) -> PoolStatus {
    let used = pool_used(active);
    let pool_available = if matches {
        definition.capacity.saturating_sub(used)
    } else {
        0
    };
    let resources: Vec<_> = definition
        .resources
        .iter()
        .map(|(name, r)| {
            let occupancy = usage(active, name);
            let used = occupancy.slots();
            ResourceStatus {
                requires_approval: r.requires_approval,
                approval_lifetime: r.approval_lifetime,
                kind: r.kind,
                readers: occupancy.readers,
                writers: occupancy.writers,
                read_available: matches && r.can_acquire(LockMode::Read, occupancy, pool_available),
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
    PoolStatus {
        name,
        scope,
        capacity: definition.capacity,
        used,
        available,
        configuration_matches: matches,
        resources,
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
