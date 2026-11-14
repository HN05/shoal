//! Requests `shoal forge api` and `shoal forge download` send to the forge of
//! a space's repository or another one, so a scoped caller acts as its
//! agent account and only reads, unless its `api_writes` permission lets it
//! write to its own repository.
use anyhow::{Result, ensure};
use serde_json::Value;

use crate::{
    config::permissions::PermissionKey,
    daemon::{links::space::LinkSpace, permissions::Action, scope::Caller, workspace::Manager},
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
        let write = (request.method != HttpMethod::Get).then_some(request);
        let (forge, account) = (self.api_forge(context, repository, write, caller)).await?;
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
        let (forge, account) = (self.api_forge(context, repository, None, caller)).await?;
        forge.download(&account, url).await
    }

    /// The repository of `context`, or the one `repository` names, and the
    /// account that sends a request there, `write` when it is not a read:
    /// the space's agent account for a scoped `caller` and the user's own
    /// logins otherwise. Only unscoped callers send from a registered
    /// checkout.
    async fn api_forge(
        &self,
        context: &ItemContext,
        repository: Option<&str>,
        write: Option<&RawRequest>,
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
                        (self.api_agent_account(&space, write, elsewhere, caller)).await?
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

    /// The agent account that may send `caller`'s request, which is only a
    /// read in a repository `elsewhere` than the space's; in its own, a
    /// `write` is the `api_writes` permission's, bound by its approval to
    /// its method, path and body.
    async fn api_agent_account(
        &self,
        space: &LinkSpace,
        write: Option<&RawRequest>,
        elsewhere: bool,
        caller: &Caller,
    ) -> Result<Account> {
        if let Some(write) = write {
            ensure!(
                !elsewhere,
                "scoped processes can only send GET requests to another repository"
            );
            let repository = Some(space.repository_id().clone());
            let action = Action::request(PermissionKey::ApiWrites, write, repository);
            self.authorize_action(Some(caller), action).await?;
        }
        Ok(self.agent_account())
    }
}

#[cfg(test)]
mod tests;
