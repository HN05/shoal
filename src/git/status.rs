//! Worktree changes and commits a removal inspection must retain.
use std::path::Path;

use anyhow::Result;

use super::run;

pub async fn porcelain(repo: &Path) -> Result<String> {
    run(repo, &["status", "--porcelain", "--untracked-files=normal"]).await
}

/// Quote paths and list every untracked file for a user-facing preview.
pub async fn changed_files(repo: &Path) -> Result<String> {
    run(
        repo,
        &[
            "-c",
            "core.quotePath=true",
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
        ],
    )
    .await
}

/// Commits on HEAD that are reachable from neither a remote nor the given ref.
pub async fn unretained_commits(repo: &Path, retained_ref: Option<&str>) -> Result<u64> {
    let mut args = vec!["rev-list", "--count", "HEAD", "--not", "--remotes"];
    args.extend(retained_ref);
    Ok(run(repo, &args).await?.trim().parse()?)
}
