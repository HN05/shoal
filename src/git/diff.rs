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

pub async fn numstat(repo: &Path, base: &str) -> Result<String> {
    run(repo, &["diff", "--numstat", base, "--"]).await
}
