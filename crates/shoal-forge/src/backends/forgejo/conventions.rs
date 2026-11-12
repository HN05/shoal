//! Forgejo's description templates, labels and milestones.
use anyhow::Result;
use serde_json::Value;

use super::{Forgejo, organization};
use crate::{
    ForgeRepo,
    account::Account,
    backends::Conventions,
    conventions::{Lookup, shared_lookups},
    items::link::ItemKind,
};

impl Conventions for Forgejo {
    fn template_lookups(&self, kind: ItemKind) -> Vec<(&'static str, Lookup)> {
        shared_lookups(kind)
    }

    /// Forgejo lists an organization's labels, which its repositories'
    /// items take too, apart from the repository's own.
    async fn labels(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<Value>> {
        let endpoint = repo.repo_endpoint("labels");
        let (mut labels, organization) = tokio::try_join!(
            repo.pages(account, &endpoint),
            organization::labels(repo, account),
        )?;
        labels.extend(organization);
        Ok(labels)
    }

    fn exclusive_label(&self, _name: &str, label: &Value) -> bool {
        label["exclusive"].as_bool().unwrap_or(false)
    }
}
