//! Where a review's findings are in the commit it reviewed, which the
//! opening posts of their threads point at.
use std::path::Path;

use shoal_protocol::code::{self, CodeAt, Lines};
use shoal_refs::CommitId;

use crate::{daemon::FindingCode, git, log, reviews::output::NewFinding};

/// Where each of `findings` is in the commit `head` of the repository the
/// worktree `worktree` belongs to, in order: its lines, within its file, and
/// the blob and snippet of its file there, which a file the commit lacks,
/// or one a reference keeps no versions of, goes without.
pub(crate) async fn finding_code(
    worktree: &Path,
    head: &CommitId,
    findings: &[NewFinding],
) -> Vec<FindingCode> {
    let mut found = Vec::new();
    for finding in findings {
        let text = match file_at(worktree, head, &finding.path).await {
            Ok(text) => text,
            Err(error) => {
                log!("{} at {}: {error:#}", finding.path, head.short());
                None
            }
        };
        let lines = match &text {
            Some((_, text)) => within(finding.lines(), text),
            None => finding.lines(),
        };
        found.push(FindingCode {
            commit: head.clone(),
            snippet: text.as_ref().map(|(_, text)| code::snippet(text, lines)),
            at: CodeAt {
                path: finding.path.clone(),
                lines: Some(lines),
                blob: text.map(|(blob, _)| blob),
            },
        });
    }
    found
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
