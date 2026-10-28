//! Boolean Git queries whose negative answers have documented exit statuses.
use std::path::Path;

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::subprocess;

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
    predicate(command, 1).await
}

/// Whether an exact ref exists, without resolving its target object. Callers
/// pass full ref names, not revision expressions. Unlike `--verify --quiet`,
/// `--exists` distinguishes a missing ref (2) from a broken ref (1).
pub async fn ref_exists(
    repo: &Path,
    reference: &str,
    command: impl FnOnce(&Path) -> Command,
) -> Result<bool> {
    let mut command = command(repo);
    command.args(["show-ref", "--exists", "--", reference]);
    predicate(command, 2)
        .await
        .context("check reference existence; Git 2.43 or newer is required")
}

/// Whether the repository has the commit `commit`.
pub async fn has_commit(repo: &Path, commit: &str) -> Result<bool> {
    let mut command = super::isolated_command(repo);
    command.args([
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("{commit}^{{commit}}"),
    ]);
    predicate(command, 1).await
}

async fn predicate(command: Command, negative: i32) -> Result<bool> {
    let description = format!(
        "{} {:?}",
        command.as_std().get_program().to_string_lossy(),
        command.as_std().get_args().collect::<Vec<_>>()
    );
    let output = super::bounded(command).capture().await;
    match output.as_ref().ok().and_then(|output| output.status.code()) {
        Some(0) => Ok(true),
        Some(code) if code == negative => Ok(false),
        _ => subprocess::checked_output(&description, output).map(|_| true),
    }
}

#[cfg(test)]
mod tests;
