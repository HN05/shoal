//! GitHub, through `gh`, which owns the login: its CLI reports items and
//! activity as structured data, and `gh api` sends REST and GraphQL requests.
use super::Backend;
use crate::{ForgeRepo, repo::locator::ItemRoute};

#[derive(Clone, Copy)]
pub(crate) struct GitHub;

impl Backend for GitHub {
    fn nests_groups(&self) -> bool {
        false
    }

    fn issue_route(&self) -> ItemRoute {
        ItemRoute::Issue
    }

    fn pull_route(&self) -> ItemRoute {
        ItemRoute::Pull
    }

    fn repo_endpoint(&self, repo: &ForgeRepo, path: &str) -> String {
        format!("repos/{}/{path}", repo.path)
    }

    fn pull_resource(&self) -> &'static str {
        "pulls"
    }

    fn number_field(&self) -> &'static str {
        "number"
    }

    fn commit_field(&self) -> &'static str {
        "sha"
    }

    fn login_hint(&self) -> &'static str {
        "check `gh auth login` and repository access"
    }
}
