//! Diff queries, separate from the daemon's workspace base selection.
use std::path::Path;

use anyhow::Result;

use super::run;

pub async fn fork_point(repo: &Path, reference: &str) -> Result<String> {
    run(repo, &["merge-base", "--fork-point", reference, "HEAD"]).await
}

pub async fn merge_base(repo: &Path, base: &str) -> Result<String> {
    run(repo, &["merge-base", base, "HEAD"]).await
}

/// The merge base of two commits, as a PR's diff starts from.
pub async fn merge_base_of(repo: &Path, head: &str, base: &str) -> Result<String> {
    Ok(run(repo, &["merge-base", base, head])
        .await?
        .trim()
        .to_owned())
}

pub async fn numstat(repo: &Path, base: &str) -> Result<String> {
    run(repo, &["diff", "--numstat", base, "--"]).await
}
