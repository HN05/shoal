//! GitHub's routes, on GitHub.com or an Enterprise Server: where its items,
//! branches and runs are on the web.
use crate::{ForgeRepo, backends::Routes, ci::id, numbers::RunNumber, repo::locator::ItemRoute};

#[derive(Clone, Copy)]
pub struct GitHub;

impl Routes for GitHub {
    fn name(&self) -> &'static str {
        "GitHub"
    }

    fn nests_groups(&self) -> bool {
        false
    }

    fn issue_route(&self) -> ItemRoute {
        ItemRoute::Issue
    }

    fn pull_route(&self) -> ItemRoute {
        ItemRoute::Pull
    }

    fn branch_route(&self) -> &'static str {
        "/tree/"
    }

    /// A run's URL holds its API ID.
    fn locate_run(&self, repo: &ForgeRepo, target: &str) -> Option<RunNumber> {
        id(repo.repository_path(target, "/actions/runs/")?.first()?).map(RunNumber::new)
    }
}
