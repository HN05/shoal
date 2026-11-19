//! What a repository's forge offers: its capabilities, reactions and labels.
use anyhow::Result;

use crate::{
    daemon::{manager::Manager, scope::Caller},
    forge::{
        capability::Capabilities, conventions::labels::Label,
        items::reaction::allowed::AllowedReactions,
    },
    protocol::RepoTarget,
};
use shoal_forge::{
    capability::CapabilityRequests, conventions::labels::LabelRequests,
    items::reaction::allowed::AllowedReactionRequests,
};

impl Manager {
    /// What the forge of the `target` repository supports, asking its
    /// server where the forge's versions differ.
    pub async fn capabilities(
        &self,
        target: &RepoTarget,
        caller: Option<&Caller>,
    ) -> Result<Capabilities> {
        let forge = self.read_forge(target, caller).await?;
        forge.server_capabilities(&self.read_account(caller)).await
    }

    /// The reactions the forge of the `target` repository allows.
    pub async fn reactions(
        &self,
        target: &RepoTarget,
        caller: Option<&Caller>,
    ) -> Result<AllowedReactions> {
        let forge = self.read_forge(target, caller).await?;
        forge.allowed_reactions(&self.read_account(caller)).await
    }

    /// The labels of the `target` repository.
    pub async fn labels(&self, target: &RepoTarget, caller: Option<&Caller>) -> Result<Vec<Label>> {
        let forge = self.read_forge(target, caller).await?;
        forge.labels(&self.read_account(caller)).await
    }
}
