//! What differs between forges. Each forge implements [`Backend`];
//! [`ForgeKind`] names the one a repository uses and forwards each call to
//! it, so a new forge implements the trait and adds its variant here.
pub mod bitbucket;
mod forgejo;
mod github;
pub(crate) mod gitlab;
pub(crate) mod glab;
pub(crate) mod rest;
pub(crate) mod tea;

use crate::{ForgeRepo, repo::locator::ItemRoute};
use bitbucket::Bitbucket;
use forgejo::Forgejo;
use github::GitHub;
use gitlab::GitLab;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ForgeKind {
    GitHub,
    /// Forgejo, and Gitea, which serves the same API.
    Forgejo,
    GitLab,
    /// Bitbucket Cloud.
    Bitbucket,
}

impl ForgeKind {
    /// Well-known hosts by name, where Bitbucket means Bitbucket Cloud; a host glab has a login for, or whose name
    /// has a `gitlab` label, as GitLab; any other as Forgejo or Gitea.
    pub fn detect(host: &str) -> Self {
        match host {
            "github.com" => Self::GitHub,
            "gitlab.com" => Self::GitLab,
            "bitbucket.org" => Self::Bitbucket,
            _ if host.split(['.', ':']).any(|label| label == "gitlab") || glab::has_login(host) => {
                Self::GitLab
            }
            _ => Self::Forgejo,
        }
    }
}

/// What a forge does its own way. Methods that need the repository take it
/// as `repo`.
pub(crate) trait Backend {
    /// Whether a repository path may have more than `owner/repository`.
    fn nests_groups(&self) -> bool;

    fn issue_route(&self) -> ItemRoute;

    fn pull_route(&self) -> ItemRoute;

    /// The API endpoint for `path` under the repository.
    fn repo_endpoint(&self, repo: &ForgeRepo, path: &str) -> String;

    /// The REST collection PRs are under.
    fn pull_resource(&self) -> &'static str;

    /// The field that holds an issue's or PR's number in the repository.
    fn number_field(&self) -> &'static str;

    /// The field that holds a commit's full ID in a list of PR commits.
    fn commit_field(&self) -> &'static str;

    /// What a failed read as the user most likely needs.
    fn login_hint(&self) -> &'static str;
}

/// Run `$body` with `$backend` bound to the backend `$kind` names.
macro_rules! dispatch {
    ($kind:expr, $backend:ident => $body:expr) => {
        match $kind {
            ForgeKind::GitHub => {
                let $backend = GitHub;
                $body
            }
            ForgeKind::Forgejo => {
                let $backend = Forgejo;
                $body
            }
            ForgeKind::GitLab => {
                let $backend = GitLab;
                $body
            }
            ForgeKind::Bitbucket => {
                let $backend = Bitbucket;
                $body
            }
        }
    };
}

impl Backend for ForgeKind {
    fn nests_groups(&self) -> bool {
        dispatch!(self, backend => backend.nests_groups())
    }

    fn issue_route(&self) -> ItemRoute {
        dispatch!(self, backend => backend.issue_route())
    }

    fn pull_route(&self) -> ItemRoute {
        dispatch!(self, backend => backend.pull_route())
    }

    fn repo_endpoint(&self, repo: &ForgeRepo, path: &str) -> String {
        dispatch!(self, backend => backend.repo_endpoint(repo, path))
    }

    fn pull_resource(&self) -> &'static str {
        dispatch!(self, backend => backend.pull_resource())
    }

    fn number_field(&self) -> &'static str {
        dispatch!(self, backend => backend.number_field())
    }

    fn commit_field(&self) -> &'static str {
        dispatch!(self, backend => backend.commit_field())
    }

    fn login_hint(&self) -> &'static str {
        dispatch!(self, backend => backend.login_hint())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_known_and_gitlab_named_hosts_are_recognized() {
        for (host, kind) in [
            ("github.com", ForgeKind::GitHub),
            ("gitlab.com", ForgeKind::GitLab),
            ("bitbucket.org", ForgeKind::Bitbucket),
            ("gitlab.corp.example", ForgeKind::GitLab),
            ("git.corp.example", ForgeKind::Forgejo),
            ("mygitlab.example", ForgeKind::Forgejo),
            ("codeberg.org", ForgeKind::Forgejo),
        ] {
            assert_eq!(ForgeKind::detect(host), kind, "{host}");
        }
    }
}
