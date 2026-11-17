//! Issues and PRs the daemon lists for a workspace, a chat or a registered
//! checkout, in its repository or another one, so a scoped caller's search
//! goes out as its agent account.
use anyhow::Result;

use crate::{
    daemon::{scope::Caller, workspace::Manager},
    forge::items::{item::Item, link::ItemKind, list::Search},
    protocol::RepoTarget,
};
use shoal_forge::items::list::ListRequests;

impl Manager {
    /// Issues or PRs that `search` matches in the `target` repository, read
    /// as the agent account for a scoped `caller` and as the user's own
    /// logins for the user.
    pub async fn list_items(
        &self,
        target: &RepoTarget,
        kind: ItemKind,
        search: &Search,
        caller: Option<&Caller>,
    ) -> Result<Vec<Item>> {
        let forge = self.read_forge(target, caller).await?;
        forge.list(&self.read_account(caller), kind, search).await
    }
}

#[cfg(test)]
mod tests;
