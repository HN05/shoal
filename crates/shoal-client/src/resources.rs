//! Resource pools: their configuration, leases and what they have free.
use crate::repository_ids::RepositoryId;
use crate::space_ids::SpaceId;
use std::{collections::BTreeMap, fmt, path::PathBuf, str::FromStr};

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    approvals::Lifetime,
    state::{states, text_key},
    validate,
};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct RepositoryView {
    pub id: RepositoryId,
    pub path: PathBuf,
}

/// Where a pool's leases are shared: across every repository or within one.
/// Spelled `global` or `repo/<repository id>` in the database and JSON.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scope {
    Global,
    Repo(RepositoryId),
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Global => f.write_str("global"),
            Self::Repo(id) => write!(f, "repo/{id}"),
        }
    }
}

impl FromStr for Scope {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        match (text, text.strip_prefix("repo/")) {
            ("global", _) => Ok(Self::Global),
            (_, Some(id)) if !id.is_empty() => Ok(Self::Repo(id.to_owned().into())),
            _ => anyhow::bail!("invalid resource scope: {text}"),
        }
    }
}

text_key!(Scope);

states!(
    #[derive(Default)]
    ResourceKind {
        #[default]
        Semaphore => "semaphore",
        Rwlock => "rwlock",
        Repo => "repo",
    }
);

states!(LockMode: ValueEnum {
    Permit => "permit",
    Read => "read",
    Write => "write",
});

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourceConfig {
    pub requires_approval: bool,
    pub approval_lifetime: Lifetime,
    pub kind: ResourceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub capacity: u32,
    pub reason: Option<String>,
}

impl Default for ResourceConfig {
    fn default() -> Self {
        Self {
            requires_approval: false,
            approval_lifetime: Lifetime::Lease,
            kind: ResourceKind::Semaphore,
            repo: None,
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
            resource.validate_repository(name)?;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<RepositoryView>,
    pub mode: LockMode,
    pub id: String,
    pub workspace_id: SpaceId,
    pub scope: Scope,
    pub pool: String,
    /// Caller-chosen lease name; one workspace may hold several per pool.
    pub name: String,
    /// The pool member actually granted.
    pub resource: String,
    pub reason: Option<String>,
    pub created_at: crate::time::UnixTime,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ResourceRequest {
    #[serde(default)]
    pub mode: Option<LockMode>,
    /// Restrict selection to members of this kind.
    #[serde(default)]
    pub kind: Option<ResourceKind>,
    pub pool: String,
    pub name: String,
    pub resource: Option<String>,
    pub reason: Option<String>,
    /// Take a place in the pool's arrival-order queue while it is busy.
    #[serde(default)]
    pub queue: bool,
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
    pub scope: Scope,
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

/// A configured pool with the scope its leases count against.
#[derive(Debug, Serialize, Deserialize)]
pub struct CatalogPool {
    pub name: String,
    pub scope: Scope,
    #[serde(flatten)]
    pub definition: Definition,
}

/// The pools a workspace's resource and repository leases draw from.
#[derive(Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub pools: Vec<CatalogPool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceOverview {
    pub workspace: crate::model::Workspace,
    #[serde(flatten)]
    pub overview: Overview,
}

impl ResourceConfig {
    fn validate_repository(&self, name: &str) -> Result<()> {
        match (self.kind, self.repo.as_deref()) {
            (ResourceKind::Repo, Some(repo)) => {
                if !crate::repository::is_remote_url(repo) {
                    validate::name("repository", repo)?;
                }
                ensure!(
                    self.capacity == 1,
                    "repo resource {name} must use capacity 1; readers share its one slot"
                );
            }
            (ResourceKind::Repo, None) => bail!("repo resource {name} requires repo"),
            (_, Some(_)) => bail!("repo is only valid for kind = repo on resource {name}"),
            (_, None) => {}
        }
        Ok(())
    }
}
