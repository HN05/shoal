//! What each forge does its own way with merges and reverts of PRs.
use anyhow::{Context, Result};
use shoal_refs::{BranchName, CommitId};

use crate::{
    MERGED_HINT,
    backends::{Backend, rest},
    capability::{self, Supports},
    items::revert::NewRevert,
    transport::account::Account,
    transport::api::Request,
};
use shoal_forge_types::{
    ForgeRepo,
    backends::ForgeKind,
    capability::Capability,
    items::{
        action::{MergeMethod, UpdateMethod},
        item::Current,
    },
    numbers::PullNumber,
};

/// How the forge merges and reverts PRs.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Merging: Backend + Supports {
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

    /// Read into `current` what a merge that names no method needs: the
    /// method the repository merges by, where the forge needs one named.
    /// Elsewhere the request leaves the method out, so the repository's own
    /// setting applies.
    async fn prepare_merge(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _current: &mut Current,
    ) -> Result<()> {
        Ok(())
    }

    /// The requests that merge the open PR `current` by `method`, or by the
    /// repository's default without one, a squash merge with `message` as
    /// its commit message where one is given.
    fn merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: Option<MergeMethod>,
        message: Option<&str>,
        delete_branch: bool,
    ) -> Result<Vec<Request>>;

    /// The requests that have the forge merge the open PR `current` once its
    /// required checks and approvals pass, by `method` and with `message`
    /// as a merge takes them.
    fn auto_merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: Option<MergeMethod>,
        message: Option<&str>,
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
        Err(capability::unsupported(self, Capability::RevertPrs))
    }
}

/// A commit message's subject line and the body after it, for a forge that
/// takes them apart.
pub(crate) fn subject_and_body(message: &str) -> (&str, &str) {
    let message = message.trim();
    let (subject, body) = message.split_once('\n').unwrap_or((message, ""));
    (subject.trim_end(), body.trim())
}
