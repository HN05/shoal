//! The agents' tools, whose configs trust a repository's paths.
use std::path::PathBuf;

use futures_util::future::BoxFuture;
use shoal_agents::trust::TrustChange;
use shoal_repositories::daemon::Agents;

use crate::{daemon::manager::Manager, model::Repository};

impl Agents for Manager {
    fn trust_removed<'a>(
        &'a self,
        repo: &'a Repository,
        paths: Vec<PathBuf>,
    ) -> BoxFuture<'a, bool> {
        let removed = TrustChange::Removed(paths);
        Box::pin(self.agents.update_agent_trust(self, repo, removed))
    }

    fn trust_moved<'a>(
        &'a self,
        repo: &'a Repository,
        moves: Vec<(PathBuf, PathBuf)>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let moved = TrustChange::Moved(moves);
            self.agents.update_agent_trust(self, repo, moved).await;
        })
    }

    fn forget_agent_environments<'a>(&'a self, repo: &'a Repository) -> BoxFuture<'a, ()> {
        Box::pin(self.agents.forget_agent_environments_logged(repo))
    }
}
