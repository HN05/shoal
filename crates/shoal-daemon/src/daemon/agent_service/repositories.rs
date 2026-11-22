//! The registered repositories, whose paths agents trust.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_agents::daemon::Repositories;
use shoal_ids::repository_ids::RepositoryId;

use crate::{daemon::manager::Manager, model::Repository};

impl Repositories for Manager {
    fn repository<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Repository>> {
        Box::pin(self.repositories.repository(id))
    }
}
