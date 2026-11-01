//! Keeps each registered checkout's default branch in sync with its remote:
//! every repository each [`SYNC_INTERVAL`], and a repository sooner when a
//! merge changed its remote. Commands that read the default branch use what
//! the checkout has; `shoal repo sync` and decisions that need the remote's
//! state now sync through the same path.
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use anyhow::{Error, Result};
use tokio::{
    sync::Notify,
    time::{Instant, sleep_until},
};

use crate::{
    daemon::{log, workspace::Manager},
    model::SyncedRepository,
};

const SYNC_INTERVAL: Duration = Duration::from_secs(60);
/// A fetch at most this old still shows what the remote's default branch
/// has, for decisions that would otherwise fetch it themselves.
const FRESH_FOR: Duration = Duration::from_secs(10 * 60);

/// What the syncs of each repository found, and the repositories to sync
/// before the next scheduled pass.
#[derive(Default)]
pub(crate) struct Syncs {
    repositories: Mutex<HashMap<String, Health>>,
    requested: Mutex<HashSet<String>>,
    wake: Notify,
}

/// The outcome of a repository's recent syncs.
#[derive(Default)]
struct Health {
    /// When its remotes were last fetched.
    fetched: Option<Instant>,
}

impl Syncs {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Health>> {
        self.repositories
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn fetched(&self, repository_id: &str, at: Instant) {
        self.lock()
            .entry(repository_id.to_owned())
            .or_default()
            .fetched = Some(at);
    }

    fn fetched_since(&self, repository_id: &str, since: Instant) -> bool {
        self.lock()
            .get(repository_id)
            .and_then(|health| health.fetched)
            .is_some_and(|at| at >= since)
    }

    fn take_requested(&self) -> Vec<String> {
        self.requested
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
            .collect()
    }
}

/// How far a sync got.
enum Synced {
    /// The remotes could not be fetched.
    Unfetched(Error),
    /// The remotes were fetched; the local default branch followed them or
    /// could not.
    Fetched(Result<SyncedRepository>),
}

impl Manager {
    /// Fetch the repository's default branch and fast-forward the local one,
    /// as the background syncs do.
    pub async fn sync_repository(
        &self,
        selector: &crate::repository::Selector,
    ) -> Result<SyncedRepository> {
        let repo = self.repository(selector).await?;
        match self.sync(&repo.id).await {
            Synced::Unfetched(error) => Err(error),
            Synced::Fetched(synced) => synced,
        }
    }

    /// Fetch the default branch's remotes now, for a decision that needs what
    /// they have; a local default branch that cannot follow them does not
    /// fail it.
    pub(crate) async fn fetch_default_branch(&self, repository_id: &str) -> Result<()> {
        match self.sync(repository_id).await {
            Synced::Unfetched(error) => Err(error),
            Synced::Fetched(_) => Ok(()),
        }
    }

    /// Whether the default branch's remotes were fetched recently, fetching
    /// them unless they were. Stale refs may show changes the default branch
    /// no longer has, so after a failed fetch nothing that depends on it is
    /// decided.
    pub(crate) async fn default_branch_fresh(&self, repository_id: &str) -> bool {
        let since = Instant::now().checked_sub(FRESH_FOR);
        if since.is_some_and(|since| self.syncs.fetched_since(repository_id, since)) {
            return true;
        }
        match self.fetch_default_branch(repository_id).await {
            Ok(()) => true,
            Err(error) => {
                log!("could not fetch the default branch: {error:#}");
                false
            }
        }
    }

    /// One sync under the repository's Git gate, recorded for later decisions.
    async fn sync(&self, repository_id: &str) -> Synced {
        let started = Instant::now();
        let _git = self.gates.lock_git(repository_id).await;
        let synced = async {
            let repo = self.repository(repository_id).await?;
            let default = self.fetch_default_remotes(&repo).await?;
            Ok((repo, default))
        }
        .await;
        let (repo, default) = match synced {
            Ok(fetched) => fetched,
            Err(error) => return Synced::Unfetched(error),
        };
        self.syncs.fetched(repository_id, started);
        Synced::Fetched(self.fast_forward_default(&repo, &default).await)
    }

    /// Sync each repository, logging the ones that fail.
    async fn sync_all(&self, repository_ids: Vec<String>) {
        for repository_id in repository_ids {
            let error = match self.sync(&repository_id).await {
                Synced::Unfetched(error) | Synced::Fetched(Err(error)) => error,
                Synced::Fetched(Ok(_)) => continue,
            };
            log!("sync of repository {repository_id}: {error:#}");
        }
    }
}

/// Sync every repository at once and then each [`SYNC_INTERVAL`], and a
/// requested one as soon as it is asked for.
pub async fn run(manager: Arc<Manager>) {
    let mut next = Instant::now();
    loop {
        let repository_ids = if Instant::now() >= next {
            next = Instant::now() + SYNC_INTERVAL;
            manager.syncs.take_requested();
            match manager.repositories().await {
                Ok(repositories) => repositories.into_iter().map(|repo| repo.id).collect(),
                Err(error) => {
                    log!("checkout sync: {error:#}");
                    Vec::new()
                }
            }
        } else {
            manager.syncs.take_requested()
        };
        {
            let _operation = manager.gates.operation().await;
            manager.sync_all(repository_ids).await;
        }
        tokio::select! {
            _ = sleep_until(next) => {}
            _ = manager.syncs.wake.notified() => {}
        }
    }
}

#[cfg(test)]
mod tests;
