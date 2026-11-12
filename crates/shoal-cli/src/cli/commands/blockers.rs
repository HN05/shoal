//! What `--add-blocker` and `--remove-blocker` name.
use anyhow::{Context as _, Result};

use crate::{
    forge::{
        IssueInput,
        items::{link::LinkKind, reference::ItemInput},
    },
    model::Blocker,
};

/// The blocker `input` names: an issue by number or URL, a PR or CI run by
/// URL, and otherwise a space by its ID. Unlike every other space argument,
/// a bare number here is an issue, never a space's number.
pub(super) fn parse(input: &str) -> Result<Blocker> {
    if IssueInput::parse(input) == IssueInput::Invalid {
        return Ok(Blocker::Space(input.to_owned()));
    }
    let item = input
        .parse::<ItemInput>()
        .context("a blocker is an issue, PR, CI run or space")?;
    Ok(match item {
        ItemInput::Url(url) if url.kind() == LinkKind::Pr => Blocker::Pr(url),
        ItemInput::Url(url) if url.kind() == LinkKind::Ci => Blocker::CiRun(url),
        issue => Blocker::Issue(issue),
    })
}

#[cfg(test)]
mod tests;
