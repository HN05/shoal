//! Cooperative semaphores and reader/writer locks; only Shoal bookkeeping is enforced.
mod acquire;
mod catalog;
mod freeing;
mod records;
mod release;
mod repositories;
mod usage;

pub use records::{holding_workspaces, leases, row_acquirer};
pub use repositories::{borrowed, ensure_no_external_leases};
pub use shoal_protocol::resources::{
    Catalog, CatalogPool, Definition, LockMode, Overview, PoolStatus, RepositoryView,
    ResourceConfig, ResourceKind, ResourceLease, ResourceRequest, ResourceStatus, Scope,
    definitions,
};
#[cfg(test)]
mod tests;
