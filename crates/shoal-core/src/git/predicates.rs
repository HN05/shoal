//! Boolean Git queries whose negative answers have documented exit statuses.
use std::path::Path;

use anyhow::{Context, Result};
use tokio::process::Command;

use super::{
    reader, refs,
    version::{self, Release},
};
use crate::subprocess;

/// The first Git whose `show-ref` takes `--exists`.
const SHOW_REF_EXISTS: Release = (2, 43);

/// Whether `ancestor` is reachable from `descendant`. Invalid commits and
/// command failures are errors, not negative ancestry answers.
pub async fn is_ancestor(
    repo: &Path,
    ancestor: &str,
    descendant: &str,
    command: impl FnOnce(&Path) -> Command,
) -> Result<bool> {
    let mut command = command(repo);
    command.args(["merge-base", "--is-ancestor", ancestor, descendant]);
    predicate(command, 1, None).await
}

/// Whether `one` and `other` share any history, so a merge of them has a
/// base; an orphan branch, such as a site's, shares none.
pub(crate) async fn share_history(repo: &Path, one: &str, other: &str) -> Result<bool> {
    let mut command = super::command(repo);
    command.args(["merge-base", one, other]);
    predicate(command, 1, None).await
}

/// Whether an exact ref exists, without resolving its target object. Callers
/// pass full ref names, not revision expressions.
pub async fn ref_exists(repo: &Path, reference: &str) -> Result<bool> {
    let owned = reference.to_owned();
    let in_process = move |repo: &gix::Repository| refs::exists_in_process(repo, &owned);
    let git = async |repo: &Path| {
        // Unlike `--verify --quiet`, `--exists` tells a missing ref (2)
        // from a broken one (1).
        let mut command = super::command(repo);
        command.args(["show-ref", "--exists", "--", reference]);
        predicate(command, 2, Some(SHOW_REF_EXISTS)).await
    };
    reader::read(repo, in_process, git)
        .await
        .context("check reference existence")
}

/// Whether the repository has the commit `rev` names.
pub async fn has_commit(repo: &Path, rev: &str) -> Result<bool> {
    let in_process =
        |repo: &gix::Repository, rev: &str| Ok(refs::peeled_commit(repo, rev)?.is_some());
    let git = async |repo: &Path| has_commit_by_git(repo, rev).await;
    refs::read_revision(repo, rev, in_process, git).await
}

async fn has_commit_by_git(repo: &Path, rev: &str) -> Result<bool> {
    let mut command = super::isolated_command(repo);
    command.args([
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("{rev}^{{commit}}"),
    ]);
    predicate(command, 1, None).await
}

/// The answer of `command`, which exits 0 for yes and `negative` for no and
/// needs Git `needed`, when given.
async fn predicate(command: Command, negative: i32, needed: Option<Release>) -> Result<bool> {
    let description = format!(
        "{} {:?}",
        command.as_std().get_program().to_string_lossy(),
        command.as_std().get_args().collect::<Vec<_>>()
    );
    let output = super::bounded(command).capture().await;
    match output.as_ref().ok().and_then(|output| output.status.code()) {
        Some(0) => Ok(true),
        Some(code) if code == negative => Ok(false),
        _ => match needed {
            Some(needed) => version::checked_output(&description, output, needed).await,
            None => subprocess::checked_output(&description, output),
        }
        .map(|_| true),
    }
}

#[cfg(test)]
mod tests;
