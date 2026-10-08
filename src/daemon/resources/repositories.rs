//! Borrow registered checkouts without creating or deleting filesystem views.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

use super::{Definition, ResourceLease};
use crate::{
    daemon::{store, workspace::Manager},
    forge::repository,
    model::Repository,
};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct RepositoryView {
    pub id: String,
    pub path: PathBuf,
}

/// Registered repositories named by a pool's members, keyed by selector.
pub(super) type Related = BTreeMap<String, Result<Repository>>;

impl Manager {
    /// Resolve selectors before the claim transaction: remote identity reads
    /// each local checkout's origin. Failures surface only if their member is chosen.
    pub(super) async fn related_repositories(&self, definition: &Definition) -> Result<Related> {
        let selectors: BTreeSet<_> = definition
            .resources
            .values()
            .filter_map(|resource| resource.repo.as_deref())
            .collect();
        if selectors.is_empty() {
            return Ok(Related::new());
        }
        let repositories = self.repositories().await?;
        let mut related = Related::new();
        for selector in selectors {
            let repo = repository::select(&repositories, selector)
                .await
                .cloned()
                .with_context(|| format!("resolve related repository {selector}"));
            related.insert(selector.to_owned(), repo);
        }
        Ok(related)
    }
}

pub(super) fn resolve(
    db: &Connection,
    selector: &str,
    related: &mut Related,
) -> Result<RepositoryView> {
    let repo = related
        .remove(selector)
        .with_context(|| format!("related repository was not resolved: {selector}"))??;
    ensure!(
        store::exists(
            db,
            "SELECT 1 FROM repositories WHERE id=?1 AND path=?2",
            params![
                repo.id,
                repo.path.to_str().context("repository path is not UTF-8")?
            ]
        )?,
        "related repository registration changed: {selector}"
    );
    ensure!(
        !store::exists(
            db,
            "SELECT 1 FROM repository_removals WHERE repository_id=?1",
            [&repo.id]
        )?,
        "related repository removal is incomplete: {selector}"
    );
    ensure!(
        fs::canonicalize(&repo.path)
            .with_context(|| format!("related repository checkout is unavailable: {selector}"))?
            == repo.path
            && repo.path.is_dir()
            && repo.path.join(".git").exists(),
        "related repository checkout is unavailable or moved: {selector}"
    );
    ensure!(
        !store::exists(
            db,
            "SELECT 1 FROM workspaces WHERE path=?1",
            [repo.path.to_str().context("repository path is not UTF-8")?]
        )?,
        "related repository checkout belongs to a managed workspace: {selector}"
    );
    Ok(RepositoryView {
        id: repo.id,
        path: repo.path,
    })
}

pub(super) fn record(db: &Connection, lease: &ResourceLease) -> Result<()> {
    if let Some(repo) = &lease.repository {
        db.execute(
            "INSERT INTO repository_resource_leases(lease_id,repository_id,path) VALUES (?1,?2,?3)",
            params![
                lease.id,
                repo.id,
                repo.path.to_str().context("repository path is not UTF-8")?
            ],
        )?;
    }
    Ok(())
}

/// Check in the transaction that records removal, excluding its own workspaces
/// whose leases will be released by the shared workspace removal path.
pub(crate) fn ensure_no_external_leases(db: &Connection, repository_id: &str) -> Result<()> {
    ensure!(
        !store::exists(
            db,
            "SELECT 1 FROM repository_resource_leases r
             JOIN resource_leases l ON l.id=r.lease_id
             JOIN workspaces w ON w.id=l.workspace_id
             WHERE r.repository_id=?1 AND w.repository_id<>?1",
            [repository_id]
        )?,
        "repository is borrowed by another workspace; release its repo resource leases first"
    );
    Ok(())
}
