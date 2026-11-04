//! What a repository's forge offers: its capabilities, reactions and labels.
use anyhow::Result;

use crate::{
    daemon::workspace::Manager,
    forge::{
        capability::Capabilities, conventions::labels::Label,
        items::reaction::allowed::AllowedReactions,
    },
    protocol::ItemContext,
};

impl Manager {
    /// What the forge of the repository of `context`, or of the one
    /// `repository` names, supports, asking its server where the forge's
    /// versions differ.
    pub async fn capabilities(
        &self,
        context: &ItemContext,
        repository: Option<&str>,
        scoped: bool,
    ) -> Result<Capabilities> {
        let forge = self.repository_forge(context, repository).await?;
        forge.server_capabilities(&self.read_account(scoped)).await
    }

    /// The reactions the forge of the repository of `context`, or of the
    /// one `repository` names, allows.
    pub async fn reactions(
        &self,
        context: &ItemContext,
        repository: Option<&str>,
        scoped: bool,
    ) -> Result<AllowedReactions> {
        let forge = self.repository_forge(context, repository).await?;
        forge.allowed_reactions(&self.read_account(scoped)).await
    }

    /// The labels of the repository of `context`, or of the one
    /// `repository` names.
    pub async fn labels(
        &self,
        context: &ItemContext,
        repository: Option<&str>,
        scoped: bool,
    ) -> Result<Vec<Label>> {
        let forge = self.repository_forge(context, repository).await?;
        forge.labels(&self.read_account(scoped)).await
    }
}
