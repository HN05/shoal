//! A repository's branches on its forge: listing them with the open PRs
//! that join them, and deleting one checked at the commit it was read at.
use anyhow::{Result, ensure};
use shoal_refs::{BranchName, CommitId};

use crate::{
    account::Account,
    backends::{Branches, LoginHint},
    items::create::CreateRequests,
    transport::api::TransportRequests,
};
use shoal_forge_types::{
    ForgeRepo,
    branches::{ForgeBranch, OpenPull},
};

/// Listing and deleting branches.
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

    /// Delete `branch` once it still points to `head`, the commit it was
    /// read at. The forge deletes it by name, so a push between the check
    /// and the deletion goes with it.
    pub async fn delete_branch(
        &self,
        account: &Account,
        branch: &BranchName,
        head: &CommitId,
    ) -> Result<()> {
        self.ensure_branch_at(account, branch, head).await?;
        let request = self.kind().branch_delete(self, branch.as_str());
        self.send(account, &request).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
