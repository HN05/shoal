//! Deletion of branches that removal kept once a remote or the default branch
//! has their commits too. Nothing is lost, so nobody is told.
use std::collections::HashMap;

use anyhow::{Result, ensure};
use tokio::time::Instant;

use super::merged::{FETCH_INTERVAL, Fetches};
use crate::{
    daemon::{log, workspace::Manager},
    git::RemoteHead,
};

/// What the kept-branch sweep remembers between passes.
#[derive(Default)]
pub struct State {
    fetches: Fetches,
    /// The branches each repository's remotes advertised, and when.
    heads: HashMap<String, (Instant, Vec<RemoteHead>)>,
}

impl State {
    /// The branches the repository's remotes advertise, asking them again
    /// once the last answer is older than the fetch interval. `None` when a
    /// remote cannot answer, since a missing answer proves nothing.
    async fn advertised(
        &mut self,
        manager: &Manager,
        repository_id: &str,
    ) -> Option<&[RemoteHead]> {
        let now = Instant::now();
        let stale = self
            .heads
            .get(repository_id)
            .is_none_or(|(at, _)| now.duration_since(*at) >= FETCH_INTERVAL);
        if stale {
            match manager.remote_heads(repository_id).await {
                Ok(heads) => {
                    self.heads.insert(repository_id.to_owned(), (now, heads));
                }
                Err(error) => {
                    log!("kept branch cleanup could not list remote branches: {error:#}");
                    self.heads.remove(repository_id);
                    return None;
                }
            }
        }
        self.heads
            .get(repository_id)
            .map(|(_, heads)| heads.as_slice())
    }
}

/// One pass over every repository with kept branches; a failure in one does
/// not skip the others.
pub async fn sweep(manager: &Manager, state: &mut State) -> Result<()> {
    let mut failed = Vec::new();
    for repository_id in manager.kept_branch_repositories().await? {
        // Squash and rebase merges show only on the fetched default branch;
        // any other evidence comes from what the remotes advertise now, never
        // from remote-tracking refs, which may be stale or unmapped.
        if !state.fetches.fresh(manager, &repository_id).await {
            continue;
        }
        let Some(heads) = state.advertised(manager, &repository_id).await else {
            continue;
        };
        if let Err(error) = manager
            .delete_kept_branches_kept_elsewhere(&repository_id, heads)
            .await
        {
            failed.push(format!("{repository_id}: {error:#}"));
        }
    }
    ensure!(failed.is_empty(), "{}", failed.join("; "));
    Ok(())
}
