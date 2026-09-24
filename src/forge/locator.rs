//! Issue and PR locator syntax; callers select the route for their forge.
use anyhow::{Context, Result, ensure};

use super::{ForgeRepo, IssueInput};

#[derive(Clone, Copy)]
pub(super) enum ItemRoute {
    Issue,
    Pull,
    Pulls,
}

pub(super) struct ItemUrl<'a> {
    pub repository: &'a str,
    number: &'a str,
    url: &'a str,
}

impl ItemRoute {
    fn marker(self) -> &'static str {
        match self {
            Self::Issue => "/issues/",
            Self::Pull => "/pull/",
            Self::Pulls => "/pulls/",
        }
    }

    fn diagnostic(self, issue: &'static str, pull: &'static str) -> &'static str {
        match self {
            Self::Issue => issue,
            Self::Pull | Self::Pulls => pull,
        }
    }

    /// Repository discovery deliberately leaves number validation to lookup.
    pub fn split_url(self, input: &str) -> Option<ItemUrl<'_>> {
        let url = input
            .split(['?', '#'])
            .next()
            .unwrap()
            .trim_end_matches('/');
        let (repository, number) = url.rsplit_once(self.marker())?;
        Some(ItemUrl {
            repository,
            number,
            url,
        })
    }

    pub fn repository(self, input: &str) -> Result<ForgeRepo> {
        ForgeRepo::parse(self.url(input)?.repository)
    }

    fn url(self, input: &str) -> Result<ItemUrl<'_>> {
        self.split_url(input).context(self.diagnostic(
            "expected an issue URL ending in /issues/<number>",
            "invalid PR URL",
        ))
    }

    pub fn resolve(self, repo: &ForgeRepo, input: &str) -> Result<(u64, String)> {
        let (number, url) = if IssueInput::parse(input) == IssueInput::Url {
            let item = self.url(input)?;
            ensure!(
                ForgeRepo::parse(item.repository)? == *repo,
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
