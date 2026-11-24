//! Where a review's findings are in the commit it reviewed, which the
//! opening posts of their threads point at.
use std::path::{Component, Path};

use anyhow::{Context as _, Result, bail};
use shoal_protocol::code::{self, CodeAt, Lines, NewCodeRef};
use shoal_refs::CommitId;

use crate::{daemon::FindingCode, git, log};

/// Where each place in `code` is in the commit `head`, checked out at
/// `checkout`, of the repository the worktree `worktree` belongs to, in
/// order; refused for a place outside the checkout.
pub(crate) async fn in_checkout(
    checkout: &Path,
    worktree: &Path,
    head: &CommitId,
    code: &[NewCodeRef],
) -> Result<Vec<FindingCode>> {
    let mut found = Vec::new();
    for place in code {
        let path = within_checkout(checkout, &place.path)?;
        found.push(at_commit(worktree, head, path, place.lines).await);
    }
    Ok(found)
}

/// The path of `path` relative to the checkout `checkout`, with `/`
/// between its parts, as a commit names it; refused outside the checkout.
fn within_checkout(checkout: &Path, path: &Path) -> Result<String> {
    let canonical = |path: &Path| {
        std::fs::canonicalize(path).with_context(|| format!("{} cannot be read", path.display()))
    };
    let (root, place) = (canonical(checkout)?, canonical(path)?);
    let Ok(relative) = place.strip_prefix(&root) else {
        bail!(
            "{} is outside the checkout of the reviewed commit, {}",
            path.display(),
            checkout.display()
        );
    };
    let parts = relative.components().map(|part| match part {
        Component::Normal(part) => part.to_str().context("a path that is not UTF-8"),
        _ => bail!("{} is no path in the checkout", path.display()),
    });
    Ok(parts.collect::<Result<Vec<_>>>()?.join("/"))
}

/// Where `path`'s `lines` are in the commit `head` of the repository the
/// worktree `worktree` belongs to: within its file, and with the blob and
/// snippet of its file there, which a file the commit lacks, or one a
/// reference keeps no versions of, goes without.
async fn at_commit(
    worktree: &Path,
    head: &CommitId,
    path: String,
    lines: Option<Lines>,
) -> FindingCode {
    let text = match file_at(worktree, head, &path).await {
        Ok(text) => text,
        Err(error) => {
            log!("{path} at {}: {error:#}", head.short());
            None
        }
    };
    let lines = match &text {
        Some((_, text)) => lines.map(|lines| within(lines, text)),
        None => lines,
    };
    FindingCode {
        commit: head.clone(),
        snippet: (text.as_ref()).and_then(|(_, text)| Some(code::snippet(text, lines?))),
        at: CodeAt {
            path,
            lines,
            blob: text.map(|(blob, _)| blob),
        },
    }
}

/// The blob of the file `path` in the commit `head` and its text, when a
/// reference keeps versions of it.
async fn file_at(
    worktree: &Path,
    head: &CommitId,
    path: &str,
) -> anyhow::Result<Option<(String, String)>> {
    let Some(blob) = git::changes::blob_in(worktree, head.as_str(), path).await? else {
        return Ok(None);
    };
    let text = git::code_versions::read(worktree, &blob).await?;
    Ok(code::kept_text(text.into_bytes()).map(|text| (blob, text)))
}

/// `lines` cut to the lines `text` has, which a reviewer may overstate.
fn within(lines: Lines, text: &str) -> Lines {
    let count = u32::try_from(text.lines().count()).unwrap_or(u32::MAX);
    let end = lines.end.min(count);
    Lines {
        start: lines.start.min(end.saturating_sub(1)),
        end,
    }
}
