//! Listing the issues and PRs a search matches.
use anyhow::Result;

use crate::{capability::CapabilityRequests, items::Items, transport::account::Account};
use shoal_forge_types::{
    ForgeRepo,
    items::{item::Item, link::ItemKind, list::Search},
};

/// Listing issues and PRs.
#[shoal_forge_derive::extension(pub ListRequests)]
impl ForgeRepo {
    /// Up to `search.limit` issues or PRs that `search` matches, newest first.
    pub async fn list(
        &self,
        account: &Account,
        kind: ItemKind,
        search: &Search,
    ) -> Result<Vec<Item>> {
        search.check()?;
        self.require_all(search.needs())?;
        let mut items = self.kind().list_items(self, account, kind, search).await?;
        items.retain(|item| search.includes(item.state));
        items.truncate(search.limit);
        Ok(items)
    }
}

#[cfg(test)]
mod tests;
