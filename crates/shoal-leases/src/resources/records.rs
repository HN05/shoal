//! The resource lease records: reading, inserting and naming leases, and each
//! pool's recorded definition.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;

use shoal_protocol::acquirers::Acquirer;
pub use shoal_protocol::resources::{
    Definition, LockMode, RepositoryView, ResourceKind, ResourceLease, Scope,
};

/// Operations on the client type [`ResourceLease`] that only the `shoal` command uses.
pub(super) trait ResourceLeaseExt {
    /// Leases record the member's kind through their mode and repository binding.
    fn kind(&self) -> ResourceKind;
}

impl ResourceLeaseExt for ResourceLease {
    fn kind(&self) -> ResourceKind {
        match (&self.repository, self.mode) {
            (Some(_), _) => ResourceKind::Repo,
            (None, LockMode::Permit) => ResourceKind::Semaphore,
            (None, LockMode::Read | LockMode::Write) => ResourceKind::Rwlock,
        }
    }
}

pub(super) const LEASE_COLUMNS: &str = "id,workspace_id,scope,pool,name,resource,reason,created_at,mode,
    acquirer,acquirer_id,
    (SELECT repository_id FROM repository_resource_leases WHERE lease_id=resource_leases.id) AS repository_id,
    (SELECT path FROM repository_resource_leases WHERE lease_id=resource_leases.id) AS repository_path";

pub(super) fn row_lease(row: &rusqlite::Row<'_>) -> rusqlite::Result<ResourceLease> {
    Ok(ResourceLease {
        repository: row
            .get::<_, Option<RepositoryId>>("repository_id")?
            .map(|id| {
                Ok::<_, rusqlite::Error>(RepositoryView {
                    id,
                    path: row.get::<_, String>("repository_path")?.into(),
                })
            })
            .transpose()?,
        id: row.get("id")?,
        workspace_id: row.get("workspace_id")?,
        scope: row.get("scope")?,
        pool: row.get("pool")?,
        name: row.get("name")?,
        resource: row.get("resource")?,
        reason: row.get("reason")?,
        created_at: row.get("created_at")?,
        mode: row.get("mode")?,
        acquirer: row_acquirer(row)?,
    })
}

/// The acquirer a lease row records in its two columns.
pub fn row_acquirer(row: &rusqlite::Row<'_>) -> rusqlite::Result<Acquirer> {
    let index = row.as_ref().column_index("acquirer")?;
    Acquirer::from_parts(row.get(index)?, row.get("acquirer_id")?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, error.into())
    })
}

pub(super) fn leases_query(by_owner: bool) -> String {
    let filter = if by_owner {
        "WHERE workspace_id=?1"
    } else {
        ""
    };
    format!(
        "SELECT {LEASE_COLUMNS} FROM resource_leases {filter} ORDER BY scope,pool,resource,name,id"
    )
}

pub fn leases(db: &Connection, owner: Option<&SpaceId>) -> Result<Vec<ResourceLease>> {
    Ok(db
        .prepare(&leases_query(owner.is_some()))?
        .query_map(rusqlite::params_from_iter(owner), row_lease)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The workspaces that hold a lease of the pool or resource `name`.
pub fn holding_workspaces(db: &Connection, name: &str) -> Result<Vec<SpaceId>> {
    Ok(db
        .prepare("SELECT DISTINCT workspace_id FROM resource_leases WHERE pool=?1 OR resource=?1")?
        .query_map([name], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

// Deliberately independent of scope: configuration drift must not hide a name
// already held by this workspace under the previous scope.
pub(super) fn named_lease(
    db: &Connection,
    workspace: &SpaceId,
    pool: &str,
    name: &str,
) -> Result<Option<ResourceLease>> {
    Ok(db
        .query_row(
            &format!("SELECT {LEASE_COLUMNS} FROM resource_leases WHERE workspace_id=?1 AND pool=?2 AND name=?3"),
            params![workspace, pool, name],
            row_lease,
        )
        .optional()?)
}

pub(super) fn pool_leases(
    db: &Connection,
    scope: &Scope,
    pool: &str,
) -> Result<Vec<ResourceLease>> {
    Ok(db
        .prepare(&format!(
            "SELECT {LEASE_COLUMNS} FROM resource_leases WHERE scope=?1 AND pool=?2 ORDER BY resource,name,id"
        ))?
        .query_map(params![scope, pool], row_lease)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The definition recorded when a pool's leases were granted, if any.
pub(super) fn stored_definition(
    tx: &Transaction<'_>,
    scope: &Scope,
    pool: &str,
) -> Result<Option<Definition>> {
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

pub(super) fn save_definition(
    tx: &Transaction<'_>,
    scope: &Scope,
    pool: &str,
    definition: &Definition,
) -> Result<()> {
    tx.execute(
        "INSERT INTO resource_pools(scope,name,definition) VALUES (?1,?2,?3) ON CONFLICT(scope,name) DO UPDATE SET definition=excluded.definition",
        params![scope, pool, serde_json::to_string(definition)?],
    )?;
    Ok(())
}

pub(super) fn insert_lease(tx: &Transaction<'_>, lease: &ResourceLease) -> Result<()> {
    tx.execute(
        "INSERT INTO resource_leases(id,workspace_id,scope,pool,name,resource,reason,created_at,mode,acquirer,acquirer_id)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            lease.id,
            lease.workspace_id,
            lease.scope,
            lease.pool,
            lease.name,
            lease.resource,
            lease.reason,
            lease.created_at,
            lease.mode,
            lease.acquirer.kind(),
            lease.acquirer.id()
        ],
    )?;
    Ok(())
}
