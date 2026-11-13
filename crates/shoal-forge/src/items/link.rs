//! Classify pasted forge links without making network requests. A link of
//! a host this process has not resolved names its forge by its routes.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    ForgeRepo, IssueInput,
    backends::{Backend, ForgeKind, Runs},
    items::reference::{ItemInput, ItemRef},
    repo::locator::{ISSUE_URL, PULL_URL, item_path, locate},
};

shoal_states::states!(ItemKind {
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

shoal_states::states!(
    /// What a workspace links and a selection names: an issue, a PR or a CI
    /// run. Only issues and PRs are items of the forge's item API.
    LinkKind: Variants {
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

/// The issues, PRs or CI runs a command names: every linked item, those of
/// one kind, or one item.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Selection {
    /// Missing for every kind, or for a number the CLI has yet to classify.
    pub kind: Option<LinkKind>,
    pub input: Option<ItemInput>,
}

/// Whether `input` is an issue, PR or branch URL of any forge, rather than a
/// repository.
pub fn is_item_link(input: &str) -> bool {
    IssueInput::parse(input) == IssueInput::Url
        && ForgeKind::ALL.iter().any(|forge| forge.links_item(input))
}

impl ForgeRepo {
    /// The number and canonical URL of the `kind` item `input`, its number
    /// or URL, names.
    pub(crate) fn item(&self, kind: LinkKind, input: &str) -> Result<(u64, String)> {
        match kind {
            LinkKind::Issue => self.issue(input).map(|(number, url)| (number.get(), url)),
            LinkKind::Pr => self.pull(input).map(|(number, url)| (number.get(), url)),
            LinkKind::Ci => self.run(input).map(|(number, url)| (number.get(), url)),
        }
    }

    /// The URL of the `kind` item numbered `number` in this repository.
    pub(crate) fn numbered_url(&self, kind: LinkKind, number: u64) -> Result<String> {
        Ok(match kind {
            LinkKind::Issue => self.kind.issue_route().url_of(self, number),
            LinkKind::Pr => self.kind.pull_route().url_of(self, number),
            LinkKind::Ci => self.run_url(crate::numbers::RunNumber::new(number))?,
        })
    }

    /// The repository a `kind` item URL belongs to.
    pub(crate) fn from_item_url(kind: LinkKind, url: &str) -> Result<Self> {
        let (repository, forge) = item_repository(kind, url)?;
        Self::parse_linked(repository, forge)
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
    /// An issue, PR or CI run.
    Item(ItemRef),
    Branch(String),
}

impl Link {
    pub fn parse(input: &str) -> Result<Self> {
        ensure!(
            IssueInput::parse(input) == IssueInput::Url,
            "expected an issue, PR, CI run or branch URL"
        );
        if let Ok(item) = input.parse::<ItemRef>() {
            return Ok(Self {
                repository: item.repository().clone(),
                target: LinkTarget::Item(item),
            });
        }
        let (repository, branch) =
            branch_link(item_path(input)).context("expected an issue, PR, CI run or branch URL")?;
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
