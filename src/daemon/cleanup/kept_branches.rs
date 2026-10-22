//! Deletion of branches that removal kept once a remote or the default branch
//! has their commits too. Nothing is lost, so nobody is told.
use anyhow::{Result, ensure};

use super::merged::Fetches;
use crate::daemon::workspace::Manager;

/// One pass over every repository with kept branches; a failure in one does
/// not skip the others.
pub async fn sweep(manager: &Manager, fetches: &mut Fetches) -> Result<()> {
    let mut failed = Vec::new();
    for repository_id in manager.kept_branch_repositories().await? {
        // A stale tracking ref could count commits as pushed that the
        // remote no longer has.
        if !fetches.fresh(manager, &repository_id).await {
            continue;
        }
        if let Err(error) = manager
            .delete_kept_branches_kept_elsewhere(&repository_id)
            .await
        {
            failed.push(format!("{repository_id}: {error:#}"));
        }
    }
    ensure!(failed.is_empty(), "{}", failed.join("; "));
    Ok(())
}
