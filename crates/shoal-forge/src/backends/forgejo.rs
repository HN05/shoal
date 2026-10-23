//! Forgejo, and Gitea, which serves the same API: GitHub's REST shapes
//! under `/api/v1`, sent with the token fj saved for the host, else through
//! tea's login, else anonymously.
use super::Backend;
use crate::{ForgeRepo, repo::locator::ItemRoute};

#[derive(Clone, Copy)]
pub(crate) struct Forgejo;

impl Backend for Forgejo {
    fn nests_groups(&self) -> bool {
        false
    }

    fn issue_route(&self) -> ItemRoute {
        ItemRoute::Issue
    }

    fn pull_route(&self) -> ItemRoute {
        ItemRoute::Pulls
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
        "Forgejo API request failed; private repositories need an fj or tea login"
    }
}
