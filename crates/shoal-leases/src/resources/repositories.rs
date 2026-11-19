//! Borrow registered checkouts without creating or deleting filesystem views.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, params};
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
};

use super::{Definition, RepositoryView, ResourceLease};
use crate::{
    daemon::{Daemon, Records, Unborrowable},
    model::Repository,
    repository,
};

/// Registered repositories named by a pool's members, keyed by selector.
pub(super) type Related = BTreeMap<String, Result<Repository>>;

/// Resolve selectors before the claim transaction: remote identity reads
/// each local checkout's origin. Failures surface only if their member is chosen.
pub(super) async fn related(daemon: &dyn Daemon, definition: &Definition) -> Result<Related> {
    let selectors: BTreeSet<_> = definition
        .resources
        .values()
        .filter_map(|resource| resource.repo.as_deref())
        .collect();
    if selectors.is_empty() {
        return Ok(Related::new());
    }
    let repositories = daemon.repositories().await?;
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

pub(super) fn resolve(
    db: &Connection,
    records: &dyn Records,
    selector: &str,
    related: &mut Related,
) -> Result<RepositoryView> {
    let repo = related
        .remove(selector)
        .with_context(|| format!("related repository was not resolved: {selector}"))??;
    let unborrowable = records.unborrowable(db, &repo)?;
    let refusal = |reason| match reason {
        Unborrowable::Registration => "registration changed",
        Unborrowable::Removal => "removal is incomplete",
        Unborrowable::Move => "move is incomplete",
        Unborrowable::Worktree => "checkout belongs to a managed workspace",
    };
    if let Some(reason) = unborrowable.filter(|reason| *reason != Unborrowable::Worktree) {
        bail!("related repository {}: {selector}", refusal(reason));
    }
    ensure!(
        fs::canonicalize(&repo.path)
            .with_context(|| format!("related repository checkout is unavailable: {selector}"))?
            == repo.path
            && repo.path.is_dir()
            && repo.path.join(".git").exists(),
        "related repository checkout is unavailable or moved: {selector}"
    );
    if let Some(reason) = unborrowable {
        bail!("related repository {}: {selector}", refusal(reason));
    }
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
pub fn ensure_no_external_leases(
    db: &Connection,
    records: &dyn Records,
    repository_id: &RepositoryId,
) -> Result<()> {
    ensure!(
        !borrowed(db, records, repository_id)?,
        "repository is borrowed by another workspace; release its repo resource leases first"
    );
    Ok(())
}

/// Whether a workspace of another repository holds a lease on one of the
/// repository's resources.
pub fn borrowed(
    db: &Connection,
    records: &dyn Records,
    repository_id: &RepositoryId,
) -> Result<bool> {
    let holders: Vec<SpaceId> = db
        .prepare(
            "SELECT DISTINCT l.workspace_id FROM repository_resource_leases r
             JOIN resource_leases l ON l.id=r.lease_id WHERE r.repository_id=?1",
        )?
        .query_map([repository_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for holder in holders {
        let repository = records.space_repository(db, &holder)?;
        if repository.is_some_and(|repository| repository != *repository_id) {
            return Ok(true);
        }
    }
    Ok(false)
}
