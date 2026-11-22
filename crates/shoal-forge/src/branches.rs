//! A repository's branches on its forge: listing them with the open PRs
//! that join them, and checking one at the commit it was read at.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use shoal_refs::{BranchName, CommitId};

use crate::{
    items::create::CreateRequests,
    transport::{account::Account, api::TransportRequests, backend::LoginHint},
};
use shoal_forge_types::{
    ForgeRepo,
    backends::ForgeKind,
    branches::{ForgeBranch, OpenPull},
};

/// How the forge lists a repository's branches.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Branches {
    /// The repository's branches.
    async fn branches(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<ForgeBranch>>;

    /// The path under the repository that lists its open PRs.
    fn open_pulls_path(&self) -> &'static str;

    /// The open PR of `repo` a listing of them returned as `value`.
    fn open_pull(&self, repo: &ForgeRepo, value: &Value) -> Result<OpenPull>;

    /// The open PRs from branches of `repo` into the repository it is a
    /// fork of, which its own listing leaves out; none for a repository that
    /// is no fork.
    async fn outgoing_pulls(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<OpenPull>>;
}

/// Where a forge's listing of branches puts what Shoal reads of each, as
/// JSON pointers; none for what it leaves out.
pub(crate) struct BranchFields {
    pub name: &'static str,
    pub head: &'static str,
    pub committed_at: Option<&'static str>,
}

impl BranchFields {
    /// The branch the listing returned as `value`, `protected` as the
    /// forge says it is.
    pub(crate) fn read(&self, value: &Value, protected: bool) -> Result<ForgeBranch> {
        let text = |pointer| value.pointer(pointer).and_then(Value::as_str);
        let name = text(self.name).context("the forge listed a branch without a name")?;
        let head = (text(self.head))
            .with_context(|| format!("the forge listed branch {name} without its commit"))?;
        Ok(ForgeBranch {
            name: name.into(),
            head: head.into(),
            committed_at: self.committed_at.and_then(text).map(Into::into),
            protected,
        })
    }
}

/// Listing and checking branches.
#[shoal_forge_derive::extension(pub BranchRequests)]
impl ForgeRepo {
    /// The repository's branches.
    pub async fn branches(&self, account: &Account) -> Result<Vec<ForgeBranch>> {
        let kind = self.kind();
        kind.branches(self, account).await.hint_login(&kind)
    }

    /// The repository's open PRs, and those from its branches into the
    /// repository it is a fork of, by the branches they join.
    pub async fn open_pulls(&self, account: &Account) -> Result<Vec<OpenPull>> {
        let kind = self.kind();
        let endpoint = self.repo_endpoint(kind.open_pulls_path());
        let listed = self.pages(account, &endpoint).await.hint_login(&kind)?;
        let mut pulls = (listed.iter())
            .map(|pull| kind.open_pull(self, pull))
            .collect::<Result<Vec<_>>>()?;
        pulls.extend(kind.outgoing_pulls(self, account).await.hint_login(&kind)?);
        Ok(pulls)
    }

    /// Refuse `branch` once it no longer points to `head`, the commit it
    /// was read at.
    pub async fn ensure_branch_at(
        &self,
        account: &Account,
        branch: &BranchName,
        head: &CommitId,
    ) -> Result<()> {
        let tip = self.branch_commit(account, branch).await?;
        ensure!(
            &tip == head,
            "branch {branch} moved from {head} to {tip} since it was read; list the branches again"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests;
