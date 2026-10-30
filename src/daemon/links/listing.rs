//! Issues and PRs the daemon lists for a workspace or a registered
//! checkout, in its repository or another one, so a scoped caller's search
//! goes out as its agent account.
use anyhow::Result;

use crate::{
    daemon::workspace::Manager,
    forge::{
        ForgeRepo,
        account::Account,
        items::{item::Item, link::ItemKind, list::Search},
    },
    protocol::ItemContext,
};

impl Manager {
    /// Issues or PRs that `search` matches in the repository of `context`,
    /// or the one `repository` names, read as the workspace's agent account
    /// for a `scoped` caller and as the user's own logins otherwise. Only
    /// unscoped callers list from a registered checkout.
    pub async fn list_items(
        &self,
        context: &ItemContext,
        kind: ItemKind,
        search: &Search,
        repository: Option<&str>,
        scoped: bool,
    ) -> Result<Vec<Item>> {
        let (own, account) = self.listing_forge_account(context, scoped).await?;
        let forge = self.named_forge(own, repository).await?;
        forge.list(&account, kind, search).await
    }

    /// The repository of `context` and the account a listing reads it as.
    async fn listing_forge_account(
        &self,
        context: &ItemContext,
        scoped: bool,
    ) -> Result<(ForgeRepo, Account)> {
        match context {
            ItemContext::Workspace(selector) => {
                let workspace = self.workspace(selector).await?;
                self.verify_worktree(&workspace).await?;
                if scoped {
                    return self.workspace_forge_account(&workspace).await;
                }
                Ok((self.workspace_forge(&workspace).await?, self.user_account()))
            }
            ItemContext::Checkout(selector) => {
                let checkout = self.repository(selector).await?;
                Ok((self.checkout_forge(&checkout).await?, self.user_account()))
            }
        }
    }
}
