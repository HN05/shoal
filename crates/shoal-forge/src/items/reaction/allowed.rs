//! The reactions a repository's forge allows, so a client can offer them
//! before it reacts.
use anyhow::Result;

use crate::{
    capability::CapabilityRequests, items::reaction::Reactions, transport::account::Account,
};
use shoal_forge_types::{
    ForgeRepo, capability::Capability, items::reaction::allowed::AllowedReactions,
};

/// Reading which reactions the forge allows.
#[shoal_forge_derive::extension(pub AllowedReactionRequests)]
impl ForgeRepo {
    /// The reactions the forge allows; refused where it has none.
    pub async fn allowed_reactions(&self, account: &Account) -> Result<AllowedReactions> {
        self.require(Capability::Reactions)?;
        self.kind().allowed_reactions(self, account).await
    }
}

#[cfg(test)]
mod tests;
