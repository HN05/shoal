//! What each forge does its own way with merges and reverts of PRs, and the
//! branches they delete.
use anyhow::{Context, Result};
use shoal_refs::{BranchName, CommitId};

use crate::{
    ForgeRepo, MERGED_HINT,
    account::Account,
    backends::{Backend, ForgeKind, rest},
    capability::Capability,
    items::{
        action::{MergeMethod, UpdateMethod},
        item::Current,
        revert::NewRevert,
    },
    numbers::PullNumber,
    transport::api::Request,
};

/// How the forge merges and reverts PRs and deletes their branches.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Merging: Backend {
    /// Fail closed on changed output: only commits the merged PR lists can
    /// authorize cleanup, including squash and rebase merges. None while the
    /// PR is unmerged.
    async fn merged_commits(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
    ) -> Result<Option<Vec<CommitId>>> {
        rest::merged_commits(repo, account, number)
            .await
            .context(MERGED_HINT.trim_start_matches("; ").to_owned())
    }

    /// How many commits the base branch of PR `number` has that its head
    /// lacks; none once it is closed, or where the forge cannot tell.
    async fn behind(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _number: PullNumber,
    ) -> Result<Option<u64>> {
        Ok(None)
    }

    /// The requests that merge the open PR `current`.
    fn merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<Vec<Request>>;

    /// The requests that have the forge merge the open PR `current` once its
    /// required checks and approvals pass.
    fn auto_merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<Vec<Request>>;

    /// The requests that withdraw the automatic merge of the open PR `current`.
    fn cancel_auto_merge(&self, repo: &ForgeRepo, current: &Current) -> Result<Vec<Request>>;

    /// The requests that bring the branch of the open PR `current` up to
    /// date with its base by `method`.
    fn update_branch(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: UpdateMethod,
    ) -> Result<Vec<Request>>;

    /// Have the forge open `revert` for the merged PR `current` as
    /// `account`, returning the new PR's number and head branch.
    async fn revert(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _current: &Current,
        _revert: &NewRevert,
    ) -> Result<(PullNumber, BranchName)> {
        Err(crate::capability::unsupported(self, Capability::RevertPrs))
    }

    /// The request that deletes `branch` from the repository.
    fn branch_delete(&self, repo: &ForgeRepo, branch: &str) -> Request;
}
