//! Deletion of branches that removal kept once a remote or the default branch
//! has their commits too. Nothing is lost, so nobody is told.
use crate::client::repository_ids::RepositoryId;
use std::{collections::HashMap, time::Duration};

use anyhow::{Result, ensure};
use tokio::time::Instant;

use crate::{
    daemon::{log, workspace::Manager},
    git::RemoteHead,
};

/// Ask a repository's remotes for their branches at most this often.
const HEADS_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// What the kept-branch sweep remembers between passes.
#[derive(Default)]
pub struct State {
    /// The branches each repository's remotes advertised, and when.
    heads: HashMap<RepositoryId, (Instant, Vec<RemoteHead>)>,
}

impl State {
    /// The branches the repository's remotes advertise, asking them again
    /// once the last answer is older than [`HEADS_INTERVAL`]. `None` when a
    /// remote cannot answer, since a missing answer proves nothing.
    async fn advertised(
        &mut self,
        manager: &Manager,
        repository_id: &RepositoryId,
    ) -> Option<&[RemoteHead]> {
        let now = Instant::now();
        let stale = self
            .heads
            .get(repository_id)
            .is_none_or(|(at, _)| now.duration_since(*at) >= HEADS_INTERVAL);
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
        match checkout_answers(manager, &repository_id).await {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                failed.push(format!("{repository_id}: {error:#}"));
                continue;
            }
        }
        // Squash and rebase merges show only on the fetched default branch;
        // any other evidence comes from what the remotes advertise now, never
        // from remote-tracking refs, which may be stale or unmapped.
        if !manager.default_branch_fresh(&repository_id).await {
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

/// Whether the repository's checkout, where its kept branches' Git runs,
/// answers, as [`Manager::sweep_skips`] tells.
async fn checkout_answers(manager: &Manager, repository_id: &RepositoryId) -> Result<bool> {
    let repo = manager.repository(repository_id).await?;
    Ok(!manager.sweep_skips(&repo.path).await)
}
