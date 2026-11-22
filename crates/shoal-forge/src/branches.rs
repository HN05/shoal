//! A repository's branches on its forge: listing them with the open PRs
//! that join them, and checking one at the commit it was read at.
use anyhow::{Result, ensure};
use shoal_refs::{BranchName, CommitId};

use crate::{
    backends::{Branches, LoginHint},
    items::create::CreateRequests,
    transport::account::Account,
    transport::api::TransportRequests,
};
use shoal_forge_types::{
    ForgeRepo,
    branches::{ForgeBranch, OpenPull},
};

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
