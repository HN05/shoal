//! Issues and PRs the daemon lists for a workspace, a chat or a registered
//! checkout, in its repository or another one, so a scoped caller's search
//! goes out as its agent account.
use anyhow::Result;

use crate::{
    daemon::workspace::Manager,
    forge::items::{item::Item, link::ItemKind, list::Search},
    protocol::ItemContext,
};

impl Manager {
    /// Issues or PRs that `search` matches in the repository of `context`,
    /// or the one `repository` names, read as the agent account for a
    /// `scoped` caller and as the user's own logins otherwise. Only
    /// unscoped callers list from a registered checkout.
    pub async fn list_items(
        &self,
        context: &ItemContext,
        kind: ItemKind,
        search: &Search,
        repository: Option<&str>,
        scoped: bool,
    ) -> Result<Vec<Item>> {
        let forge = self.repository_forge(context, repository).await?;
        forge.list(&self.read_account(scoped), kind, search).await
    }
}

#[cfg(test)]
mod tests;
