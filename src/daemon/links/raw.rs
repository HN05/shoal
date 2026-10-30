//! Requests `shoal forge api` sends to a workspace repository's forge API, so a
//! scoped caller acts as its agent account and only reads unless the
//! repository lets agents write.
use anyhow::{Result, ensure};
use serde_json::Value;

use crate::{
    daemon::workspace::Manager,
    forge::{
        account::Account,
        raw::{HttpMethod, RawRequest},
    },
    model::Workspace,
};

impl Manager {
    /// The forge's response to `request` in the workspace's repository, sent
    /// as its agent account for a `scoped` caller and as the user's own
    /// logins otherwise.
    pub async fn forge_api(
        &self,
        selector: &str,
        request: &RawRequest,
        scoped: bool,
    ) -> Result<Value> {
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let forge = self.workspace_forge(&workspace).await?;
        let account = if scoped {
            self.api_agent_account(&workspace, request.method).await?
        } else {
            self.user_account()
        };
        forge.raw(&account, request).await
    }

    /// The agent account that may send a `method` request, which is only a
    /// read unless `[permissions] api_writes` allows more.
    async fn api_agent_account(
        &self,
        workspace: &Workspace,
        method: HttpMethod,
    ) -> Result<Account> {
        // One snapshot, so a reload cannot pair a profile with another map.
        let config = self.config();
        let settings = self.workspace_settings_from(&config, workspace).await?;
        ensure!(
            method == HttpMethod::Get
                || settings.permissions.api_writes == crate::config::repo::ApiWrites::Direct,
            "workspace processes can only send GET requests; [permissions] api_writes = \"direct\" lets agents send {method}"
        );
        self.agent_account_from(&config, &settings)
    }
}

#[cfg(test)]
mod tests;
