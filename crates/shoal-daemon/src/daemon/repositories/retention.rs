//! The retention each repository's settings give what Shoal keeps about its
//! spaces once it no longer serves them.
use std::time::Duration;

use futures_util::future::BoxFuture;
use shoal_ids::repository_ids::RepositoryId;

use crate::daemon::{log, manager::Manager, retention::Retention};

impl Retention for Manager {
    /// Its settings' retention, or the global setting's for a chat without
    /// a repository or when the repository's cannot be read.
    fn of<'a>(&'a self, repository: Option<&'a RepositoryId>) -> BoxFuture<'a, Duration> {
        Box::pin(async move {
            let settings = self.repositories.space_settings_in(repository).await;
            let settings = settings.or_else(|error| {
                log!("global retention used for a repository: {error:#}");
                self.config().resolve(&Default::default())
            });
            settings.map_or(Duration::ZERO, |settings| {
                settings.cleanup.restore.retention()
            })
        })
    }
}
