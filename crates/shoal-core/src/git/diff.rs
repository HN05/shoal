//! Diff queries, separate from the daemon's workspace base selection.
use std::path::Path;

use anyhow::Result;
use shoal_refs::CommitId;

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
