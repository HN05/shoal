//! Diff queries, separate from the daemon's workspace base selection.
use std::path::Path;

use anyhow::Result;
use shoal_protocol::diffs::DiffDisplay;
use shoal_refs::CommitId;
use tokio::process::Command;

use super::run;

pub async fn fork_point(repo: &Path, rev: &str) -> Result<CommitId> {
    Ok(run(repo, &["merge-base", "--fork-point", rev, "HEAD"])
        .await?
        .trim()
        .into())
}

pub async fn merge_base(repo: &Path, rev: &str) -> Result<CommitId> {
    merge_base_of(repo, "HEAD", rev).await
}

/// The merge base of two commits, as a PR's diff starts from.
pub async fn merge_base_of(repo: &Path, head: &str, base: &str) -> Result<CommitId> {
    Ok(run(repo, &["merge-base", base, head]).await?.trim().into())
}

pub async fn numstat(repo: &Path, rev: &str) -> Result<String> {
    run(repo, &["diff", "--numstat", rev, "--"]).await
}

/// `git diff` in the worktree at `repo` since `rev`, as a caller on another
/// machine shows it: under the repository's own Git config, without a
/// pager, coloured and as wide as `display` says. Its caller pipes and
/// reads what it writes.
pub fn shown_elsewhere(repo: &Path, rev: &str, display: DiffDisplay) -> Command {
    let mut command = super::command(repo);
    let color = match display.color {
        true => "--color=always",
        false => "--color=never",
    };
    command.args(["--no-pager", "diff", color, rev, "--"]);
    if let Some(columns) = display.columns {
        command.env("COLUMNS", columns.to_string());
    }
    command
}
