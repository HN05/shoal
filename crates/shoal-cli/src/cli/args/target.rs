//! What `shoal start` and `shoal contribute` start on, named by their
//! positional argument.
use std::{fmt, str::FromStr};

use anyhow::{Result, bail};

use crate::forge::{
    ForgeRepo, IssueInput,
    items::{
        link::{Link, LinkKind, LinkTarget, is_item_link},
        reference::{ItemInput, ItemRef},
    },
};

/// A registered repository or a repository's URL, an issue by number or
/// URL, or a PR or branch by its link.
#[derive(Debug, Clone, PartialEq)]
pub enum StartTarget {
    /// A repository, by name, ID, path or URL.
    Repository(String),
    Issue(ItemInput),
    Pull(ItemRef),
    /// A branch, by its link: the repository and the branch.
    Branch {
        url: String,
        repository: ForgeRepo,
        branch: String,
    },
}

impl StartTarget {
    /// The repository a link names; none for a repository or issue number.
    pub fn linked_repository(&self) -> Option<&ForgeRepo> {
        match self {
            Self::Repository(_) | Self::Issue(ItemInput::Number(_)) => None,
            Self::Issue(ItemInput::Url(item)) | Self::Pull(item) => Some(item.repository()),
            Self::Branch { repository, .. } => Some(repository),
        }
    }

    /// The issue it names, by number or URL.
    pub fn issue(&self) -> Option<&ItemInput> {
        match self {
            Self::Issue(issue) => Some(issue),
            _ => None,
        }
    }
}

impl FromStr for StartTarget {
    type Err = anyhow::Error;

    fn from_str(input: &str) -> Result<Self> {
        let number = input
            .strip_prefix('#')
            .filter(|number| IssueInput::parse(number) == IssueInput::Number);
        match IssueInput::parse(number.unwrap_or(input)) {
            IssueInput::Number => Ok(Self::Issue(input.parse()?)),
            IssueInput::Url => link(input),
            IssueInput::Invalid => Ok(Self::Repository(input.to_owned())),
        }
    }
}

/// The target the URL `input` names: an item or branch by its link, else
/// the repository it is the URL of.
fn link(input: &str) -> Result<StartTarget> {
    let link = match Link::parse(input) {
        Ok(link) => link,
        // An item's URL says what is wrong with it.
        Err(error) if is_item_link(input) => {
            return Err(input.parse::<ItemRef>().err().unwrap_or(error));
        }
        Err(_) => return Ok(StartTarget::Repository(input.to_owned())),
    };
    Ok(match link.target {
        LinkTarget::Item(item) => match item.kind() {
            LinkKind::Issue => StartTarget::Issue(ItemInput::Url(item)),
            LinkKind::Pr => StartTarget::Pull(item),
            LinkKind::Ci => bail!("a CI run names no work; use an issue, PR or branch link"),
        },
        LinkTarget::Branch(branch) => StartTarget::Branch {
            url: input.to_owned(),
            repository: link.repository,
            branch,
        },
    })
}

impl fmt::Display for StartTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Repository(repository) => f.write_str(repository),
            Self::Issue(issue) => issue.fmt(f),
            Self::Pull(pull) => pull.fmt(f),
            Self::Branch { url, .. } => f.write_str(url),
        }
    }
}

/// clap parser for [`StartTarget`].
pub(in crate::cli) fn start_target(input: &str) -> Result<StartTarget, String> {
    input.parse().map_err(|error| format!("{error:#}"))
}

#[cfg(test)]
mod tests;
