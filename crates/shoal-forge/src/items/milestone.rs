//! Milestones, which edits name by title and forges take by ID.
use anyhow::{Context, Result};

use crate::{
    backends::Conventions,
    transport::{account::Account, api::TransportRequests},
};
use shoal_forge_types::ForgeRepo;

/// Reading milestones.
#[shoal_forge_derive::extension(pub(crate) MilestoneRequests)]
impl ForgeRepo {
    /// The ID the forge's edits take for the repository's milestone titled
    /// `title`, open or closed.
    pub(crate) async fn milestone_id(&self, account: &Account, title: &str) -> Result<u64> {
        let endpoint = self.kind().milestones_endpoint(self);
        let mut found = None;
        self.pages_while(account, &endpoint, |batch| {
            found = batch
                .iter()
                .filter_map(|listed| self.kind().milestone(listed))
                .find(|(listed, _)| listed == title)
                .map(|(_, id)| id);
            Ok(found.is_none())
        })
        .await?;
        found.with_context(|| format!("{} has no milestone {title}", self.path))
    }
}

#[cfg(test)]
mod tests;
