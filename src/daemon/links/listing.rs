//! Issues and PRs the daemon lists for a workspace or a registered
//! checkout, so a scoped caller's search goes out as its agent account.
use anyhow::Result;

use crate::{
    daemon::workspace::Manager,
    forge::items::{item::Item, link::ItemKind, list::Search},
    protocol::ItemContext,
};

impl Manager {
    /// Issues or PRs of the repository of `context` that `search` matches,
    /// read as the workspace's agent account for a `scoped` caller and as
    /// the user's own logins otherwise. Only unscoped callers list from a
    /// registered checkout.
    pub async fn list_items(
        &self,
        context: &ItemContext,
        kind: ItemKind,
        search: &Search,
        scoped: bool,
    ) -> Result<Vec<Item>> {
        let forge = match context {
            ItemContext::Workspace(selector) => {
                let workspace = self.workspace(selector).await?;
                self.verify_worktree(&workspace).await?;
                if scoped {
                    let (forge, account) = self.workspace_forge_account(&workspace).await?;
                    return forge.list(&account, kind, search).await;
                }
                self.workspace_forge(&workspace).await?
            }
            ItemContext::Checkout(selector) => {
                self.checkout_forge(&self.repository(selector).await?)
                    .await?
            }
        };
        forge.list(&self.user_account(), kind, search).await
    }
}
