//! The configured pools a workspace sees, its leases and how full each pool is.
use anyhow::{Result, ensure};
use std::collections::BTreeMap;

use super::{
    records::{leases, stored_definition},
    usage::{PoolUsage, ResourceConfigExt, grouped_usage},
};
use crate::{Leases, daemon::Daemon, model::Workspace};
pub use shoal_protocol::resources::{
    Catalog, CatalogPool, Definition, LockMode, Overview, PoolStatus, ResourceLease,
    ResourceStatus, Scope, definitions,
};
use shoal_protocol::space_names::SpaceSelector;

impl Leases {
    /// Global definitions plus the workspace repository's, keyed by pool name
    /// with the scope each belongs to.
    pub(super) async fn resource_definitions(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
    ) -> Result<BTreeMap<String, (Scope, Definition)>> {
        let config = self.config.current();
        let repo = daemon.settings(&config, workspace).await?;
        let global = config.global_settings()?;
        let global = definitions(&global.resources, &global.resource_pools)?;
        let local = definitions(&repo.resources, &repo.resource_pools)?;
        let mut result: BTreeMap<_, _> = global
            .into_iter()
            .map(|(name, definition)| (name, (Scope::Global, definition)))
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
                    (Scope::Repo(workspace.repository().clone()), definition),
                );
            }
        }
        Ok(result)
    }

    pub async fn list_resources(
        &self,
        daemon: &dyn Daemon,
        selector: Option<&SpaceSelector>,
    ) -> Result<Vec<ResourceLease>> {
        let owner = self.workspace_filter(daemon, selector).await?;
        self.store.run(move |db| leases(db, owner.as_ref())).await
    }

    pub async fn resource_catalog(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<Catalog> {
        let workspace = daemon.workspace(selector).await?;
        let pools = self
            .resource_definitions(daemon, &workspace)
            .await?
            .into_iter()
            .map(|(name, (scope, definition))| CatalogPool {
                name,
                scope,
                definition,
            })
            .collect();
        Ok(Catalog { pools })
    }

    pub async fn resource_overview(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<Overview> {
        let workspace = daemon.workspace(selector).await?;
        let definitions = self.resource_definitions(daemon, &workspace).await?;
        self.store
            .read(move |tx| {
                let all = leases(tx, None)?;
                let mut grouped = grouped_usage(&all);
                let mut pools = Vec::new();
                for (name, (scope, definition)) in &definitions {
                    let usage = grouped.remove(&(scope, name.as_str())).unwrap_or_default();
                    let matches = usage.slots == 0
                        || stored_definition(tx, scope, name)?.as_ref() == Some(definition);
                    pools.push(pool_status(
                        name.clone(),
                        scope.clone(),
                        definition,
                        &usage,
                        matches,
                    ));
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

pub(super) fn pool_status(
    name: String,
    scope: Scope,
    definition: &Definition,
    usage: &PoolUsage<'_>,
    matches: bool,
) -> PoolStatus {
    let used = usage.slots;
    let pool_available = if matches {
        definition.capacity.saturating_sub(used)
    } else {
        0
    };
    let resources: Vec<_> = definition
        .resources
        .iter()
        .map(|(name, r)| {
            let occupancy = usage.member(name);
            let used = occupancy.slots();
            ResourceStatus {
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
