//! Classify pasted forge links without making network requests.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use super::{ForgeRepo, IssueInput};

crate::state::states!(ItemKind {
    Issue => "issue",
    Pr => "pr",
});

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Selection {
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
            Some(_) => {
                ensure!(
                    second.is_none(),
                    "use a URL, or pr/issue followed by a number or URL"
                );
                let link = Link::parse(first.as_deref().unwrap())?;
                let kind = match link.target {
                    LinkTarget::Issue => ItemKind::Issue,
                    LinkTarget::Pr => ItemKind::Pr,
                    LinkTarget::Branch(_) => anyhow::bail!("expected an issue or PR link"),
                };
                Self {
                    kind: Some(kind),
                    input: first,
                }
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
pub(crate) fn item(kind: ItemKind, url: &str) -> Result<(ForgeRepo, u64)> {
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

pub(crate) struct Link {
    pub repository: ForgeRepo,
    pub target: LinkTarget,
}

pub(crate) enum LinkTarget {
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
        let (repository, branch) = url
            .split_once("/src/branch/")
            .or_else(|| url.split_once("/tree/"))
            .context("expected an issue, PR or branch URL")?;
        let repository = ForgeRepo::parse(repository)?;
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
mod tests {
    use super::*;

    #[test]
    fn links_preserve_branch_names_and_validate_item_routes() {
        for (url, expected) in [
            (
                "https://github.com/team/repo/tree/feature/api",
                "feature/api",
            ),
            (
                "https://forge.example/team/repo/src/branch/feature%2Fapi#readme",
                "feature/api",
            ),
            (
                "http://forge.example:3000/team/repo/src/branch/topic+name",
                "topic+name",
            ),
        ] {
            let link = Link::parse(url).unwrap();
            assert!(matches!(link.target, LinkTarget::Branch(branch) if branch == expected));
        }
        assert!(matches!(
            Link::parse("https://github.com/team/repo/issues/1")
                .unwrap()
                .target,
            LinkTarget::Issue
        ));
        assert!(matches!(
            Link::parse("https://forge.example/team/repo/pulls/2")
                .unwrap()
                .target,
            LinkTarget::Pr
        ));
        for url in [
            "https://github.com/team/repo/issues/0",
            "https://github.com/team/repo/pulls/1",
            "https://forge.example/team/repo/src/branch/%ZZ",
            "https://github.com/team/repo",
        ] {
            assert!(Link::parse(url).is_err(), "{url}");
        }
    }

    #[test]
    fn selections_require_kinds_for_numbers_and_match_urls() {
        assert!(Selection::parse(Some("505".into()), None).is_err());
        let selection = Selection::parse(Some("pr".into()), Some("505".into())).unwrap();
        assert_eq!(selection.kind, Some(ItemKind::Pr));
        assert_eq!(selection.input.as_deref(), Some("505"));
        assert!(
            Selection::parse(
                Some("pr".into()),
                Some("https://github.com/team/repo/issues/1".into())
            )
            .is_err()
        );
        assert!(
            Selection::parse(Some("issue".into()), None)
                .unwrap()
                .input
                .is_none()
        );
    }
}
