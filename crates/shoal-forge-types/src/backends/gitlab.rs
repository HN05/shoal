use crate::{
    ForgeRepo,
    backends::Routes,
    numbers::RunNumber,
    repo::locator::{ItemRoute, id},
};

#[derive(Clone, Copy)]
pub struct GitLab;

impl Routes for GitLab {
    fn name(&self) -> &'static str {
        "GitLab"
    }

    fn nests_groups(&self) -> bool {
        true
    }

    fn issue_route(&self) -> ItemRoute {
        ItemRoute::GitLabIssue
    }

    fn pull_route(&self) -> ItemRoute {
        ItemRoute::MergeRequest
    }

    fn branch_route(&self) -> &'static str {
        "/-/tree/"
    }

    fn run_route(&self) -> Option<&'static str> {
        Some("/-/pipelines/")
    }

    /// A pipeline's URL holds its API ID.
    fn locate_run(&self, repo: &ForgeRepo, target: &str) -> Option<RunNumber> {
        let parts = repo.repository_path(target, "/-/pipelines/")?;
        let [pipeline] = parts.as_slice() else {
            return None;
        };
        id(pipeline).map(RunNumber::new)
    }
}
