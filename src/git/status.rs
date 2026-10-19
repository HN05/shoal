//! Worktree state and refs a removal inspection compares to decide what to retain.
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

/// The full ref of a local branch, when it exists.
pub async fn local_branch(repo: &Path, name: &str) -> Result<Option<String>> {
    let reference = super::local_ref(name);
    Ok(super::ref_exists(repo, &reference, super::command)
        .await?
        .then_some(reference))
}

/// The tree HEAD records, to compare with other revisions by content.
pub async fn head_tree(repo: &Path) -> Result<String> {
    super::revision(repo, "HEAD^{tree}", run).await
}

/// Whether `reference` retains HEAD, by recording its tree or containing it.
pub async fn retains_head(repo: &Path, reference: &str, head_tree: &str) -> Result<bool> {
    Ok(records_tree(repo, reference, head_tree).await
        || super::is_ancestor(repo, "HEAD", reference, super::command).await?)
}

/// Whether HEAD's upstream records the tree; a missing upstream does not.
pub async fn upstream_records_tree(repo: &Path, head_tree: &str) -> bool {
    records_tree(repo, "@{upstream}", head_tree).await
}

async fn records_tree(repo: &Path, revision: &str, tree: &str) -> bool {
    super::verify_revision(repo, &format!("{revision}^{{tree}}"), run)
        .await
        .is_ok_and(|other| other == tree)
}
