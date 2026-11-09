//! Issue, PR and CI run locator syntax; callers select the route for their
//! forge.
use anyhow::{Context, Result, ensure};

use crate::{ForgeRepo, IssueInput, backends::ForgeKind};

pub(crate) const ISSUE_URL: &str = "expected an issue URL ending in /issues/<number>";
pub(crate) const PULL_URL: &str = "expected a PR URL ending in /pull/<number>, /pulls/<number>, \
    /-/merge_requests/<number> or /pull-requests/<number>";

#[derive(Clone, Copy)]
pub(crate) enum ItemRoute {
    Issue,
    /// GitLab keeps item routes under `/-/`, which no project path can contain.
    GitLabIssue,
    Pull,
    Pulls,
    MergeRequest,
    PullRequests,
}

pub(crate) struct ItemUrl<'a> {
    pub repository: &'a str,
    number: &'a str,
    url: &'a str,
}

impl ItemRoute {
    pub(crate) fn marker(self) -> &'static str {
        match self {
            Self::Issue => "/issues/",
            Self::GitLabIssue => "/-/issues/",
            Self::Pull => "/pull/",
            Self::Pulls => "/pulls/",
            Self::MergeRequest => "/-/merge_requests/",
            Self::PullRequests => "/pull-requests/",
        }
    }

    fn diagnostic(self, issue: &'static str, pull: &'static str) -> &'static str {
        match self {
            Self::Issue | Self::GitLabIssue => issue,
            Self::Pull | Self::Pulls | Self::MergeRequest | Self::PullRequests => pull,
        }
    }

    /// Repository discovery deliberately leaves number validation to lookup.
    pub fn split_url(self, input: &str) -> Option<ItemUrl<'_>> {
        let url = item_path(input);
        let (repository, number) = url.rsplit_once(self.marker())?;
        Some(ItemUrl {
            repository,
            number,
            url,
        })
    }

    fn url(self, input: &str) -> Result<ItemUrl<'_>> {
        self.split_url(input)
            .context(self.diagnostic(ISSUE_URL, "invalid PR URL"))
    }

    pub fn resolve(self, repo: &ForgeRepo, input: &str) -> Result<(u64, String)> {
        let (number, url) = if IssueInput::parse(input) == IssueInput::Url {
            let item = self.url(input)?;
            ensure!(
                ForgeRepo::parse_linked(item.repository, Some(repo.kind))? == *repo,
                self.diagnostic(
                    "issue URL belongs to a different repository",
                    "PR belongs to a different repository",
                )
            );
            (item.number, item.url.to_owned())
        } else {
            (
                input,
                format!(
                    "{}://{}/{}{marker}{input}",
                    repo.web_scheme,
                    repo.host,
                    repo.path,
                    marker = self.marker(),
                ),
            )
        };
        ensure!(
            IssueInput::parse(number) == IssueInput::Number,
            self.diagnostic(
                "issue must be a positive number or an issue URL",
                "invalid PR number",
            )
        );
        let number = number
            .parse::<u64>()
            .context(self.diagnostic("issue number is too large", "PR number is too large"))?;
        ensure!(
            number > 0,
            self.diagnostic("issue number must be positive", "invalid PR number")
        );
        Ok((number, url))
    }
}

/// An item URL without its query, fragment or trailing slash.
pub(crate) fn item_path(input: &str) -> &str {
    input
        .split(['?', '#'])
        .next()
        .unwrap()
        .trim_end_matches('/')
}

/// The repository URL that `url` gives before one of the routes `route_of`
/// gives each forge, and the forge that alone uses that route. Of the
/// routes `url` has, the one that ends last is taken, and of those that end
/// there the longest, as GitLab's `/-/issues/` over `/issues/`.
pub(crate) fn locate(
    url: &str,
    route_of: impl Fn(ForgeKind) -> Option<&'static str>,
) -> Option<(&str, Option<ForgeKind>)> {
    let routes: Vec<_> = ForgeKind::ALL
        .iter()
        .filter_map(|&forge| Some((forge, route_of(forge)?)))
        .collect();
    let (repository, route) = routes
        .iter()
        .filter_map(|&(_, route)| Some((url.rsplit_once(route)?.0, route)))
        .max_by_key(|(repository, route)| (repository.len() + route.len(), route.len()))?;
    let users: Vec<_> = routes
        .iter()
        .filter(|(_, other)| *other == route)
        .map(|&(forge, _)| forge)
        .collect();
    let forge = match users[..] {
        [forge] => Some(forge),
        _ => None,
    };
    Some((repository, forge))
}
