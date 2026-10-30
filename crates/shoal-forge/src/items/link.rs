//! Classify pasted forge links without making network requests. A link of
//! a host this process has not resolved names its forge by its routes.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{ForgeRepo, IssueInput, backends::ForgeKind};

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

/// The repository and number of an issue, PR or CI run URL.
pub fn item(kind: LinkKind, url: &str) -> Result<(ForgeRepo, u64)> {
    let (repository, (number, _)) = match kind {
        LinkKind::Pr => {
            let repository = ForgeRepo::from_pull_url(url)?;
            let item = repository.pull(url)?;
            (repository, item)
        }
        LinkKind::Issue => {
            let repository = ForgeRepo::from_issue_url(url)?;
            let item = repository.issue(url)?;
            (repository, item)
        }
        LinkKind::Ci => {
            let repository = ForgeRepo::from_run_url(url)?;
            let item = repository.run(url)?;
            (repository, item)
        }
    };
    Ok((repository, number))
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
        let url = input
            .split(['?', '#'])
            .next()
            .unwrap()
            .trim_end_matches('/');
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
        let ((repository, branch), forge) = [
            ("/src/branch/", ForgeKind::Forgejo),
            ("/-/tree/", ForgeKind::GitLab),
            ("/tree/", ForgeKind::GitHub),
            ("/branch/", ForgeKind::Bitbucket),
        ]
        .iter()
        .find_map(|&(marker, forge)| Some((url.split_once(marker)?, forge)))
        .context("expected an issue, PR, CI run or branch URL")?;
        let repository = ForgeRepo::parse_linked(repository, Some(forge))?;
        let branch = decode_branch(branch)?;
        ensure!(!branch.is_empty(), "branch URL has no branch");
        Ok(Self {
            repository,
            target: LinkTarget::Branch(branch),
        })
    }
}

fn decode_branch(input: &str) -> Result<String> {
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
