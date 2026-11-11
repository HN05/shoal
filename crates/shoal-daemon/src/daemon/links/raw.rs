//! Requests `shoal forge api` and `shoal forge download` send to the forge of
//! a space's repository or another one, so a scoped caller acts as its
//! agent account and only reads, unless the space's repository lets agents
//! write there.
use anyhow::{Result, ensure};
use serde_json::Value;

use crate::{
    config::permissions::ApiWrites,
    daemon::{links::space::LinkSpace, scope::Caller, workspace::Manager},
    forge::{
        ForgeRepo,
        account::Account,
        download::Download,
        raw::{HttpMethod, RawRequest},
    },
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
        caller: Option<&Caller>,
    ) -> Result<Value> {
        let (forge, account) = self
            .api_forge(context, repository, request.method, caller)
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
        caller: Option<&Caller>,
    ) -> Result<Download> {
        let (forge, account) = self
            .api_forge(context, repository, HttpMethod::Get, caller)
            .await?;
        forge.download(&account, url).await
    }

    /// The repository of `context`, or the one `repository` names, and the
    /// account that sends a `method` request there: the space's agent
    /// account for a scoped `caller` and the user's own logins otherwise.
    /// Only unscoped callers send from a registered checkout.
    async fn api_forge(
        &self,
        context: &ItemContext,
        repository: Option<&str>,
        method: HttpMethod,
        caller: Option<&Caller>,
    ) -> Result<(ForgeRepo, Account)> {
        match context {
            ItemContext::Workspace(_) | ItemContext::Chat(_) | ItemContext::ChatIn { .. } => {
                let space = self.context_space(context).await?;
                self.verify_space(&space).await?;
                let own = self.space_forge(&space).await?;
                let forge = self.named_forge(own.clone(), repository).await?;
                let account = match caller {
                    Some(caller) => {
                        let elsewhere = forge != own;
                        (self.api_agent_account(&space, method, elsewhere, caller)).await?
                    }
                    None => self.user_account(),
                };
                Ok((forge, account))
            }
            ItemContext::Checkout(_) => {
                let checkout = self.context_checkout(context).await?;
                let own = self.checkout_forge(&checkout).await?;
                let forge = self.named_forge(own, repository).await?;
                Ok((forge, self.user_account()))
            }
        }
    }

    /// The agent account that may send `caller`'s `method` request, which
    /// is only a read in a repository `elsewhere` than the space's, and in
    /// its own unless the caller's `api_writes` permission allows more.
    async fn api_agent_account(
        &self,
        space: &LinkSpace,
        method: HttpMethod,
        elsewhere: bool,
        caller: &Caller,
    ) -> Result<Account> {
        // One snapshot, so a reload cannot pair a permission with other tokens.
        let config = self.config();
        if method != HttpMethod::Get {
            ensure!(
                !elsewhere,
                "scoped processes can only send GET requests to another repository"
            );
            let permissions = self
                .caller_permissions_from(&config, caller, space.repository_id())
                .await?;
            ensure!(
                permissions.api_writes.value == ApiWrites::Direct,
                "scoped processes can only send GET requests: {}; \"direct\" lets agents send \
                 {method}",
                permissions.api_writes.cited("api_writes")
            );
        }
        Ok(self.agent_account_from(&config))
    }
}

#[cfg(test)]
mod tests;
