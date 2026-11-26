//! How long Shoal keeps what it records about a repository's spaces once
//! it no longer serves them: the repository's
//! `cleanup.restore.retention_days`.
use shoal_ids::repository_ids::RepositoryId;
use std::{collections::HashMap, time::Duration};

use futures_util::future::BoxFuture;

/// The retention of each repository, which the daemon reads from its
/// settings.
pub trait Retention: Send + Sync {
    /// The retention of what Shoal keeps about `repository`, or about a
    /// chat without one and Shoal itself.
    fn of<'a>(&'a self, repository: Option<&'a RepositoryId>) -> BoxFuture<'a, Duration>;
}

/// The retention of each repository a pass asks about, read once.
pub struct Periods<'a> {
    retention: &'a dyn Retention,
    read: HashMap<Option<RepositoryId>, Duration>,
}

impl<'a> Periods<'a> {
    pub fn new(retention: &'a dyn Retention) -> Self {
        Self {
            retention,
            read: HashMap::new(),
        }
    }

    /// The retention of `repository`, read on its first request.
    pub async fn of(&mut self, repository: Option<&RepositoryId>) -> Duration {
        if let Some(period) = self.read.get(&repository.cloned()) {
            return *period;
        }
        let period = self.retention.of(repository).await;
        self.read.insert(repository.cloned(), period);
        period
    }
}
