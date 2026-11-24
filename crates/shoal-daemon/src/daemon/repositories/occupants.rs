//! The subsystems that work in a repository's directories or keep files
//! there, which its removal stops or deletes.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_ids::repository_ids::RepositoryId;
use shoal_repositories::daemon::Occupants;

use crate::{daemon::manager::Manager, model::Repository, shoal_dirs::ShoalDir};

impl Occupants for Manager {
    fn ensure_no_reviews_in<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.reviews.ensure_none_in(self, id))
    }

    fn ensure_no_import_in(&self, imports: ShoalDir) -> BoxFuture<'_, Result<()>> {
        Box::pin(self.spaces.ensure_no_import_in(imports))
    }

    fn stop_reviews<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, ()> {
        Box::pin(self.reviews.stop_in_repository(self, id))
    }

    fn ensure_no_review_runs_left<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.reviews.ensure_no_runs_left(self, id))
    }

    fn ensure_no_gate_runs_in<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.reviews.ensure_no_gate_runs_in(self, id))
    }

    fn delete_runner_of_removed<'a>(&'a self, repo: &'a Repository) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.runner.delete_runner_of_removed(repo))
    }

    fn delete_kept_files<'a>(&'a self, repo: &'a Repository) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.links.delete_proposal_files(repo).await?;
            super::super::imported_sessions::remove_imported_sessions(repo).await?;
            self.spaces.delete_repository_fetched(repo).await?;
            self.spaces.delete_repository_files(repo).await
        })
    }
}
