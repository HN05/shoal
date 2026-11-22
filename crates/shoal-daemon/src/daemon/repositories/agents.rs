//! The agents' tools, whose configs trust a repository's paths.
use std::path::PathBuf;

use futures_util::future::BoxFuture;
use shoal_repositories::daemon::Agents;

use crate::{
    daemon::{agent_trust::TrustChange, manager::Manager},
    model::Repository,
};

impl Agents for Manager {
    fn trust_removed<'a>(
        &'a self,
        repo: &'a Repository,
        paths: Vec<PathBuf>,
    ) -> BoxFuture<'a, bool> {
        Box::pin(self.update_agent_trust(repo, TrustChange::Removed(paths)))
    }

    fn trust_moved<'a>(
        &'a self,
        repo: &'a Repository,
        moves: Vec<(PathBuf, PathBuf)>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.update_agent_trust(repo, TrustChange::Moved(moves))
                .await;
        })
    }

    fn forget_agent_environments<'a>(&'a self, repo: &'a Repository) -> BoxFuture<'a, ()> {
        Box::pin(self.forget_agent_environments_logged(repo))
    }
}
