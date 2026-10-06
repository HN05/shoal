//! Borrow registered checkouts without creating or deleting filesystem views.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

use super::ResourceLease;
use crate::{daemon::store, forge::repository};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct RepositoryView {
    pub id: String,
    pub path: PathBuf,
}

pub(super) fn resolve(db: &Connection, selector: &str) -> Result<RepositoryView> {
    let repositories = db
        .prepare(&format!(
            "SELECT {} FROM repositories",
            store::REPOSITORY_COLUMNS
        ))?
        .query_map([], store::repository)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let matches: Vec<_> = repositories
        .iter()
        .filter(|repo| repo.id == selector || repository::name(repo) == selector)
        .collect();
    ensure!(
        !matches.is_empty(),
        "unknown related repository: {selector}"
    );
    ensure!(
        matches.len() == 1,
        "ambiguous related repository: {selector}; use its ID"
    );
    let repo = matches[0];
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
        id: repo.id.clone(),
        path: repo.path.clone(),
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
