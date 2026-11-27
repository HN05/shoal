//! How long this machine holds back its claim of a waiting job that other
//! machines of the hub rank ahead of it for;
//! `docs/design/runner.md#placement-delays-a-claim` says why.
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::Duration,
};

use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::model::Repository;
use tokio::time::Instant;

use super::lock;
use crate::{Runner, daemon::Daemon, log};

/// How long a claim waits for each machine ranked ahead of this one.
pub const STEP: Duration = Duration::from_secs(15);

/// A held-back claim: until when it waits, and how many machines rank
/// ahead of this one.
#[derive(Clone, Copy)]
struct Deferral {
    until: Instant,
    ahead: usize,
}

/// The held-back claims, by repository and job handle. A daemon restart
/// forgets them, so its jobs are claimed at once.
#[derive(Default)]
pub struct Deferrals(Mutex<HashMap<(RepositoryId, String), Deferral>>);

impl Deferrals {
    /// Hold back the claim of `repository`'s job `handle`, first seen now,
    /// by a step for each of the `ahead` machines ranked ahead of this one.
    pub fn defer(&self, repository: &RepositoryId, handle: &str, ahead: usize) {
        if ahead == 0 {
            return;
        }
        let steps = u32::try_from(ahead).unwrap_or(u32::MAX);
        let deferral = Deferral {
            until: Instant::now() + STEP.saturating_mul(steps),
            ahead,
        };
        let key = (repository.clone(), handle.to_owned());
        lock(&self.0).insert(key, deferral);
    }

    /// The machines ranked ahead of this one for `repository`'s job
    /// `handle`, while its claim waits for them.
    pub fn ahead(&self, repository: &RepositoryId, handle: &str) -> Option<usize> {
        let key = (repository.clone(), handle.to_owned());
        let deferral = lock(&self.0).get(&key).copied()?;
        (deferral.until > Instant::now()).then_some(deferral.ahead)
    }

    /// Forget the claims whose wait is over, returning whether there were
    /// any, so admission looks at their jobs again.
    pub fn take_lapsed(&self) -> bool {
        let mut deferrals = lock(&self.0);
        let before = deferrals.len();
        let now = Instant::now();
        deferrals.retain(|_, deferral| deferral.until > now);
        deferrals.len() < before
    }

    /// Forget the held-back claims of `repository`'s jobs that `listed`
    /// leaves out, which the forge no longer lists.
    pub fn keep_listed(&self, repository: &RepositoryId, listed: &HashSet<String>) {
        lock(&self.0).retain(|(of, handle), _| of != repository || listed.contains(handle));
    }

    /// Make each held-back claim `by` older.
    #[cfg(any(test, feature = "test-support"))]
    pub fn age(&self, by: Duration) {
        for deferral in lock(&self.0).values_mut() {
            deferral.until = deferral.until.checked_sub(by).unwrap_or_else(Instant::now);
        }
    }
}

impl Runner {
    /// Hold back the claim of `repository`'s job `handle`, which the runner
    /// sees for the first time, by this machine's rank for it among the
    /// hub's machines. One whose rank the hub cannot give is claimed at
    /// once.
    pub async fn defer_by_rank(&self, daemon: &dyn Daemon, repository: &Repository, handle: &str) {
        let ahead = match daemon.rank_job(repository).await {
            Ok(rank) => rank,
            Err(error) => {
                let name = repository.display_name();
                log!("runner: claiming a job of {name} at once, unranked: {error:#}");
                0
            }
        };
        self.jobs.deferrals.defer(&repository.id, handle, ahead);
    }

    /// Have admission look again at the jobs whose claim no longer waits.
    pub fn admit_lapsed_claims(&self, daemon: &dyn Daemon) {
        if self.jobs.deferrals.take_lapsed() {
            daemon.wake_admission();
        }
    }
}

#[cfg(test)]
mod tests;
