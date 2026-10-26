//! Classify pasted forge links without making network requests. A link of
//! a host this process has not resolved names its forge by its routes.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{ForgeRepo, IssueInput, backends::ForgeKind};

crate::state::states!(ItemKind {
    Issue => "issue",
    Pr => "pr",
});

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Selection {
    /// Missing for every kind, or for a number the CLI has yet to classify.
    pub kind: Option<ItemKind>,
    pub input: Option<String>,
}

impl Selection {
    pub fn parse(first: Option<String>, second: Option<String>) -> Result<Self> {
        let selection = match first.as_deref() {
            Some("pr") => Self {
                kind: Some(ItemKind::Pr),
                input: second,
            },
            Some("issue") => Self {
                kind: Some(ItemKind::Issue),
                input: second,
            },
            Some(input) => {
                ensure!(
                    second.is_none(),
                    "use a number or URL, or pr/issue followed by one"
                );
                let kind = match IssueInput::parse(input) {
                    IssueInput::Number => None,
                    _ => Some(match Link::parse(input)?.target {
                        LinkTarget::Issue => ItemKind::Issue,
                        LinkTarget::Pr => ItemKind::Pr,
                        LinkTarget::Branch(_) => anyhow::bail!("expected an issue or PR link"),
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
                        (LinkTarget::Issue, Some(ItemKind::Issue))
                            | (LinkTarget::Pr, Some(ItemKind::Pr))
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

/// The repository and number of an issue or PR URL.
pub fn item(kind: ItemKind, url: &str) -> Result<(ForgeRepo, u64)> {
    let (repository, (number, _)) = match kind {
        ItemKind::Pr => {
            let repository = ForgeRepo::from_pull_url(url)?;
            let item = repository.pull(url)?;
            (repository, item)
        }
        ItemKind::Issue => {
            let repository = ForgeRepo::from_issue_url(url)?;
            let item = repository.issue(url)?;
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
    Branch(String),
}

impl Link {
    pub fn parse(input: &str) -> Result<Self> {
        ensure!(
            IssueInput::parse(input) == IssueInput::Url,
            "expected an issue, PR or branch URL"
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
        let ((repository, branch), forge) = [
            ("/src/branch/", ForgeKind::Forgejo),
            ("/-/tree/", ForgeKind::GitLab),
            ("/tree/", ForgeKind::GitHub),
            ("/branch/", ForgeKind::Bitbucket),
        ]
        .iter()
        .find_map(|&(marker, forge)| Some((url.split_once(marker)?, forge)))
        .context("expected an issue, PR or branch URL")?;
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
