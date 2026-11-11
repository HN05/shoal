//! What `--add-blocker` and `--remove-blocker` name.
use anyhow::{Result, bail};

use crate::{
    forge::{
        IssueInput,
        items::link::{Link, LinkTarget},
    },
    model::Blocker,
};

/// The blocker `input` names: an issue by number or URL, a PR or CI run by
/// URL, and otherwise a space by its ID. Unlike every other space argument,
/// a bare number here is an issue, never a space's number.
pub(super) fn parse(input: &str) -> Result<Blocker> {
    let input = input.to_owned();
    Ok(match IssueInput::parse(&input) {
        IssueInput::Number => Blocker::Issue(input),
        IssueInput::Url => match Link::parse(&input)?.target {
            LinkTarget::Issue => Blocker::Issue(input),
            LinkTarget::Pr => Blocker::Pr(input),
            LinkTarget::Run => Blocker::CiRun(input),
            LinkTarget::Branch(_) => bail!("a blocker is an issue, PR, CI run or space"),
        },
        IssueInput::Invalid => Blocker::Space(input),
    })
}

#[cfg(test)]
mod tests;
