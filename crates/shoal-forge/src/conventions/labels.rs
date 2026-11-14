//! The repository's labels as the forge lists them, so agents can follow
//! the scheme the repository describes in its label descriptions.
use anyhow::Result;
use serde_json::Value;

use crate::{
    account::Account, backends::Conventions, capability::CapabilityRequests, conventions::Label,
};
use shoal_forge_types::{ForgeRepo, capability::Capability};

/// Listing the repository's labels.
#[shoal_forge_derive::extension(pub LabelRequests, pub(crate) LabelSteps)]
impl ForgeRepo {
    /// Every label of the repository, in the forge's order; Bitbucket has none.
    pub async fn labels(&self, account: &Account) -> Result<Vec<Label>> {
        if !self.supports(Capability::Labels) {
            return Ok(Vec::new());
        }
        // Boxed so a forge's further reads, such as a Forgejo organization's
        // labels, do not grow the future of every action that checks labels.
        let listed = Box::pin(self.kind().labels(self, account)).await?;
        Ok(listed
            .iter()
            .filter_map(|label| self.label(label))
            .collect())
    }

    fn label(&self, label: &Value) -> Option<Label> {
        let name = label["name"].as_str()?.to_owned();
        let exclusive = self.kind().exclusive_label(&name, label);
        Some(Label {
            description: label["description"].as_str().unwrap_or_default().to_owned(),
            name,
            exclusive,
        })
    }
}

#[cfg(test)]
mod tests;
