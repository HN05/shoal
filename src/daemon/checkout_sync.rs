//! Keeps each registered checkout's default branch in sync with its remote:
//! every repository each [`SYNC_INTERVAL`], and a repository sooner when a
//! merge changed its remote. Commands that read the default branch use what
//! the checkout has; `shoal repo sync` and decisions that need the remote's
//! state now sync through the same path. A sync that keeps failing shows in
//! workspace status, then in the inbox with one notification.
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use anyhow::{Error, Result};
use tokio::{
    sync::Notify,
    task::{self, JoinError, JoinSet},
    time::{Instant, sleep_until},
};

use self::health::Health;
use crate::{
    daemon::{log, notifications::NotificationKind, workspace::Manager},
    model::{CheckoutSync, SyncedRepository},
    time::{local_minutes, unix_seconds},
};

mod health;

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
            .fetched(at);
    }

    fn fetched_since(&self, repository_id: &str, since: Instant) -> bool {
        self.lock()
            .get(repository_id)
            .is_some_and(|health| health.fetched_since(since))
    }

    fn record(&self, repository_id: &str, error: Option<&Error>) {
        self.lock()
            .entry(repository_id.to_owned())
            .or_default()
            .record(error, Instant::now(), unix_seconds());
    }

    /// Forget the repositories no longer registered.
    fn retain(&self, repository_ids: &[String]) {
        self.lock().retain(|id, _| repository_ids.contains(id));
    }

    /// The failures to tell the user about, each once.
    fn unnotified(&self) -> Vec<(String, String)> {
        let now = Instant::now();
        self.lock()
            .iter_mut()
            .filter_map(|(id, health)| Some((id.clone(), health.notify(now)?)))
            .collect()
    }

    /// Sync `repository_id` before the next scheduled pass.
    pub(crate) fn request(&self, repository_id: &str) {
        self.requested
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(repository_id.to_owned());
        self.wake.notify_one();
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

impl Synced {
    fn error(&self) -> Option<&Error> {
        match self {
            Synced::Unfetched(error) | Synced::Fetched(Err(error)) => Some(error),
            Synced::Fetched(Ok(_)) => None,
        }
    }
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

    /// How the background sync of the repository went, once it was tried.
    pub(crate) fn checkout_sync(&self, repository_id: &str) -> Option<CheckoutSync> {
        self.syncs
            .lock()
            .get(repository_id)
            .and_then(|health| health.view(repository_id))
    }

    /// The registered repositories whose syncs have failed long enough to
    /// need the user.
    pub async fn failing_syncs(&self) -> Result<Vec<CheckoutSync>> {
        let repositories = self.repositories().await?;
        let now = Instant::now();
        let syncs = self.syncs.lock();
        Ok(repositories
            .iter()
            .filter_map(|repo| {
                let health = syncs.get(&repo.id)?;
                health.needs_user(now).then(|| health.view(&repo.id))?
            })
            .collect())
    }

    /// One sync, recorded for status and later decisions.
    async fn sync(&self, repository_id: &str) -> Synced {
        let synced = self.fetch_and_fast_forward(repository_id).await;
        self.syncs.record(repository_id, synced.error());
        synced
    }

    /// Fetch and fast-forward under the repository's Git gate.
    async fn fetch_and_fast_forward(&self, repository_id: &str) -> Synced {
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
    /// Every registered repository, forgetting the syncs of those removed.
    async fn registered_repository_ids(&self) -> Vec<String> {
        match self.repositories().await {
            Ok(repositories) => {
                let ids: Vec<String> = repositories.into_iter().map(|repo| repo.id).collect();
                self.syncs.retain(&ids);
                ids
            }
            Err(error) => {
                log!("checkout sync: {error:#}");
                Vec::new()
            }
        }
    }

    /// A scheduled or requested sync: logged when it fails, and told to the
    /// user once it has failed long enough.
    async fn sync_in_background(&self, repository_id: &str) {
        let _operation = self.gates.operation().await;
        if let Some(error) = self.sync(repository_id).await.error() {
            log!("sync of repository {repository_id}: {error:#}");
        }
        self.notify_failing_syncs().await;
    }

    /// Tell the user about each sync that has failed long enough, once.
    async fn notify_failing_syncs(&self) {
        for (repository_id, error) in self.syncs.unnotified() {
            let (Some(sync), Ok(repo)) = (
                self.checkout_sync(&repository_id),
                self.repository(repository_id.as_str()).await,
            ) else {
                continue;
            };
            let name = repo.name.unwrap_or_else(|| repo.path.display().to_string());
            let message = failure_message(&name, sync.failing_since, &error);
            self.notify(None, NotificationKind::SyncFailed, message)
                .await;
        }
    }
}

/// The notification for a repository whose syncs keep failing.
fn failure_message(name: &str, failing_since: Option<u64>, error: &str) -> String {
    let since = failing_since.map_or_else(String::new, |since| {
        format!(" since {}", local_minutes(since as i64))
    });
    format!(
        "the default branch of {name} has not synced{since}: {error}; retry with shoal repo sync"
    )
}

/// Sync every repository at once and then each [`SYNC_INTERVAL`], and a
/// requested one as soon as it is asked for.
pub async fn run(manager: Arc<Manager>) {
    let mut next = Instant::now();
    let mut syncing = Syncing::default();
    loop {
        let due = if Instant::now() >= next {
            next = Instant::now() + SYNC_INTERVAL;
            manager.syncs.take_requested();
            manager.registered_repository_ids().await
        } else {
            manager.syncs.take_requested()
        };
        syncing.start(&manager, due);
        tokio::select! {
            _ = sleep_until(next) => {}
            _ = manager.syncs.wake.notified() => {}
            Some(finished) = syncing.tasks.join_next_with_id(), if !syncing.tasks.is_empty() => {
                syncing.finished(&manager, finished);
            }
        }
    }
}

/// The repositories syncing now, each in a task of its own so one whose fetch
/// hangs or whose Git gate is held delays no other, and those due again
/// while they sync, which sync once more when they finish.
#[derive(Default)]
struct Syncing {
    tasks: JoinSet<()>,
    running: HashMap<task::Id, String>,
    again: HashSet<String>,
}

impl Syncing {
    fn start(&mut self, manager: &Arc<Manager>, repository_ids: Vec<String>) {
        for repository_id in repository_ids {
            if self
                .running
                .values()
                .any(|running| running == &repository_id)
            {
                self.again.insert(repository_id);
                continue;
            }
            let manager = manager.clone();
            let id = repository_id.clone();
            let handle = self
                .tasks
                .spawn(async move { manager.sync_in_background(&id).await });
            self.running.insert(handle.id(), repository_id);
        }
    }

    fn finished(&mut self, manager: &Arc<Manager>, finished: Result<(task::Id, ()), JoinError>) {
        let id = match finished {
            Ok((id, ())) => id,
            Err(error) => {
                log!("checkout sync task failed: {error}");
                error.id()
            }
        };
        let Some(repository_id) = self.running.remove(&id) else {
            return;
        };
        if self.again.remove(&repository_id) {
            self.start(manager, vec![repository_id]);
        }
    }
}

#[cfg(test)]
mod tests;
