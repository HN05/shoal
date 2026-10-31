//! Requests `shoal forge api` and `shoal forge download` send to the forge of
//! a workspace's repository or another one, so a scoped caller acts as its
//! agent account and only reads, unless the workspace's repository lets
//! agents write there.
use anyhow::{Result, ensure};
use serde_json::Value;

use crate::{
    daemon::workspace::Manager,
    forge::{
        ForgeRepo,
        account::Account,
        download::Download,
        raw::{HttpMethod, RawRequest},
    },
    model::Workspace,
    protocol::ItemContext,
};

impl Manager {
    /// The forge's response to `request` in the repository of `context`, or
    /// the one `repository` names.
    pub async fn forge_api(
        &self,
        context: &ItemContext,
        request: &RawRequest,
        repository: Option<&str>,
        scoped: bool,
    ) -> Result<Value> {
        let (forge, account) = self
            .api_forge(context, repository, request.method, scoped)
            .await?;
        forge.raw(&account, request).await
    }

    /// The file at `url` on the forge of `context`'s repository, or of the
    /// one `repository` names.
    pub async fn forge_download(
        &self,
        context: &ItemContext,
        url: &str,
        repository: Option<&str>,
        scoped: bool,
    ) -> Result<Download> {
        let (forge, account) = self
            .api_forge(context, repository, HttpMethod::Get, scoped)
            .await?;
        forge.download(&account, url).await
    }

    /// The repository of `context`, or the one `repository` names, and the
    /// account that sends a `method` request there: the workspace's agent
    /// account for a `scoped` caller and the user's own logins otherwise.
    /// Only unscoped callers send from a registered checkout.
    async fn api_forge(
        &self,
        context: &ItemContext,
        repository: Option<&str>,
        method: HttpMethod,
        scoped: bool,
    ) -> Result<(ForgeRepo, Account)> {
        match context {
            ItemContext::Workspace(selector) => {
                let workspace = self.workspace(selector).await?;
                self.verify_worktree(&workspace).await?;
                let own = self.workspace_forge(&workspace).await?;
                let forge = self.named_forge(own.clone(), repository).await?;
                let account = if scoped {
                    let elsewhere = forge != own;
                    self.api_agent_account(&workspace, method, elsewhere)
                        .await?
                } else {
                    self.user_account()
                };
                Ok((forge, account))
            }
            ItemContext::Checkout(selector) => {
                let checkout = self.repository(selector).await?;
                let own = self.checkout_forge(&checkout).await?;
                let forge = self.named_forge(own, repository).await?;
                Ok((forge, self.user_account()))
            }
        }
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
