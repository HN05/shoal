//! The commits a review reads, and their checkout in its run.
use anyhow::{Context, Result};
use shoal_refs::{BranchName, CommitId};

use super::directories::review_runs;
use crate::{
    daemon::Daemon,
    git,
    model::Workspace,
    reviewer::{self, Files},
    reviews::{ReviewId, ReviewTarget},
};

/// The commits a run reviews.
pub(crate) struct Commits {
    pub(super) head: CommitId,
    pub(super) base: CommitId,
}

/// Review `id`'s run directory with its ID and a detached checkout of
/// `head`.
pub(super) async fn prepare_checkout(
    daemon: &dyn Daemon,
    id: ReviewId,
    workspace: &Workspace,
    head: &CommitId,
    files: &Files,
) -> Result<()> {
    let repository = daemon.repository(workspace.repository()).await?;
    review_runs(&repository).create()?;
    std::fs::create_dir_all(&files.directory)
        .with_context(|| format!("create {}", files.directory.display()))?;
    reviewer::write_id(files, id)?;
    git::detached::add(&repository.path, &files.checkout(), head.as_str()).await
}

/// The commits the review covers: the workspace's committed HEAD against
/// `shoal diff`'s base, or the PR's fetched head against its base.
pub(crate) async fn review_commits(
    daemon: &dyn Daemon,
    workspace: &Workspace,
    target: &ReviewTarget,
) -> Result<Commits> {
    match target {
        ReviewTarget::Workspace => {
            let head = daemon.head(workspace).await?;
            let base = daemon.diff_base(&workspace.id).await?;
            Ok(Commits { head, base })
        }
        ReviewTarget::Pr { head, base, .. } => {
            let base_ref = git::remote_ref("origin", base.as_str());
            let head = pull_review_head(workspace, head).await?;
            let base = git::diff::merge_base_of(&workspace.path, &head, &base_ref)
                .await
                .with_context(|| format!("the PR base {base} is not fetched"))?;
            Ok(Commits {
                head: head.into(),
                base,
            })
        }
    }
}

/// The commit a PR review reads: the fetched head branch `head`, or for a
/// workspace at a commit, opened at the head of a PR whose branch origin
/// does not have, as a fork's, the head it was last moved to, whatever was
/// committed on top of it there since.
pub(super) async fn pull_review_head(workspace: &Workspace, head: &BranchName) -> Result<String> {
    if let Some(at) = workspace.at() {
        return Ok(at.commit.clone().into());
    }
    let head_ref = git::remote_ref("origin", head.as_str());
    git::resolve_commit(&workspace.path, &head_ref, git::run)
        .await
        .map(String::from)
        .with_context(|| format!("the PR head {head} is not fetched"))
}
