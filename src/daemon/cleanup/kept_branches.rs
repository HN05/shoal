//! Deletion of branches that removal kept once a remote or the default branch
//! has their commits too. Nothing is lost, so nobody is told.
use anyhow::{Result, ensure};

use super::merged::{FetchScope, Fetches};
use crate::daemon::workspace::Manager;

/// When each repository's remotes were last fetched for kept branches.
pub struct State(Fetches);

impl Default for State {
    fn default() -> Self {
        Self(Fetches::new(FetchScope::AllRemotes))
    }
}

/// One pass over every repository with kept branches; a failure in one does
/// not skip the others.
pub async fn sweep(manager: &Manager, state: &mut State) -> Result<()> {
    let mut failed = Vec::new();
    for repository_id in manager.kept_branch_repositories().await? {
        // Any remote-tracking ref can count a commit as kept, so a stale
        // one of any remote could approve deleting the only copy.
        if !state.0.fresh(manager, &repository_id).await {
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
