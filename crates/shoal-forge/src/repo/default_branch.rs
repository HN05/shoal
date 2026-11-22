//! The branch a repository's forge reports as its default.
use anyhow::{Context, Result};

use crate::{
    repo::Repositories,
    transport::{account::Account, api::TransportRequests},
};
use shoal_forge_types::ForgeRepo;

/// Reading the repository's default branch.
#[shoal_forge_derive::extension(pub DefaultBranchRequests)]
impl ForgeRepo {
    /// The repository's default branch.
    pub async fn default_branch(&self, account: &Account) -> Result<String> {
        let value = self.read_repository(account).await?;
        let branch = self.kind().default_branch(&value);
        branch
            .map(Into::into)
            .with_context(|| format!("{} reports no default branch", self.repository_url()))
    }
}
