//! Deleting the branches removal kept once a remote or the default branch
//! has their commits too. Nothing is lost, so nobody is told.
use crate::daemon::Daemon;
use shoal_ids::repository_ids::RepositoryId;
use std::{collections::HashMap, time::Duration};

use anyhow::{Result, ensure};
use tokio::time::Instant;

use crate::{git::RemoteHead, log};

/// Ask a repository's remotes for their branches at most this often.
const HEADS_INTERVAL: Duration = Duration::from_secs(10 * 60);

#[derive(Default)]
pub struct State {
    heads: HashMap<RepositoryId, (Instant, Vec<RemoteHead>)>,
}

impl State {
    /// Asks again once the last answer is older than [`HEADS_INTERVAL`].
    /// `None` when a remote cannot answer, since a missing answer proves
    /// nothing.
    async fn advertised(
        &mut self,
        daemon: &dyn Daemon,
        repository_id: &RepositoryId,
    ) -> Option<&[RemoteHead]> {
        let now = Instant::now();
        let stale = self
            .heads
            .get(repository_id)
            .is_none_or(|(at, _)| now.duration_since(*at) >= HEADS_INTERVAL);
        if stale {
            match daemon.spaces().remote_heads(daemon, repository_id).await {
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

/// A failure in one repository does not skip the others.
pub async fn sweep(daemon: &dyn Daemon, state: &mut State) -> Result<()> {
    let mut failed = Vec::new();
    for repository_id in daemon.spaces().kept_branch_repositories().await? {
        match checkout_answers(daemon, &repository_id).await {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                failed.push(format!("{repository_id}: {error:#}"));
                continue;
            }
        }
        // Squash and rebase merges show only on the fetched default branch.
        // Any other evidence comes from what the remotes advertise now,
        // never from remote-tracking refs, which may be stale or unmapped.
        if !daemon.default_branch_fresh(&repository_id).await {
            continue;
        }
        let Some(heads) = state.advertised(daemon, &repository_id).await else {
            continue;
        };
        if let Err(error) = daemon
            .spaces()
            .delete_kept_branches_kept_elsewhere(daemon, &repository_id, heads)
            .await
        {
            failed.push(format!("{repository_id}: {error:#}"));
        }
    }
    ensure!(failed.is_empty(), "{}", failed.join("; "));
    Ok(())
}

/// The kept branches' Git runs in the checkout. [`Manager::sweep_skips`]
/// tells whether it answers.
async fn checkout_answers(daemon: &dyn Daemon, repository_id: &RepositoryId) -> Result<bool> {
    let repo = daemon
        .repository(crate::repository::Selector::from(repository_id))
        .await?;
    Ok(!daemon
        .spaces()
        .file_access
        .sweep_skips(&daemon.spaces().notifications, &repo.path)
        .await)
}
