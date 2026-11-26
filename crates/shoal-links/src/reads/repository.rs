//! What a repository's forge offers.
use anyhow::Result;
use shoal_daemon_base::callers::Caller;
use shoal_forge::{
    capability::CapabilityRequests, conventions::labels::LabelRequests,
    items::reaction::allowed::AllowedReactionRequests,
};
use shoal_forge_types::{
    capability::Capabilities, conventions::labels::Label,
    items::reaction::allowed::AllowedReactions,
};
use shoal_protocol::protocol::RepoTarget;

use crate::{Links, daemon::Daemon};

impl Links {
    /// What the forge of the `target` repository supports, asking its
    /// server where the forge's versions differ.
    pub async fn capabilities(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        caller: Option<&Caller>,
    ) -> Result<Capabilities> {
        let forge = self.read_forge(daemon, target, caller).await?;
        forge
            .server_capabilities(&self.forges.read_account(caller))
            .await
    }

    /// The reactions the forge of the `target` repository allows.
    pub async fn reactions(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        caller: Option<&Caller>,
    ) -> Result<AllowedReactions> {
        let forge = self.read_forge(daemon, target, caller).await?;
        forge
            .allowed_reactions(&self.forges.read_account(caller))
            .await
    }

    pub async fn labels(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        caller: Option<&Caller>,
    ) -> Result<Vec<Label>> {
        let forge = self.read_forge(daemon, target, caller).await?;
        forge.labels(&self.forges.read_account(caller)).await
    }
}
