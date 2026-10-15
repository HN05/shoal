//! Change a PR's base branch, for stacked workspaces whose base PR merged.
use anyhow::{Context, Result};

use super::{
    ForgeKind, ForgeRepo,
    account::Account,
    api::{HttpMethod, Request},
    link::ItemKind,
};

impl ForgeRepo {
    /// Retargeting follows the user's merge, so it uses the user's login.
    pub(crate) async fn retarget(&self, account: &Account, number: u64, base: &str) -> Result<()> {
        let (method, field) = match self.kind {
            ForgeKind::GitHub | ForgeKind::Forgejo => (HttpMethod::Patch, "base"),
            ForgeKind::GitLab => (HttpMethod::Put, "target_branch"),
        };
        let request = Request::new(
            method,
            self.item_endpoint(ItemKind::Pr, number),
            Some(serde_json::json!({ field: base })),
        );
        self.send(account, &request)
            .await
            .with_context(|| format!("could not retarget PR #{number} to {base}"))?;
        Ok(())
    }
}
