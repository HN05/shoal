//! Classify pasted forge links without making network requests.
use anyhow::{Context, Result, ensure};

use super::{ForgeRepo, IssueInput};

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
}
