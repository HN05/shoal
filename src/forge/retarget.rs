//! Change a PR's base branch, for stacked workspaces whose base PR merged.
use anyhow::{Context, Result};

use super::{
    ForgeRepo,
    account::Account,
    api::{HttpMethod, Request},
};

impl ForgeRepo {
    /// Retargeting follows the user's merge, so it uses the user's login.
    pub(crate) async fn retarget(&self, number: u64, base: &str) -> Result<()> {
        let request = Request::new(
            HttpMethod::Patch,
            self.repo_endpoint(&format!("pulls/{number}")),
            Some(serde_json::json!({ "base": base })),
        );
        self.send(&Account::user(), &request)
            .await
            .with_context(|| format!("could not retarget PR #{number} to {base}"))?;
        Ok(())
    }
}
