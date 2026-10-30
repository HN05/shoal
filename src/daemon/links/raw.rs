//! Requests `shoal forge api` sends to the forge API of a workspace's
//! repository or another one, so a scoped caller acts as its agent account
//! and only reads, unless the workspace's repository lets agents write there.
use anyhow::{Result, ensure};
use serde_json::Value;

use crate::{
    daemon::workspace::Manager,
    forge::{
        account::Account,
        raw::{HttpMethod, RawRequest},
    },
    model::Workspace,
    protocol::ItemContext,
};

impl Manager {
    /// The forge's response to `request` in the repository of `context`, or
    /// the one `repository` names, sent as the workspace's agent account for
    /// a `scoped` caller and as the user's own logins otherwise. Only
    /// unscoped callers send from a registered checkout.
    pub async fn forge_api(
        &self,
        context: &ItemContext,
        request: &RawRequest,
        repository: Option<&str>,
        scoped: bool,
    ) -> Result<Value> {
        match context {
            ItemContext::Workspace(selector) => {
                self.workspace_forge_api(selector, request, repository, scoped)
                    .await
            }
            ItemContext::Checkout(selector) => {
                let checkout = self.repository(selector).await?;
                let own = self.checkout_forge(&checkout).await?;
                let forge = self.named_forge(own, repository).await?;
                forge.raw(&self.user_account(), request).await
            }
        }
    }

    /// The forge's response to `request` sent for the workspace.
    async fn workspace_forge_api(
        &self,
        selector: &str,
        request: &RawRequest,
        repository: Option<&str>,
        scoped: bool,
    ) -> Result<Value> {
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let own = self.workspace_forge(&workspace).await?;
        let forge = self.named_forge(own.clone(), repository).await?;
        let account = if scoped {
            let elsewhere = forge != own;
            self.api_agent_account(&workspace, request.method, elsewhere)
                .await?
        } else {
            self.user_account()
        };
        forge.raw(&account, request).await
    }

    /// The agent account that may send a `method` request, which is only a
    /// read in a repository `elsewhere` than the workspace's, and in its own
    /// unless `[permissions] api_writes` allows more.
    async fn api_agent_account(
        &self,
        workspace: &Workspace,
        method: HttpMethod,
        elsewhere: bool,
    ) -> Result<Account> {
        // One snapshot, so a reload cannot pair a profile with another map.
        let config = self.config();
        let settings = self.workspace_settings_from(&config, workspace).await?;
        if method != HttpMethod::Get {
            ensure!(
                !elsewhere,
                "workspace processes can only send GET requests to another repository"
            );
            ensure!(
                settings.permissions.api_writes == crate::config::repo::ApiWrites::Direct,
                "workspace processes can only send GET requests; [permissions] api_writes = \"direct\" lets agents send {method}"
            );
        }
        self.agent_account_from(&config, &settings)
    }
}

#[cfg(test)]
mod tests;
