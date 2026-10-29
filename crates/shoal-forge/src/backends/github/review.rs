//! GitHub reviews: a PR's diff as the patches of its changed files.
use anyhow::{Context, Result};
use serde_json::Value;

use crate::{
    ForgeRepo,
    account::Account,
    items::item::{Current, text},
    review::diff::{Diff, FileDiff},
};

/// The PR's diff as GitHub lists its files. A file too large for GitHub
/// to show has no patch, so none of its lines takes a comment.
pub(super) async fn diff(repo: &ForgeRepo, account: &Account, current: &Current) -> Result<Diff> {
    let endpoint = repo.repo_endpoint(&format!("pulls/{}/files", current.item.number));
    let files = repo.pages(account, &endpoint).await?;
    files
        .iter()
        .map(changed_file)
        .collect::<Result<_>>()
        .map(Diff::new)
}

/// A file as GitHub lists a PR's changed files.
fn changed_file(file: &Value) -> Result<FileDiff> {
    let path = text(&file["filename"]).context("a changed file has no name")?;
    let old_path = text(&file["previous_filename"]).unwrap_or_else(|| path.clone());
    FileDiff::new(path, old_path, file["patch"].as_str().unwrap_or_default())
}

#[cfg(test)]
mod tests;
