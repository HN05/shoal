//! Which forge serves a repository, and the details of its API and web URLs
//! that differ between forges.
use super::{glab, locator::ItemRoute};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ForgeKind {
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

    /// Whether a repository path may have more than `owner/repository`.
    pub fn nests_groups(self) -> bool {
        self == Self::GitLab
    }

    pub fn issue_route(self) -> ItemRoute {
        match self {
            Self::GitLab => ItemRoute::GitLabIssue,
            Self::GitHub | Self::Forgejo | Self::Bitbucket => ItemRoute::Issue,
        }
    }

    pub fn pull_route(self) -> ItemRoute {
        match self {
            Self::GitHub => ItemRoute::Pull,
            Self::Forgejo => ItemRoute::Pulls,
            Self::GitLab => ItemRoute::MergeRequest,
            Self::Bitbucket => ItemRoute::PullRequests,
        }
    }

    /// The field that holds an issue's or PR's number in the repository.
    pub fn number_field(self) -> &'static str {
        match self {
            Self::GitLab => "iid",
            Self::Bitbucket => "id",
            Self::GitHub | Self::Forgejo => "number",
        }
    }

    /// The field that holds a commit's full ID in a list of PR commits.
    pub fn commit_field(self) -> &'static str {
        match self {
            Self::GitLab => "id",
            Self::Bitbucket => "hash",
            Self::GitHub | Self::Forgejo => "sha",
        }
    }

    /// What a failed read as the user most likely needs.
    pub fn login_hint(self) -> &'static str {
        match self {
            Self::GitHub => "check `gh auth login` and repository access",
            Self::Forgejo => {
                "Forgejo API request failed; private repositories need an fj or tea login"
            }
            Self::GitLab => "GitLab API request failed; check `glab auth login` for the host",
            Self::Bitbucket => {
                "Bitbucket API request failed; private repositories need [bitbucket] token_command"
            }
        }
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
