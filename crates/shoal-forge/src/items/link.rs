//! Classify pasted forge links without making network requests. A link of
//! a host this process has not resolved names its forge by its routes.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    ForgeRepo, IssueInput,
    backends::{Backend, ForgeKind},
    hosts::HostTypes,
    repo::locator::{ISSUE_URL, PULL_URL, item_path, locate},
};

crate::state::states!(ItemKind {
    Issue => "issue",
    Pr => "pr",
});

impl ItemKind {
    /// The kind's name in a sentence, as in `PR #12` or `issue #3`.
    pub fn noun(self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::Pr => "PR",
        }
    }

    /// The kind's name as a heading or a label of its own.
    pub fn title(self) -> &'static str {
        match self {
            Self::Issue => "Issue",
            Self::Pr => "PR",
        }
    }
}

crate::state::states!(
    /// What a workspace links and a selection names: an issue, a PR or a CI
    /// run. Only issues and PRs are items of the forge's item API.
    LinkKind {
        Issue => "issue",
        Pr => "pr",
        /// A CI workflow run.
        Ci => "ci",
    }
);

impl From<ItemKind> for LinkKind {
    fn from(kind: ItemKind) -> Self {
        match kind {
            ItemKind::Issue => Self::Issue,
            ItemKind::Pr => Self::Pr,
        }
    }
}

impl LinkKind {
    /// The issue or PR kind, or `None` for a run.
    pub fn item(self) -> Option<ItemKind> {
        match self {
            Self::Issue => Some(ItemKind::Issue),
            Self::Pr => Some(ItemKind::Pr),
            Self::Ci => None,
        }
    }

    /// The kind's name in a sentence, as in `PR 12` or `CI run 2765`.
    pub fn noun(self) -> &'static str {
        match self.item() {
            Some(kind) => kind.noun(),
            None => "CI run",
        }
    }

    /// The kind's name as a heading or a label of its own.
    pub fn title(self) -> &'static str {
        match self.item() {
            Some(kind) => kind.title(),
            None => "CI run",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Selection {
    /// Missing for every kind, or for a number the CLI has yet to classify.
    pub kind: Option<LinkKind>,
    pub input: Option<String>,
}

impl Selection {
    pub fn parse(first: Option<String>, second: Option<String>) -> Result<Self> {
        let selection = match first.as_deref() {
            Some("pr") => Self {
                kind: Some(LinkKind::Pr),
                input: second,
            },
            Some("issue") => Self {
                kind: Some(LinkKind::Issue),
                input: second,
            },
            Some("ci") => Self {
                kind: Some(LinkKind::Ci),
                input: second,
            },
            Some(input) => {
                ensure!(
                    second.is_none(),
                    "use a number or URL, or pr/issue/ci followed by one"
                );
                // A bare number never names a run, whose numbers overlap
                // issue and PR numbers.
                let kind = match IssueInput::parse(input) {
                    IssueInput::Number => None,
                    _ => Some(match Link::parse(input)?.target {
                        LinkTarget::Issue => LinkKind::Issue,
                        LinkTarget::Pr => LinkKind::Pr,
                        LinkTarget::Run => LinkKind::Ci,
                        LinkTarget::Branch(_) => {
                            anyhow::bail!("expected an issue, PR or CI run link")
                        }
                    }),
                };
                Self { kind, input: first }
            }
            None => {
                ensure!(second.is_none(), "an item needs a kind or URL");
                Self::default()
            }
        };
        if let Some(input) = &selection.input {
            if IssueInput::parse(input) == IssueInput::Url {
                let link = Link::parse(input)?;
                ensure!(
                    matches!(
                        (&link.target, selection.kind),
                        (LinkTarget::Issue, Some(LinkKind::Issue))
                            | (LinkTarget::Pr, Some(LinkKind::Pr))
                            | (LinkTarget::Run, Some(LinkKind::Ci))
                    ),
                    "link does not match the selected item kind"
                );
            } else {
                ensure!(
                    input.parse::<u64>().is_ok_and(|number| number > 0),
                    "item number must be positive"
                );
            }
        }
        Ok(selection)
    }
}

/// Whether `input` is an issue, PR or branch URL of any forge, rather than a
/// repository.
pub fn is_item_link(input: &str) -> bool {
    IssueInput::parse(input) == IssueInput::Url
        && ForgeKind::ALL.iter().any(|forge| forge.links_item(input))
}

/// Whether the `kind` item URL `url` names the item numbered `number` of
/// the repository `remote` names.
pub fn names_item(kind: LinkKind, url: &str, remote: &str, number: u64) -> bool {
    item(kind, url).is_ok_and(|(repository, item)| item == number && repository.is_remote(remote))
}

/// The repository and number of an issue, PR or CI run URL.
pub fn item(kind: LinkKind, url: &str) -> Result<(ForgeRepo, u64)> {
    let repository = ForgeRepo::from_item_url(kind, url)?;
    let (number, _) = repository.item(kind, url)?;
    Ok((repository, number))
}

impl ForgeRepo {
    /// The number and canonical URL of the `kind` item `input`, its number
    /// or URL, names.
    pub fn item(&self, kind: LinkKind, input: &str) -> Result<(u64, String)> {
        match kind {
            LinkKind::Issue => self.issue(input),
            LinkKind::Pr => self.pull(input),
            LinkKind::Ci => self.run(input).map(|(number, url)| (number.get(), url)),
        }
    }

    /// The repository a `kind` item URL belongs to.
    pub fn from_item_url(kind: LinkKind, url: &str) -> Result<Self> {
        let (repository, forge) = item_repository(kind, url)?;
        Self::parse_linked(repository, forge)
    }

    /// The repository a `kind` item URL belongs to, on the forge `hosts`
    /// says its host serves.
    pub async fn resolve_item_url(
        kind: LinkKind,
        url: &str,
        hosts: &impl HostTypes,
    ) -> Result<Self> {
        Self::resolve(item_repository(kind, url)?.0, hosts).await
    }
}

/// The repository URL of a `kind` item URL, and the forge whose items alone
/// use its route.
fn item_repository(kind: LinkKind, url: &str) -> Result<(&str, Option<ForgeKind>)> {
    match kind {
        LinkKind::Issue => {
            locate(item_path(url), |forge| Some(forge.issue_route().marker())).context(ISSUE_URL)
        }
        LinkKind::Pr => {
            locate(item_path(url), |forge| Some(forge.pull_route().marker())).context(PULL_URL)
        }
        LinkKind::Ci => locate(url, |forge| forge.run_route()).context("expected a CI run URL"),
    }
}

pub struct Link {
    pub repository: ForgeRepo,
    pub target: LinkTarget,
}

pub enum LinkTarget {
    Issue,
    Pr,
    /// A CI workflow run.
    Run,
    Branch(String),
}

impl Link {
    pub fn parse(input: &str) -> Result<Self> {
        ensure!(
            IssueInput::parse(input) == IssueInput::Url,
            "expected an issue, PR, CI run or branch URL"
        );
        let url = item_path(input);
        if let Ok(repository) = ForgeRepo::from_issue_url(url) {
            repository.issue(url)?;
            return Ok(Self {
                repository,
                target: LinkTarget::Issue,
            });
        }
        if let Ok(repository) = ForgeRepo::from_pull_url(url) {
            repository.pull(url)?;
            return Ok(Self {
                repository,
                target: LinkTarget::Pr,
            });
        }
        if let Ok(repository) = ForgeRepo::from_run_url(url) {
            repository.run(url)?;
            return Ok(Self {
                repository,
                target: LinkTarget::Run,
            });
        }
        let (repository, branch) =
            branch_link(url).context("expected an issue, PR, CI run or branch URL")?;
        let branch = decode_branch(branch)?;
        ensure!(!branch.is_empty(), "branch URL has no branch");
        Ok(Self {
            repository,
            target: LinkTarget::Branch(branch),
        })
    }
}

/// The repository and the encoded branch a branch URL names, split at the
/// first forge's branch route that leaves a repository before it. Another
/// forge's route may appear inside a branch name or a group, as `/tree/`
/// does in `/src/branch/fix/tree/x`, so a route that contains another one,
/// as `/-/tree/` contains `/tree/`, is tried before it.
fn branch_link(url: &str) -> Option<(ForgeRepo, &str)> {
    let mut forges = ForgeKind::ALL.to_vec();
    forges.sort_by_key(|forge| std::cmp::Reverse(contained_routes(*forge)));
    forges.into_iter().find_map(|forge| {
        let (repository, branch) = url.split_once(forge.branch_route())?;
        let repository = ForgeRepo::parse_linked(repository, Some(forge)).ok()?;
        Some((repository, branch))
    })
}

/// How many other forges' branch routes `forge`'s contains.
fn contained_routes(forge: ForgeKind) -> usize {
    let route = forge.branch_route();
    ForgeKind::ALL
        .iter()
        .filter(|other| **other != forge && route.contains(other.branch_route()))
        .count()
}

pub(crate) fn decode_branch(input: &str) -> Result<String> {
    let mut bytes = Vec::new();
    let mut input = input.bytes();
    while let Some(byte) = input.next() {
        bytes.push(if byte == b'%' {
            let high = input.next().and_then(|byte| (byte as char).to_digit(16));
            let low = input.next().and_then(|byte| (byte as char).to_digit(16));
            (high.context("invalid branch URL escape")? * 16
                + low.context("invalid branch URL escape")?) as u8
        } else {
            byte
        });
    }
    String::from_utf8(bytes).context("branch URL is not UTF-8")
}

#[cfg(test)]
mod tests;
