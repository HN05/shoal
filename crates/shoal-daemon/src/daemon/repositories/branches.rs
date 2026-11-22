//! The default branch of a checkout, which its syncs fetch and
//! fast-forward as a refresh of a branch does.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_refs::BranchName;
use shoal_repositories::daemon::Branches;

use crate::{
    daemon::manager::Manager,
    model::{Repository, SyncedRepository},
};

impl Branches for Manager {
    fn fetch_default_remotes<'a>(
        &'a self,
        repo: &'a Repository,
    ) -> BoxFuture<'a, Result<BranchName>> {
        Box::pin(Manager::fetch_default_remotes(self, repo))
    }

    fn fast_forward_default<'a>(
        &'a self,
        repo: &'a Repository,
        default: &'a BranchName,
    ) -> BoxFuture<'a, Result<SyncedRepository>> {
        Box::pin(Manager::fast_forward_default(self, repo, default))
    }
}
