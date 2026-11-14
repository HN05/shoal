//! The forges Shoal knows and where each keeps its items and runs on the
//! web. Each forge implements [`Routes`] in a module of its own;
//! [`ForgeKind`] names the one a repository uses and forwards each call to
//! it.
pub mod bitbucket;
pub mod forgejo;
pub mod github;
pub mod gitlab;

use crate::{ForgeRepo, numbers::RunNumber, repo::locator::ItemRoute};

shoal_states::states!(
    /// The API a forge host serves.
    ForgeKind: Variants {
        GitHub => "github",
        /// Forgejo, and Gitea, which serves the same API.
        Forgejo => "forgejo",
        GitLab => "gitlab",
        /// Bitbucket Cloud.
        Bitbucket => "bitbucket",
    }
);

impl ForgeKind {
    /// The well-known hosts, where Bitbucket means Bitbucket Cloud.
    pub fn named(host: &str) -> Option<Self> {
        match host {
            "github.com" => Some(Self::GitHub),
            "gitlab.com" => Some(Self::GitLab),
            "bitbucket.org" => Some(Self::Bitbucket),
            _ => None,
        }
    }

    /// What this process learned `host` serves, else the well-known hosts by
    /// name, else `linked`, the forge whose routes a link of the host uses;
    /// failing all three a host whose name has a `gitlab` label counts as
    /// GitLab and any other as Forgejo or Gitea.
    pub fn detect(host: &str, linked: Option<Self>) -> Self {
        crate::hosts::learned(host)
            .or_else(|| Self::named(host))
            .or(linked)
            .unwrap_or_else(|| {
                if host.split(['.', ':']).any(|label| label == "gitlab") {
                    Self::GitLab
                } else {
                    Self::Forgejo
                }
            })
    }
}

/// Run `$body` with `$backend` bound to the forge `$kind` names, as
/// shoal-forge's own dispatch does for its traits.
macro_rules! dispatch {
    ($kind:expr, $backend:ident => $body:expr) => {
        match $kind {
            $crate::backends::ForgeKind::GitHub => {
                let $backend = $crate::backends::github::GitHub;
                $body
            }
            $crate::backends::ForgeKind::Forgejo => {
                let $backend = $crate::backends::forgejo::Forgejo;
                $body
            }
            $crate::backends::ForgeKind::GitLab => {
                let $backend = $crate::backends::gitlab::GitLab;
                $body
            }
            $crate::backends::ForgeKind::Bitbucket => {
                let $backend = $crate::backends::bitbucket::Bitbucket;
                $body
            }
        }
    };
}

/// Where a forge keeps its items and runs on the web: what parsing a link or
/// a remote needs, without a request. Methods that need the repository take
/// it as `repo`.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub trait Routes {
    /// The forge's name in messages.
    fn name(&self) -> &'static str;

    /// Whether a repository path may have more than `owner/repository`.
    fn nests_groups(&self) -> bool;

    fn issue_route(&self) -> ItemRoute;

    fn pull_route(&self) -> ItemRoute;

    /// What precedes a branch's name in the forge's branch URLs.
    fn branch_route(&self) -> &'static str;

    /// Whether `url` uses one of the forge's issue, PR or branch routes.
    fn links_item(&self, url: &str) -> bool {
        [
            self.issue_route().marker(),
            self.pull_route().marker(),
            self.branch_route(),
        ]
        .iter()
        .any(|route| url.contains(route))
    }

    /// What precedes a run's number in the forge's run web URLs, or `None`
    /// where Shoal reads no runs.
    fn run_route(&self) -> Option<&'static str> {
        Some("/actions/runs/")
    }

    /// The number a workflow run's web URL in the repository names, if
    /// `target` links one.
    fn locate_run(&self, repo: &ForgeRepo, target: &str) -> Option<RunNumber>;
}
