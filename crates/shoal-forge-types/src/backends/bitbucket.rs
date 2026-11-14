//! Bitbucket Cloud's routes: where its items and branches are on the web.
//! Shoal reads none of its runs.
use crate::{ForgeRepo, backends::Routes, numbers::RunNumber, repo::locator::ItemRoute};

#[derive(Clone, Copy)]
pub struct Bitbucket;

impl Routes for Bitbucket {
    fn name(&self) -> &'static str {
        "Bitbucket Cloud"
    }

    fn nests_groups(&self) -> bool {
        false
    }

    fn issue_route(&self) -> ItemRoute {
        ItemRoute::Issue
    }

    fn pull_route(&self) -> ItemRoute {
        ItemRoute::PullRequests
    }

    fn branch_route(&self) -> &'static str {
        "/branch/"
    }

    fn run_route(&self) -> Option<&'static str> {
        None
    }

    fn locate_run(&self, _repo: &ForgeRepo, _target: &str) -> Option<RunNumber> {
        None
    }
}
