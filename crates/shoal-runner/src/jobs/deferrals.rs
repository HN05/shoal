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

/// How long the runner waits for the hub to rank this machine before it
/// claims at once, so a hub that stops answering holds no poll up.
pub const RANK_WAIT: Duration = Duration::from_secs(3);

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
    /// This machine's rank among the hub's machines for a job of
    /// `repository`: 0, which claims at once, where the hub cannot give it
    /// within [`RANK_WAIT`].
    pub async fn job_rank(&self, daemon: &dyn Daemon, repository: &Repository) -> usize {
        let ranked = tokio::time::timeout(RANK_WAIT, daemon.rank_job(repository)).await;
        let error = match ranked {
            Ok(Ok(rank)) => return rank,
            Ok(Err(error)) => format!("{error:#}"),
            Err(_) => format!("the hub did not answer within {RANK_WAIT:?}"),
        };
        let name = repository.display_name();
        log!("runner: claiming the jobs of {name} at once, unranked: {error}");
        0
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
