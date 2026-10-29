//! The repository's labels as the forge lists them, so agents can follow
//! the scheme the repository describes in its label descriptions.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ForgeRepo, account::Account, backends::Backend, capability::Capability};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    pub name: String,
    pub description: String,
    /// Whether an item takes at most one label of its scope: Forgejo's
    /// exclusive labels and GitLab's scoped `key::value` labels.
    pub exclusive: bool,
}

impl ForgeRepo {
    /// Every label of the repository, in the forge's order; Bitbucket has none.
    pub async fn labels(&self, account: &Account) -> Result<Vec<Label>> {
        if !self.supports(Capability::Labels) {
            return Ok(Vec::new());
        }
        let listed = self.kind.labels(self, account).await?;
        Ok(listed
            .iter()
            .filter_map(|label| self.label(label))
            .collect())
    }

    fn label(&self, label: &Value) -> Option<Label> {
        let name = label["name"].as_str()?.to_owned();
        let exclusive = self.kind.exclusive_label(&name, label);
        Some(Label {
            description: label["description"].as_str().unwrap_or_default().to_owned(),
            name,
            exclusive,
        })
    }
}

#[cfg(test)]
mod tests;
